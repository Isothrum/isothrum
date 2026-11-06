//! Background downloads of the test image and box.
//!
//! Setup starts them as soon as it opens ([`super::machine_selftest_prefetch`]), so by the time a
//! test runs its image or box is usually there. A download in flight is marked busy; a test that
//! needs it waits for that one instead of starting a second.

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use isothrum_runner::ProcessRunner;

use super::docker::IMAGE;
use super::vm::{TestBox, box_provider, pick_box};
use crate::error::Result;
use crate::exec;

static DOCKER_DL: AtomicBool = AtomicBool::new(false);
static VM_DL: AtomicBool = AtomicBool::new(false);
/// The test image is ~2 MB: past this the engine is stuck, not the network slow.
const PULL_LIMIT: Duration = Duration::from_secs(300);
/// Last line of the VM box download, so a test waiting on it can show progress.
static VM_DL_LINE: Mutex<String> = Mutex::new(String::new());

/// Holds a download flag; releases it when dropped (also on error).
struct Busy(&'static AtomicBool);

impl Busy {
    fn try_take(flag: &'static AtomicBool) -> Option<Busy> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).ok().map(|_was_free| Busy(flag))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Makes sure the test image is local. Returns whether it already was (or a background download
/// finished it) rather than being pulled now.
///
/// # Errors
///
/// The pull's own failure (a wedged or unreachable engine).
pub(super) async fn ensure_image(runner: &dyn ProcessRunner) -> Result<bool> {
    let mut waited = false;
    loop {
        if exec::run(runner, "docker", &["image", "inspect", IMAGE], None).await.is_ok() {
            return Ok(true);
        }
        if let Some(_busy) = Busy::try_take(&DOCKER_DL) {
            exec::within("docker pull", PULL_LIMIT, exec::run(runner, "docker", &["pull", "-q", IMAGE], None)).await?;
            return Ok(waited);
        }
        waited = true;
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

/// Makes sure the test box is local. `on_line` sees download progress, including from a background
/// download this call is waiting on. Returns whether it was already there.
///
/// # Errors
///
/// The `vagrant box add` failure.
pub(super) async fn ensure_box(runner: &dyn ProcessRunner, dir: &Path, bx: &TestBox, mut on_line: impl FnMut(String) + Send) -> Result<bool> {
    let mut waited = false;
    loop {
        if pick_box(runner, bx.provider, bx.arm).await == (bx.name, true) {
            return Ok(true);
        }
        if let Some(_busy) = Busy::try_take(&VM_DL) {
            on_line(format!("Downloading {} for {}, once…", bx.name, bx.provider.id()));
            let add = ["box", "add", bx.name, "--provider", box_provider(bx.provider), "--force"];
            exec::stream(runner, "vagrant", &add, Some(dir), |l| {
                if let Ok(mut last) = VM_DL_LINE.lock() {
                    last.clear();
                    last.push_str(l);
                }
                on_line(l.to_owned());
            })
            .await?;
            return Ok(waited);
        }
        waited = true;
        let line = VM_DL_LINE.lock().map(|l| l.clone()).unwrap_or_default();
        on_line(if line.trim().is_empty() { format!("Finishing the background download of {}…", bx.name) } else { line });
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    use isothrum_runner::process::fake::{FakeProcesses, Reply};

    use super::*;
    use crate::providers::Provider;

    #[test]
    fn a_busy_flag_is_taken_once_and_released_on_drop() {
        static FLAG: AtomicBool = AtomicBool::new(false);
        let first = Busy::try_take(&FLAG);
        assert!(first.is_some());
        assert!(Busy::try_take(&FLAG).is_none());
        drop(first);
        assert!(Busy::try_take(&FLAG).is_some());
    }

    #[tokio::test]
    async fn ensure_image_pulls_only_when_the_test_image_is_missing() {
        let present = FakeProcesses::rules(&[("image inspect", Reply::ok("sha256:abc"))]);
        assert!(ensure_image(&present).await.unwrap(), "already local");
        assert!(!present.lines().iter().any(|l| l.contains("pull")), "nothing pulled: {:?}", present.lines());

        let pulled = FakeProcesses::rules(&[("image inspect", Reply::fail(1, "Error: No such image")), ("pull", Reply::ok(""))]);
        assert!(!ensure_image(&pulled).await.unwrap(), "pulled now, not already there");
        assert!(pulled.lines().iter().any(|l| l.contains("pull -q")), "{:?}", pulled.lines());

        let failed = FakeProcesses::rules(&[("image inspect", Reply::fail(1, "Error: No such image")), ("pull", Reply::fail(1, "boom"))]);
        assert!(ensure_image(&failed).await.is_err());
    }

    #[tokio::test]
    async fn ensure_box_adds_a_missing_box_and_forwards_its_progress() {
        let dir = std::env::temp_dir().join(format!("isothrum-dl-box-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = FakeProcesses::rules(&[("box list", Reply::ok("")), ("box add", Reply::ok("Progress: 10%\nProgress: 100%"))]);
        let mut lines = Vec::new();
        let test_box = TestBox { provider: Provider::Virtualbox, name: "bento/ubuntu-24.04", arm: false };
        let was_there = ensure_box(&fake, &dir, &test_box, |l| lines.push(l)).await.unwrap();
        assert!(!was_there, "it had to be downloaded");
        assert!(fake.lines().iter().any(|l| l.contains("box add bento/ubuntu-24.04 --provider virtualbox")), "{:?}", fake.lines());
        assert!(lines.iter().any(|l| l.contains("Downloading bento/ubuntu-24.04 for")), "the notice: {lines:?}");
        assert!(lines.iter().any(|l| l == "Progress: 100%"), "the streamed lines: {lines:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
