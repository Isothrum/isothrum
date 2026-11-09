//! Setup self-tests: prove this machine can actually run labs, not just that the tools exist.
//!
//! - [`Kind::Docker`]: a throwaway two-container lab (`BusyBox`, ~2 MB). It checks the engine
//!   answers, an image pulls, Compose starts the lab, the containers reach each other by name on
//!   the lab network, and a published port is reachable from this machine. Always cleaned up.
//! - [`Kind::Vm`]: a real throwaway VM on the first usable local hypervisor. It checks the
//!   hypervisor answers, gets a small test box (once, then cached), boots it on a private lab
//!   network, runs a command inside, pings it from this machine, then destroys it.
//!
//! Progress goes out as one [`Event`] per step: `running`, then `ok`, `fail` or `skip`.

pub mod docker;
pub mod downloads;
pub mod elevate;
pub mod vm;

use std::future::Future;
use std::path::PathBuf;

use isothrum_runner::ProcessRunner;
use serde::Serialize;

pub use elevate::{BootProbe, ElevatedBoot};

use crate::dirs::Dirs;
use crate::error::Result;
use crate::providers::Provider;
use crate::storage;

/// Which self-test to run.
#[derive(Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// The container self-test.
    Docker,
    /// The VM self-test.
    Vm,
}

/// One step's progress, sent to whoever is running the test.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// The step's short key (`engine`, `boot`, …).
    pub step: &'static str,
    /// The step's label, for the setup screen.
    pub label: &'static str,
    /// `running`, `ok`, `fail` or `skip`.
    pub state: &'static str,
    /// A live or final detail line, when there is one.
    pub detail: Option<String>,
}

/// Longest live output line shown under a running step, in characters.
const PROGRESS_MAX: usize = 160;

/// Where a test's events go: the page that runs it.
struct Reporter(Box<dyn Fn(Event) + Send + Sync>);

impl Reporter {
    /// Live output for a step that is still running (shown as its detail line).
    fn progress(&self, step: &'static str, label: &'static str, line: &str) {
        let line = line.trim();
        if !line.is_empty() {
            self.send(
                step,
                label,
                "running",
                Some(line.chars().take(PROGRESS_MAX).collect()),
            );
        }
    }

    fn send(
        &self,
        step: &'static str,
        label: &'static str,
        state: &'static str,
        detail: Option<String>,
    ) {
        (self.0)(Event {
            step,
            label,
            state,
            detail,
        });
    }

    /// Runs one step, reporting it; a failure stops the test with that step's error.
    async fn step<T>(
        &self,
        step: &'static str,
        label: &'static str,
        fut: impl Future<Output = Result<(T, Option<String>)>>,
    ) -> Result<T> {
        self.send(step, label, "running", None);
        let outcome = fut.await;
        let (state, detail) = match &outcome {
            Ok((_, detail)) => ("ok", detail.clone()),
            Err(e) => ("fail", Some(explain(&e.to_string()))),
        };
        self.send(step, label, state, detail);
        outcome.map(|(v, _)| v)
    }

    /// The closing clean-up step, reported only when the test itself passed (a failed test already
    /// says what went wrong).
    fn cleanup<T>(
        &self,
        label: &'static str,
        test: &Result<()>,
        done: Result<T>,
        ok_detail: Option<String>,
    ) {
        match (test, done) {
            (Ok(()), Ok(_)) => self.send("cleanup", label, "ok", ok_detail),
            (Ok(()), Err(e)) => self.send("cleanup", label, "fail", Some(e.to_string())),
            (Err(_), _) => {}
        }
    }
}

/// A plain-language hint for the failures people actually hit, in front of the raw error (kept as
/// the detail). Unknown errors pass through unchanged.
fn explain(raw: &str) -> String {
    let hint = if raw.contains("type=kvm") || raw.contains("preferred machine") {
        "KVM isn't usable by this user. Check that /dev/kvm exists and that you are in the kvm and libvirt groups, then run the test again."
    } else if raw.contains("Cannot connect to the Docker daemon") || raw.contains("docker.sock") {
        "The container engine isn't reachable. Start Docker and check that your user can use it (the docker group), then run the test again."
    } else {
        return raw.to_owned();
    };
    format!("{hint} Details: {raw}")
}

/// The scratch folder for a test of `kind`, under the cache folder, created if missing.
///
/// # Errors
///
/// [`Error::Invalid`](crate::error::Error::Invalid) when the cache folder is unknown, [`Error::Io`](crate::error::Error::Io) when it can't be created.
pub fn work_dir(dirs: &Dirs, kind: &str) -> Result<PathBuf> {
    let dir = dirs.cache()?.join("selftest").join(kind);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Whether this machine's CPU is ARM (the test boxes differ).
fn arm() -> bool {
    std::env::consts::ARCH == "aarch64"
}

/// Runs `op`, a self-test of `kind`, recording what it downloaded as the app's (the test image, or
/// the test box of whichever provider it used), for storage to clean up.
async fn tracked<T>(
    runner: &dyn ProcessRunner,
    dirs: &Dirs,
    kind: Kind,
    op: impl Future<Output = T>,
) -> T {
    match kind {
        Kind::Docker => storage::tracked(runner, dirs, &[docker::IMAGE.to_owned()], &[], op).await,
        Kind::Vm => {
            let boxes: Vec<String> = vm::TEST_PROVIDERS
                .iter()
                .flat_map(|p| {
                    vm::candidate_boxes(*p, arm())
                        .iter()
                        .map(|b| (*b).to_owned())
                })
                .collect();
            storage::tracked(runner, dirs, &[], &boxes, op).await
        }
    }
}

/// Starts the test downloads early (called when setup opens), recording what they download as the
/// app's. Errors are for the UI to ignore: the test itself retries and reports them.
///
/// # Errors
///
/// The download's own failure, or [`Error::Invalid`](crate::error::Error::Invalid)/[`Error::Io`](crate::error::Error::Io) resolving the work folder.
pub async fn machine_selftest_prefetch(
    runner: &dyn ProcessRunner,
    dirs: &Dirs,
    kind: Kind,
    provider: Option<Provider>,
) -> Result<()> {
    match kind {
        Kind::Docker => storage::tracked(
            runner,
            dirs,
            &[docker::IMAGE.to_owned()],
            &[],
            downloads::ensure_image(runner),
        )
        .await
        .map(|_| ()),
        Kind::Vm => {
            let Some(p) = vm::test_provider(runner, provider).await else {
                return Ok(());
            };
            let (bx, cached) = vm::pick_box(runner, p, arm()).await;
            if cached {
                return Ok(());
            }
            let dir = work_dir(dirs, "vm")?;
            let test_box = vm::TestBox {
                provider: p,
                name: bx,
                arm: arm(),
            };
            storage::tracked(
                runner,
                dirs,
                &[],
                &[bx.to_owned()],
                downloads::ensure_box(runner, &dir, &test_box, |_| {}),
            )
            .await
            .map(|_| ())
        }
    }
}

/// One self-test of a kind at a time in this process: two runs would share the test's Compose
/// project, folder and VM, each tearing the other's down. A second run waits for the first.
fn one_at_a_time(kind: Kind) -> &'static tokio::sync::Mutex<()> {
    static DOCKER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    static VM: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    match kind {
        Kind::Docker => &DOCKER,
        Kind::Vm => &VM,
    }
}

/// The VM self-test's inputs, carried together so the kind-dispatched [`machine_selftest`] keeps
/// within its parameter budget (the container test ignores them).
#[derive(Clone, Copy, Default)]
pub struct Vm<'a> {
    /// The preferred local hypervisor; `None` lets the test pick the first ready one.
    pub provider: Option<Provider>,
    /// The Hyper-V elevation hook (see [`ElevatedBoot`]); `None` is the unelevated two-call path.
    pub elevate: Option<&'a dyn ElevatedBoot>,
}

/// Runs the self-test of `kind` through `runner`, each step's event sent to `events`. `vm` carries
/// the VM test's provider and optional elevation hook; the container test ignores it.
///
/// # Errors
///
/// The error of the step that failed.
pub async fn machine_selftest(
    runner: &dyn ProcessRunner,
    dirs: &Dirs,
    kind: Kind,
    vm: Vm<'_>,
    events: impl Fn(Event) + Send + Sync + 'static,
) -> Result<()> {
    let _alone = one_at_a_time(kind).lock().await;
    let r = Reporter(Box::new(events));
    // Boxed: the steps of a test VM's run make a large future.
    Box::pin(tracked(runner, dirs, kind, async {
        match kind {
            Kind::Docker => docker::run_test(runner, &work_dir(dirs, "docker")?, &r).await,
            Kind::Vm => {
                vm::run_test(runner, &work_dir(dirs, "vm")?, vm.provider, vm.elevate, &r).await
            }
        }
    }))
    .await
}

/// Runs only the container self-test (see [`machine_selftest`]), the way a caller runs just one
/// kind.
///
/// # Errors
///
/// The error of the step that failed.
pub async fn machine_selftest_docker(
    runner: &dyn ProcessRunner,
    dirs: &Dirs,
    events: impl Fn(Event) + Send + Sync + 'static,
) -> Result<()> {
    machine_selftest(runner, dirs, Kind::Docker, Vm::default(), events).await
}

/// Runs only the VM self-test (see [`machine_selftest`]), on `provider` (preferred) with an
/// optional Hyper-V elevation hook (`None`: the unelevated two-call path).
///
/// # Errors
///
/// The error of the step that failed.
pub async fn machine_selftest_vm(
    runner: &dyn ProcessRunner,
    dirs: &Dirs,
    provider: Option<Provider>,
    elevate: Option<&dyn ElevatedBoot>,
    events: impl Fn(Event) + Send + Sync + 'static,
) -> Result<()> {
    machine_selftest(runner, dirs, Kind::Vm, Vm { provider, elevate }, events).await
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use isothrum_runner::process::fake::{FakeProcesses, Reply};

    use super::*;
    use crate::error::Error;

    type Sink = Arc<Mutex<Vec<Event>>>;

    fn events() -> (impl Fn(Event) + Send + Sync + 'static, Sink) {
        let sink: Sink = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&sink);
        (move |e| seen.lock().unwrap().push(e), sink)
    }

    fn reporter() -> (Reporter, Sink) {
        let (send, sink) = events();
        (Reporter(Box::new(send)), sink)
    }

    fn states(seen: &Sink) -> Vec<String> {
        seen.lock()
            .unwrap()
            .iter()
            .map(|e| format!("{}:{}", e.step, e.state))
            .collect()
    }

    fn details(seen: &Sink) -> Vec<Option<String>> {
        seen.lock()
            .unwrap()
            .iter()
            .map(|e| e.detail.clone())
            .collect()
    }

    fn work_dirs() -> Dirs {
        let tag = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let base = std::env::temp_dir().join(format!("isothrum-selftest-{tag}"));
        Dirs::new(Some(base.join("data")), Some(base.join("cache")))
    }

    /// A one-shot loopback HTTP server, for the container test's published-port check.
    async fn serve_once(body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 512];
                // Drain the request so closing the socket can't reset the client's read.
                let read = sock.read(&mut buf).await.unwrap_or(0);
                assert!(read <= buf.len());
                let resp = format!(
                    "HTTP/1.0 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.ok();
            }
        });
        addr
    }

    #[tokio::test]
    async fn a_step_reports_running_then_ok_or_fail() {
        let (r, seen) = reporter();
        assert_eq!(
            r.step("one", "First", async { Ok((7, Some("fine".to_owned()))) })
                .await
                .unwrap(),
            7
        );
        let err = r
            .step::<()>("two", "Second", async {
                Err(Error::Invalid(
                    "Cannot connect to the Docker daemon".to_owned(),
                ))
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Docker daemon"));
        assert_eq!(
            states(&seen),
            ["one:running", "one:ok", "two:running", "two:fail"]
        );
        let detail = details(&seen);
        assert_eq!(detail[1].as_deref(), Some("fine"));
        assert!(
            detail[3]
                .as_deref()
                .unwrap()
                .starts_with("The container engine isn't reachable")
        );
    }

    #[test]
    fn progress_lines_are_trimmed_and_short_and_cleanup_reports_only_after_a_pass() {
        let (r, seen) = reporter();
        r.progress("box", "Box", "   ");
        r.progress("box", "Box", &format!("  {}  ", "x".repeat(500)));
        r.cleanup::<()>("Clean", &Ok(()), Ok(()), Some("kept".to_owned()));
        r.cleanup::<()>(
            "Clean",
            &Ok(()),
            Err(Error::Invalid("left over".to_owned())),
            None,
        );
        r.cleanup::<()>("Clean", &Err(Error::Invalid("x".to_owned())), Ok(()), None);
        assert_eq!(states(&seen), ["box:running", "cleanup:ok", "cleanup:fail"]);
        let detail = details(&seen);
        assert_eq!(detail[0].as_deref().unwrap().chars().count(), PROGRESS_MAX);
        assert_eq!(detail[2].as_deref(), Some("left over"));
        let _unused = arm();
    }

    #[test]
    fn kvm_failure_gets_a_hint_and_keeps_the_details() {
        let raw = "could not get preferred machine for /usr/bin/qemu-system-x86_64 type=kvm";
        let out = explain(raw);
        assert!(out.starts_with("KVM isn't usable"));
        assert!(out.ends_with(raw));
        assert_eq!(explain("something else broke"), "something else broke");
    }

    #[tokio::test]
    async fn the_container_test_runs_every_step_then_cleans_up() {
        let addr = serve_once("cyberctf-selftest-ok").await;
        let fake = FakeProcesses::new(move |call| {
            let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
            match args.as_slice() {
                ["info", ..] => Reply::ok("27.3.1\n"),
                [.., "exec", "-T", "probe", "wget", _, _, _, _] => {
                    Reply::ok("cyberctf-selftest-ok\n")
                }
                [.., "port", "web", "80"] => Reply::ok(&format!("{addr}\n")),
                _ => Reply::ok(""),
            }
        });
        let done = done_states(&fake, Kind::Docker).await;
        assert_eq!(
            done,
            [
                "engine:ok",
                "pull:ok",
                "start:ok",
                "network:ok",
                "port:ok",
                "cleanup:ok"
            ]
        );
    }

    /// Runs the self-test for `kind` to success and returns its non-`running` step states, in
    /// order. Shared by the container and VM success tests.
    async fn done_states(fake: &FakeProcesses, kind: Kind) -> Vec<String> {
        let (send, seen) = events();
        machine_selftest(fake, &work_dirs(), kind, Vm::default(), send)
            .await
            .unwrap();
        states(&seen)
            .into_iter()
            .filter(|s| !s.ends_with(":running"))
            .collect()
    }

    #[tokio::test]
    async fn a_failed_container_test_stops_at_its_step_and_still_cleans_up() {
        let fake = FakeProcesses::rules(&[(
            "docker info",
            Reply::fail(
                1,
                "Cannot connect to the Docker daemon at unix:///var/run/docker.sock",
            ),
        )]);
        let (send, seen) = events();
        assert!(
            machine_selftest(&fake, &work_dirs(), Kind::Docker, Vm::default(), send)
                .await
                .is_err()
        );
        assert_eq!(states(&seen), ["engine:running", "engine:fail"]);
        assert!(
            details(&seen)[1]
                .as_deref()
                .unwrap()
                .starts_with("The container engine isn't reachable")
        );
        assert!(
            fake.lines().iter().any(|l| l.contains("down -v")),
            "the test lab is torn down even so: {:?}",
            fake.lines()
        );
    }

    /// A machine where the VM self-test can run to success: Vagrant and a hypervisor answer, a box
    /// is listed, and `vagrant ssh` prints the probe's marker. Shared by the VM success tests.
    fn vm_fake() -> FakeProcesses {
        let boxes = "generic/alpine319 (virtualbox, 4.3.12)\ngeneric/debian12 (libvirt, 1)\n";
        FakeProcesses::new(move |call| {
            let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
            match (call.program.as_str(), args.as_slice()) {
                ("vagrant", ["plugin", "list"]) => {
                    Reply::ok("vagrant-libvirt (0.12)\nvagrant-qemu (0.3.6)\n")
                }
                ("vagrant", ["box", "list"]) => Reply::ok(boxes),
                ("vagrant", ["--version"]) => Reply::ok("Vagrant 2.4.3\n"),
                ("vagrant", ["ssh", "-c", _]) => Reply::ok("cyberctf-uname:Linux 6.6 x86_64\n"),
                _ => Reply::ok(""),
            }
        })
    }

    #[tokio::test]
    async fn the_vm_test_boots_runs_a_command_and_deletes_the_vm() {
        let fake = vm_fake();
        let done = done_states(&fake, Kind::Vm).await;
        assert_eq!(&done[..2], ["vagrant:ok", "provider:ok"], "{done:?}");
        assert_eq!(
            &done[2..6],
            ["hypervisor:ok", "box:ok", "boot:ok", "exec:ok"]
        );
        assert!(
            done[6] == "network:ok" || done[6] == "network:skip",
            "{done:?}"
        );
        assert_eq!(done.last().map(String::as_str), Some("cleanup:ok"));
        assert!(
            fake.lines()
                .iter()
                .any(|l| l.contains("vagrant up --provider"))
                && fake
                    .lines()
                    .iter()
                    .any(|l| l.contains("vagrant destroy -f"))
        );
    }

    #[tokio::test]
    async fn without_vagrant_the_vm_test_stops_at_once() {
        let fake = FakeProcesses::new(|call| {
            if call.program == "vagrant" {
                Reply::missing()
            } else {
                Reply::ok("")
            }
        });
        let (send, seen) = events();
        assert!(
            machine_selftest(&fake, &work_dirs(), Kind::Vm, Vm::default(), send)
                .await
                .is_err()
        );
        assert_eq!(states(&seen), ["vagrant:running", "vagrant:fail"]);
    }

    #[tokio::test]
    async fn a_second_test_of_a_kind_waits_for_the_first() {
        let fake = FakeProcesses::rules(&[]);
        let first = one_at_a_time(Kind::Docker).lock().await;
        let (send, seen) = events();
        let second = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            machine_selftest(&fake, &work_dirs(), Kind::Docker, Vm::default(), send),
        )
        .await;
        assert!(second.is_err(), "it waits for the first to finish");
        assert!(fake.lines().is_empty() && seen.lock().unwrap().is_empty());
        drop(first);
    }

    #[tokio::test]
    async fn the_docker_only_entry_dispatches_to_the_container_test() {
        // Its first step is `engine` (the container test's), not the VM test's `vagrant`.
        let fake = FakeProcesses::rules(&[(
            "docker info",
            Reply::fail(1, "Cannot connect to the Docker daemon"),
        )]);
        let (send, seen) = events();
        assert!(
            machine_selftest_docker(&fake, &work_dirs(), send)
                .await
                .is_err()
        );
        assert_eq!(seen.lock().unwrap().first().map(|e| e.step), Some("engine"));
    }

    #[tokio::test]
    async fn the_vm_only_entry_dispatches_to_the_vm_test() {
        let fake = vm_fake();
        let (send, seen) = events();
        machine_selftest_vm(&fake, &work_dirs(), None, None, send)
            .await
            .unwrap();
        assert_eq!(
            seen.lock().unwrap().first().map(|e| e.step),
            Some("vagrant")
        );
    }

    /// An elevation hook that runs nothing itself: it records the call and hands back a canned
    /// boot-and-probe, standing in for the launcher's elevated Vagrant batch.
    #[derive(Default)]
    struct FakeElevated {
        called: std::sync::atomic::AtomicBool,
    }

    impl ElevatedBoot for FakeElevated {
        fn boot_and_probe<'a>(
            &'a self,
            _dir: &'a std::path::Path,
            _up: &'a [&'a str],
            _ssh: &'a [&'a str],
            on_line: &'a mut (dyn FnMut(&str) + Send),
        ) -> isothrum_runner::BoxFuture<'a, Result<BootProbe>> {
            self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            on_line("Bringing machine 'default' up with 'hyperv' provider...");
            Box::pin(async {
                Ok(BootProbe::new(
                    Ok(()),
                    Some(Ok("cyberctf-uname:Linux 6.6 aarch64\n".to_owned())),
                ))
            })
        }
    }

    #[tokio::test]
    async fn the_vm_boot_runs_through_the_elevation_hook_when_one_is_given() {
        // With a hook, boot and probe go through it, not the runner (one prompt on Hyper-V). The
        // host has no Hyper-V, so the test proves the wiring on whatever provider is ready.
        let fake = vm_fake();
        let hook = FakeElevated::default();
        let (send, seen) = events();
        let vm = Vm {
            provider: None,
            elevate: Some(&hook),
        };
        machine_selftest(&fake, &work_dirs(), Kind::Vm, vm, send)
            .await
            .unwrap();
        assert!(
            hook.called.load(std::sync::atomic::Ordering::SeqCst),
            "the boot went through the hook"
        );
        let lines = fake.lines();
        assert!(
            !lines.iter().any(|l| l.contains("up --provider"))
                && !lines.iter().any(|l| l.contains("ssh -c")),
            "boot and probe never touched the runner: {lines:?}"
        );
        let done: Vec<String> = states(&seen)
            .into_iter()
            .filter(|s| !s.ends_with(":running"))
            .collect();
        assert!(
            done.contains(&"boot:ok".to_owned()) && done.contains(&"exec:ok".to_owned()),
            "{done:?}"
        );
        assert!(
            details(&seen)
                .iter()
                .flatten()
                .any(|d| d.contains("Linux 6.6 aarch64")),
            "the probe's uname came back through the hook"
        );
    }
}
