//! `vagrant.remove_orphans`: this run's VMs Vagrant lost track of (a crashed import, a run
//! folder replaced while its VMs ran), removed at the hypervisor by their exact names from the
//! manifest. Never a lossy match: a user's own `run-dc01` must survive this run's `run · dc01`,
//! since `unregistervm --delete` can't be undone. VirtualBox and Parallels; the manifest names the
//! providers it runs on.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use isoloom_manifest::{Provider, Step};

use super::READ_LIMIT;
use super::actions::{vboxmanage, vm_names};
use crate::Result;
use crate::events::WarningCode;
use crate::executor::{Cx, Echo, call};
use crate::process::ProcessSpec;
use crate::store::if_there;

/// Removes the run's orphaned VMs on its provider.
pub(super) async fn remove(cx: &Cx<'_>, step: &Step) -> Result<()> {
    if cx.scope.runs_on(Provider::Virtualbox) {
        virtualbox(cx, step).await
    } else if cx.scope.runs_on(Provider::Parallels) {
        parallels(cx, step).await
    } else {
        Ok(())
    }
}

/// Runs `spec`, its failure only traced: each removal is best effort ("nothing there" is fine).
async fn attempt(cx: &Cx<'_>, step: &Step, spec: &ProcessSpec) -> Result<()> {
    let out = call(cx, step.id.as_str(), spec, READ_LIMIT, Echo::Quiet).await?;
    if let Err(reason) = out.output() {
        tracing::debug!(call = spec.line(), reason, "a removal of an orphan failed (nothing there, or it can't be)");
    }
    Ok(())
}

/// What `spec`, a listing of the hypervisor's VMs or settings, prints; `None` when it fails,
/// with a warning: no orphan it would have found is removed.
async fn listed(cx: &Cx<'_>, step: &Step, spec: &ProcessSpec) -> Result<Option<String>> {
    let out = call(cx, step.id.as_str(), spec, READ_LIMIT, Echo::Quiet).await?;
    Ok(out
        .output()
        .inspect_err(|reason| tracing::warn!(call = spec.line(), reason, "the hypervisor couldn't be read: no orphan it lists is removed"))
        .ok())
}

/// VirtualBox: each registered VM of ours powered off and deleted (a saved one's state discarded
/// first: it can't be powered off or unregistered until then), then the folders a failed import
/// left under the machine folder, which make the next import fail with `VERR_ALREADY_EXISTS`.
async fn virtualbox(cx: &Cx<'_>, step: &Step) -> Result<()> {
    let wanted: BTreeSet<String> = vm_names(cx, Provider::Virtualbox).into_iter().collect();
    if wanted.is_empty() {
        return Ok(());
    }
    if let Some(vms) = listed(cx, step, &vboxmanage(&["list", "vms"])).await? {
        for (name, uuid) in parse_vms(&vms).into_iter().filter(|(n, _)| wanted.contains(n)) {
            cx.events
                .warning(WarningCode::OrphanRemoved, &format!("removing a VM an earlier run left: {name}"));
            attempt(cx, step, &vboxmanage(&["discardstate", &uuid])).await?;
            attempt(cx, step, &vboxmanage(&["controlvm", &uuid, "poweroff"])).await?;
            attempt(cx, step, &vboxmanage(&["unregistervm", &uuid, "--delete"])).await?;
        }
    }
    let properties = listed(cx, step, &vboxmanage(&["list", "systemproperties"])).await?;
    let Some(base) = properties.as_deref().and_then(machine_folder) else {
        return Ok(());
    };
    let folders = leftover_folders(&base, &wanted).unwrap_or_else(|e| {
        cx.events.warning(WarningCode::StepTolerated, &format!("{}: {e}", base.display()));
        Vec::new()
    });
    for folder in folders {
        cx.events.warning(
            WarningCode::OrphanRemoved,
            &format!("removing a VM folder an earlier run left: {}", folder.display()),
        );
        if let Err(e) = crate::store::remove_path(&folder) {
            cx.events.warning(WarningCode::StepTolerated, &format!("{}: {e}", folder.display()));
        }
    }
    Ok(())
}

/// Parallels: each VM of ours stopped and deleted.
async fn parallels(cx: &Cx<'_>, step: &Step) -> Result<()> {
    let wanted: BTreeSet<String> = vm_names(cx, Provider::Parallels).into_iter().collect();
    let prlctl = |args: &[&str]| ProcessSpec::new("prlctl", args.iter().copied());
    let Some(vms) = listed(cx, step, &prlctl(&["list", "-a", "--no-header", "-o", "name"])).await? else {
        return Ok(());
    };
    for name in vms.lines().map(str::trim).filter(|n| wanted.contains(*n)) {
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

/// The entries under `base` named exactly like one of `wanted`'s folders (none when `base` isn't
/// there).
///
/// # Errors
///
/// `base`, or an entry of it, can't be read.
pub(super) fn leftover_folders(base: &Path, wanted: &BTreeSet<String>) -> std::io::Result<Vec<PathBuf>> {
    let names: BTreeSet<String> = wanted.iter().flat_map(|n| folder_candidates(n)).collect();
    let Some(entries) = if_there(std::fs::read_dir(base))? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        if (kind.is_dir() || kind.is_symlink()) && names.contains(entry.file_name().to_string_lossy().as_ref()) {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
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
        let c = folder_candidates("lab · dc01");
        assert!(c.contains(&"lab · dc01".to_owned()) && c.contains(&"lab - dc01".to_owned()));
        assert!(!c.iter().any(|f| f == "lab-dc01"), "never a collapsed form");
    }

    #[test]
    fn leftover_folders_go_by_their_exact_sanitized_names() {
        let base = tempfile::tempdir().unwrap();
        for f in ["lab - dc01", "lab-dc01", "unrelated"] {
            std::fs::create_dir_all(base.path().join(f)).unwrap();
        }
        std::fs::write(base.path().join("lab · dc01"), "a file, not a folder").unwrap();
        let wanted = BTreeSet::from(["lab · dc01".to_owned()]);
        assert_eq!(leftover_folders(base.path(), &wanted).unwrap(), [base.path().join("lab - dc01")]);
        assert_eq!(leftover_folders(&base.path().join("nope"), &wanted).unwrap(), Vec::<PathBuf>::new());
        // Regression: a machine folder that couldn't be read read as one with nothing left.
        assert!(leftover_folders(&base.path().join("lab · dc01"), &wanted).is_err(), "a file isn't a folder");
    }

    #[cfg(unix)]
    #[test]
    fn a_leftover_link_is_removed_never_followed() {
        let base = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("keep.txt"), "x").unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), base.path().join("lab - dc01")).unwrap();
        let wanted = BTreeSet::from(["lab · dc01".to_owned()]);
        for f in leftover_folders(base.path(), &wanted).unwrap() {
            crate::store::remove_path(&f).unwrap();
        }
        assert!(elsewhere.path().join("keep.txt").is_file());
        assert!(std::fs::symlink_metadata(base.path().join("lab - dc01")).is_err());
    }
}
