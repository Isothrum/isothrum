//! What the app downloaded itself, recorded in `<data>/storage.json`: the container images (each by
//! the reference it pulled and the id that reference named) and the Vagrant boxes (name, provider,
//! version) that appeared during one of its own operations — a lab start, the attack box or VM, a
//! self-test. Storage lists and removes only these: an image or a box the player already had, or
//! pulled for their own projects, is never touched, even when a lab uses the same one.

use std::collections::BTreeSet;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};

use isothrum_runner::ProcessRunner;
use serde::{Deserialize, Serialize};

use crate::dirs::Dirs;
use crate::error::{Error, Result};
use crate::exec;

/// An image the app pulled: the reference it asked for, and the image that reference named then.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OwnedImage {
    /// The image reference the app pulled, e.g. `nginx:1.27`.
    pub reference: String,
    /// The image id that reference named when it was pulled.
    pub id: String,
}

/// A box version the app downloaded, for one provider.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OwnedBox {
    /// The box name, e.g. `generic/debian12`.
    pub name: String,
    /// The provider the box version is for, e.g. `virtualbox`.
    pub provider: String,
    /// The version downloaded.
    pub version: String,
}

/// The record of everything the app downloaded.
#[derive(Serialize, Deserialize, Default, Debug, PartialEq, Eq)]
pub struct Owned {
    /// The images the app pulled.
    #[serde(default)]
    pub images: BTreeSet<OwnedImage>,
    /// The box versions the app downloaded.
    #[serde(default)]
    pub boxes: BTreeSet<OwnedBox>,
}

/// The record's file in `dirs`' data folder.
///
/// # Errors
///
/// [`Error::Invalid`] when the data folder is unknown.
pub fn path(dirs: &Dirs) -> Result<PathBuf> {
    Ok(dirs.data()?.join("storage.json"))
}

/// The record at `store`; empty when there is none yet, or it can't be parsed (nothing is then the
/// app's, which only means less gets cleaned up).
#[must_use]
pub fn load(store: &Path) -> Owned {
    std::fs::read(store)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Changes the record at `store` with `change`, holding an OS lock on `<store>.lock` meanwhile, so
/// a deploy worker and the app can record at once without losing each other's entries.
///
/// # Errors
///
/// [`Error::Io`] when the folder, the lock or the file can't be written, [`Error::Invalid`] when
/// the record can't be serialized.
pub fn update(store: &Path, change: impl FnOnce(&mut Owned)) -> Result<()> {
    if let Some(dir) = store.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(store.with_extension("lock"))?;
    lock.lock()?;
    let mut owned = load(store);
    change(&mut owned);
    let json = serde_json::to_vec_pretty(&owned).map_err(|e| Error::Invalid(e.to_string()))?;
    atomic_write(store, &json)?;
    Ok(())
}

/// Writes `contents` to `path` atomically: to a uniquely named temporary file next to it, flushed,
/// then renamed over it, so a crash mid-write leaves the previous file intact rather than a
/// truncated one.
fn atomic_write(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(
        ".{name}.{}-{:08x}.tmp",
        std::process::id(),
        next_temp_id()
    ));
    if let Err(e) = fill_then_rename(&tmp, path, contents) {
        if std::fs::remove_file(&tmp).is_err() {
            // The stray temporary is harmless; the next write uses a name of its own.
        }
        return Err(e);
    }
    Ok(())
}

/// Fills the temporary file and flushes it to disk, then renames it over `path`.
fn fill_then_rename(tmp: &Path, path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    std::fs::rename(tmp, path)
}

/// A per-process counter making each temporary file's name unique, so two writers never share one.
fn next_temp_id() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// What was on this machine before an operation that may download: the image ids and the boxes,
/// each only when asked for and when the tool could say (`None`: nothing is claimed from it).
pub struct Before {
    images: Option<BTreeSet<String>>,
    boxes: Option<BTreeSet<OwnedBox>>,
}

impl Before {
    /// Reads what is on the machine now, for the dimensions asked for.
    pub async fn take(runner: &dyn ProcessRunner, images: bool, boxes: bool) -> Before {
        let (images, boxes) = tokio::join!(
            async {
                if images {
                    image_ids(runner).await
                } else {
                    None
                }
            },
            async { if boxes { box_list(runner).await } else { None } },
        );
        Before { images, boxes }
    }

    /// Whether images are compared (worth listing the ones an operation used).
    #[must_use]
    pub fn tracks_images(&self) -> bool {
        self.images.is_some()
    }
}

/// Every image id on this machine; `None` when Docker can't say.
pub async fn image_ids(runner: &dyn ProcessRunner) -> Option<BTreeSet<String>> {
    let out = exec::run_read(runner, "docker", &["image", "ls", "-q", "--no-trunc"], None)
        .await
        .ok()?;
    Some(
        out.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// The image `reference` names now.
async fn image_id(runner: &dyn ProcessRunner, reference: &str) -> Option<String> {
    let out = exec::run_read(
        runner,
        "docker",
        &["image", "inspect", "--format", "{{.Id}}", reference],
        None,
    )
    .await
    .ok()?;
    Some(out.trim().to_owned()).filter(|id| !id.is_empty())
}

/// Every box version on this machine; `None` when Vagrant can't say.
pub async fn box_list(runner: &dyn ProcessRunner) -> Option<BTreeSet<OwnedBox>> {
    exec::run_read(runner, "vagrant", &["box", "list"], None)
        .await
        .ok()
        .map(|out| parse_box_list(&out))
}

/// `vagrant box list` lines: `generic/debian12   (virtualbox, 202510.26.0, (amd64))`.
#[must_use]
pub fn parse_box_list(out: &str) -> BTreeSet<OwnedBox> {
    out.lines()
        .filter_map(|line| {
            let (name, rest) = line.trim().split_once(char::is_whitespace)?;
            let inner = rest.trim().strip_prefix('(')?;
            let mut fields = inner.split(',').map(|f| f.trim().trim_end_matches(')'));
            let provider = fields.next().filter(|p| !p.is_empty())?;
            let version = fields.next().filter(|v| !v.is_empty())?;
            Some(OwnedBox {
                name: name.to_owned(),
                provider: provider.to_owned(),
                version: version.to_owned(),
            })
        })
        .collect()
}

/// Records as the app's what appeared since `before` among what an operation used: the images of
/// `images` (references) and the box versions named in `boxes`. Best effort — a failure is logged
/// and only means that download isn't cleaned up later.
pub async fn claim(
    runner: &dyn ProcessRunner,
    store: &Path,
    before: &Before,
    images: &[String],
    boxes: &[String],
) {
    let mut new_images = Vec::new();
    if let Some(had) = &before.images {
        for reference in images {
            if let Some(id) = image_id(runner, reference)
                .await
                .filter(|id| !had.contains(id))
            {
                new_images.push(OwnedImage {
                    reference: reference.clone(),
                    id,
                });
            }
        }
    }
    let new_boxes: Vec<OwnedBox> = match (&before.boxes, boxes.is_empty()) {
        (Some(had), false) => box_list(runner)
            .await
            .map(|now| {
                now.difference(had)
                    .filter(|b| boxes.contains(&b.name))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    if new_images.is_empty() && new_boxes.is_empty() {
        return;
    }
    if let Err(e) = update(store, |owned| {
        owned.images.extend(new_images);
        owned.boxes.extend(new_boxes);
    }) {
        tracing::warn!(
            "couldn't record the app's downloads in {}: {e}",
            store.display()
        );
    }
}

/// Runs `op`, then records what it downloaded of `images` and `boxes` (see [`claim`]), using the
/// record in `dirs`' data folder. For operations whose downloads are named up front.
pub async fn tracked<T>(
    runner: &dyn ProcessRunner,
    dirs: &Dirs,
    images: &[String],
    boxes: &[String],
    op: impl Future<Output = T>,
) -> T {
    let before = Before::take(runner, !images.is_empty(), !boxes.is_empty()).await;
    let out = op.await;
    if let Ok(store) = path(dirs) {
        claim(runner, &store, &before, images, boxes).await;
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use isothrum_runner::process::fake::{FakeProcesses, Reply};

    use super::*;

    fn store() -> PathBuf {
        let tag = format!("{}-{}", std::process::id(), next_temp_id());
        std::env::temp_dir()
            .join(format!("isothrum-owned-{tag}"))
            .join("storage.json")
    }

    #[test]
    fn box_listings_read_as_name_provider_and_version() {
        let out = "generic/debian12   (virtualbox, 202510.26.0, (amd64))\ngeneric/alpine319 (libvirt, 4.3.12)\nThere are no installed boxes!\nodd (\n";
        let boxes: Vec<(String, String, String)> = parse_box_list(out)
            .into_iter()
            .map(|b| (b.name, b.provider, b.version))
            .collect();
        assert_eq!(
            boxes,
            [
                (
                    "generic/alpine319".to_owned(),
                    "libvirt".to_owned(),
                    "4.3.12".to_owned()
                ),
                (
                    "generic/debian12".to_owned(),
                    "virtualbox".to_owned(),
                    "202510.26.0".to_owned()
                ),
            ]
        );
    }

    #[tokio::test]
    async fn only_what_appeared_during_the_operation_is_claimed() {
        let store = store();
        let box_calls = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&box_calls);
        // The player had postgres:16 and generic/debian12 already; the operation pulled nginx and
        // generic/alpine319. `other/box` appears too, but the operation doesn't use it.
        let fake = FakeProcesses::new(move |call| {
            let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
            match args.as_slice() {
                ["image", "ls", ..] | [.., "postgres:16"] => Reply::ok("sha256:postgres\n"),
                [.., "nginx:1.27"] => Reply::ok("sha256:nginx\n"),
                [.., "missing:1"] => Reply::fail(1, "No such image"),
                ["box", "list"] => {
                    if counter.fetch_add(1, Ordering::Relaxed) == 0 {
                        Reply::ok("generic/debian12 (virtualbox, 1)\n")
                    } else {
                        Reply::ok(
                            "generic/debian12 (virtualbox, 1)\ngeneric/alpine319 (virtualbox, 4)\nother/box (virtualbox, 2)\n",
                        )
                    }
                }
                _ => Reply::ok(""),
            }
        });
        let before = Before::take(&fake, true, true).await;
        let images = [
            "nginx:1.27".to_owned(),
            "postgres:16".to_owned(),
            "missing:1".to_owned(),
        ];
        let boxes = [
            "generic/alpine319".to_owned(),
            "generic/debian12".to_owned(),
        ];
        claim(&fake, &store, &before, &images, &boxes).await;
        let owned = load(&store);
        assert_eq!(
            owned.images.into_iter().collect::<Vec<_>>(),
            [OwnedImage {
                reference: "nginx:1.27".to_owned(),
                id: "sha256:nginx".to_owned()
            }]
        );
        assert_eq!(
            owned.boxes.into_iter().collect::<Vec<_>>(),
            [OwnedBox {
                name: "generic/alpine319".to_owned(),
                provider: "virtualbox".to_owned(),
                version: "4".to_owned()
            }]
        );
        std::fs::remove_dir_all(store.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn nothing_is_claimed_when_the_tools_couldnt_say_what_was_there() {
        let store = store();
        let fake = FakeProcesses::new(|call| match call.program.as_str() {
            "docker"
                if call.args.first().map(String::as_str) == Some("image")
                    && call.args.get(1).map(String::as_str) == Some("ls") =>
            {
                Reply::fail(1, "Cannot connect to the Docker daemon")
            }
            "vagrant" => Reply::missing(),
            _ => Reply::ok("sha256:x\n"),
        });
        let before = Before::take(&fake, true, true).await;
        assert!(!before.tracks_images());
        claim(
            &fake,
            &store,
            &before,
            &["nginx:1.27".to_owned()],
            &["a/b".to_owned()],
        )
        .await;
        assert!(!store.exists());
    }

    #[test]
    fn updates_add_to_the_record_and_a_damaged_one_starts_over() {
        let store = store();
        let image = |r: &str| OwnedImage {
            reference: r.to_owned(),
            id: format!("sha256:{r}"),
        };
        update(&store, |o| {
            o.images.insert(image("a"));
        })
        .unwrap();
        update(&store, |o| {
            o.images.insert(image("b"));
        })
        .unwrap();
        assert_eq!(load(&store).images.len(), 2);
        std::fs::write(&store, "{not json").unwrap();
        assert_eq!(load(&store), Owned::default());
        std::fs::remove_dir_all(store.parent().unwrap()).ok();
    }

    #[test]
    fn concurrent_updates_each_land_under_the_lock() {
        // Without the file lock, read-modify-write would clobber: the threads would overwrite each
        // other and the record would hold fewer than eight entries.
        let store = store();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let store = store.clone();
                std::thread::spawn(move || {
                    update(&store, |o| {
                        o.images.insert(OwnedImage {
                            reference: format!("r{i}"),
                            id: format!("id{i}"),
                        });
                    })
                    .unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(load(&store).images.len(), 8);
        std::fs::remove_dir_all(store.parent().unwrap()).ok();
    }
}
