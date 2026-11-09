//! `vagrant.remove_orphans`: this run's VMs Vagrant lost track of (a crashed import, a run
//! folder replaced while its VMs ran), removed at the hypervisor by their exact names from the
//! manifest. Never a lossy match: a user's own `run-dc01` must survive this run's `run · dc01`,
//! since `unregistervm --delete` can't be undone. VirtualBox and Parallels; the manifest names the
//! providers it runs on.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use isoloom_manifest::{Provider, Step};

use super::READ_TIMEOUT;
use super::actions::{vboxmanage, vm_names};
use crate::Result;
use crate::events::WarningCode;
use crate::executor::{Cx, Echo, call};
use crate::process::{Exit, ProcessSpec};

/// Removes the run's orphaned VMs on its provider.
pub(super) async fn remove(cx: &Cx<'_>, step: &Step) -> Result<()> {
    match cx.scope.provider.as_deref() {
        Some("virtualbox") => virtualbox(cx, step).await,
        Some("parallels") => parallels(cx, step).await,
        _ => Ok(()),
    }
}

/// Runs `spec`, its failure only noted: each removal is best effort ("nothing there" is fine).
async fn attempt(cx: &Cx<'_>, step: &Step, spec: &ProcessSpec) -> Result<Option<String>> {
    let out = call(cx, step.id.as_str(), spec, READ_TIMEOUT, Echo::Quiet).await?;
    Ok(out.exit.is_some_and(Exit::success).then(|| out.stdout.join("\n")))
}

/// VirtualBox: each registered VM of ours powered off and deleted (a saved one's state discarded
/// first: it can't be powered off or unregistered until then), then the folders a failed import
/// left under the machine folder, which make the next import fail with `VERR_ALREADY_EXISTS`.
async fn virtualbox(cx: &Cx<'_>, step: &Step) -> Result<()> {
    let wanted: BTreeSet<String> = vm_names(cx, Provider::Virtualbox).into_iter().collect();
    if wanted.is_empty() {
        return Ok(());
    }
    if let Some(listed) = attempt(cx, step, &vboxmanage(&["list", "vms"])).await? {
        for (name, uuid) in parse_vms(&listed).into_iter().filter(|(n, _)| wanted.contains(n)) {
            cx.events
                .warning(WarningCode::OrphanRemoved, &format!("removing a VM an earlier run left: {name}"));
            attempt(cx, step, &vboxmanage(&["discardstate", &uuid])).await?;
            attempt(cx, step, &vboxmanage(&["controlvm", &uuid, "poweroff"])).await?;
            attempt(cx, step, &vboxmanage(&["unregistervm", &uuid, "--delete"])).await?;
        }
    }
    let properties = attempt(cx, step, &vboxmanage(&["list", "systemproperties"])).await?;
    if let Some(base) = properties.as_deref().and_then(machine_folder) {
        for folder in leftover_folders(&base, &wanted) {
            cx.events.warning(
                WarningCode::OrphanRemoved,
                &format!("removing a VM folder an earlier run left: {}", folder.display()),
            );
            if let Err(e) = remove_entry(&folder) {
                cx.events.warning(WarningCode::StepTolerated, &format!("{}: {e}", folder.display()));
            }
        }
    }
    Ok(())
}

/// Parallels: each VM of ours stopped and deleted.
async fn parallels(cx: &Cx<'_>, step: &Step) -> Result<()> {
    let wanted: BTreeSet<String> = vm_names(cx, Provider::Parallels).into_iter().collect();
    let prlctl = |args: &[&str]| ProcessSpec::new("prlctl", args.iter().copied());
    let Some(listed) = attempt(cx, step, &prlctl(&["list", "-a", "--no-header", "-o", "name"])).await? else {
        return Ok(());
    };
    for name in listed.lines().map(str::trim).filter(|n| wanted.contains(*n)) {
        cx.events
            .warning(WarningCode::OrphanRemoved, &format!("removing a VM an earlier run left: {name}"));
        attempt(cx, step, &prlctl(&["stop", name, "--kill"])).await?;
        attempt(cx, step, &prlctl(&["delete", name])).await?;
    }
    Ok(())
}

/// `(name, uuid)` of each VM `VBoxManage list vms` prints (`"<name>" {<uuid>}` per line).
pub(super) fn parse_vms(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|line| {
            let line = line.trim_end_matches('\r');
            let (head, tail) = line.rsplit_once('{')?;
            let uuid = tail.strip_suffix('}')?;
            Some((head.trim().trim_matches('"').to_owned(), uuid.to_owned()))
        })
        .collect()
}

/// The `Default machine folder:` of `VBoxManage list systemproperties`.
pub(super) fn machine_folder(out: &str) -> Option<PathBuf> {
    out.lines()
        .find_map(|l| l.trim_end_matches('\r').strip_prefix("Default machine folder:"))
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

/// The folder names VirtualBox gives a VM named `name`: as it is, and with the middot made a
/// dash (`run · dc01` is in `run - dc01`). Exact forms only.
pub(super) fn folder_candidates(name: &str) -> [String; 2] {
    [name.to_owned(), name.replace('\u{00b7}', "-")]
}

/// The entries under `base` named exactly like one of `wanted`'s folders.
pub(super) fn leftover_folders(base: &Path, wanted: &BTreeSet<String>) -> Vec<PathBuf> {
    let names: BTreeSet<String> = wanted.iter().flat_map(|n| folder_candidates(n)).collect();
    let Ok(entries) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() || t.is_symlink()))
        .filter(|e| names.contains(e.file_name().to_string_lossy().as_ref()))
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// Removes a leftover folder; a symbolic link is removed itself, never what it points at.
fn remove_entry(path: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        std::fs::remove_file(path).or_else(|_| std::fs::remove_dir(path))
    } else {
        std::fs::remove_dir_all(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtualbox_listings_keep_braces_and_unicode_inside_a_name() {
        let out = "\"run · {odd} name\" {1111-aaaa}\r\n\"日本語 · dc01\" {2222}\ngarbage\n\"<inaccessible>\" {3333}\n";
        assert_eq!(
            parse_vms(out),
            [
                ("run · {odd} name".to_owned(), "1111-aaaa".to_owned()),
                ("日本語 · dc01".to_owned(), "2222".to_owned()),
                ("<inaccessible>".to_owned(), "3333".to_owned())
            ]
        );
    }

    #[test]
    fn reads_virtualboxs_machine_folder() {
        let out = "API version: 7_0\nDefault machine folder:          /Users/a/VirtualBox VMs\nVRDE auth library: x\n";
        assert_eq!(machine_folder(out), Some(PathBuf::from("/Users/a/VirtualBox VMs")));
        assert_eq!(machine_folder("Default machine folder:  D:\\VMs\r\n"), Some(PathBuf::from("D:\\VMs")));
        assert_eq!(machine_folder("Default machine folder:   \n"), None);
        assert_eq!(machine_folder(""), None);
    }

    #[test]
    fn folder_candidates_cover_virtualbox_sanitization_exactly() {
        let c = folder_candidates("minilab · dc01");
        assert!(c.contains(&"minilab · dc01".to_owned()) && c.contains(&"minilab - dc01".to_owned()));
        assert!(!c.iter().any(|f| f == "minilab-dc01"), "never a collapsed form");
    }

    #[test]
    fn leftover_folders_go_by_their_exact_sanitized_names() {
        let base = tempfile::tempdir().unwrap();
        for f in ["minilab - dc01", "minilab-dc01", "unrelated"] {
            std::fs::create_dir_all(base.path().join(f)).unwrap();
        }
        std::fs::write(base.path().join("minilab · dc01"), "a file, not a folder").unwrap();
        let wanted = BTreeSet::from(["minilab · dc01".to_owned()]);
        assert_eq!(leftover_folders(base.path(), &wanted), [base.path().join("minilab - dc01")]);
        assert_eq!(leftover_folders(&base.path().join("nope"), &wanted), Vec::<PathBuf>::new());
    }

    #[cfg(unix)]
    #[test]
    fn a_leftover_link_is_removed_never_followed() {
        let base = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("keep.txt"), "x").unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), base.path().join("minilab - dc01")).unwrap();
        let wanted = BTreeSet::from(["minilab · dc01".to_owned()]);
        for f in leftover_folders(base.path(), &wanted) {
            remove_entry(&f).unwrap();
        }
        assert!(elsewhere.path().join("keep.txt").is_file());
        assert!(std::fs::symlink_metadata(base.path().join("minilab - dc01")).is_err());
    }
}
