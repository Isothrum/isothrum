//! `vagrant.remove_orphans`: this run's VMs Vagrant lost track of (a crashed import, a run
//! folder replaced while its VMs ran), removed at the hypervisor. A VM is this run's when its name
//! carries the run's key ([`crate::copy_key`], every tool's [`crate::RUN_KEY_VAR`]) as a word of
//! its own: the key is made from this runner's home and the run's name, so another run of the same
//! project (a second copy, from another folder or another home) never matches, even while its own
//! start fails beside this one, and neither does a user's own VM. A project whose VMs' names don't
//! carry the key has none removed. VirtualBox and Parallels; the manifest names the providers it
//! runs on.

use std::path::{Path, PathBuf};

use isoloom_manifest::{Provider, Step};

use super::READ_LIMIT;
use super::actions::vboxmanage;
use crate::Result;
use crate::events::WarningCode;
use crate::executor::{Cx, Echo, call};
use crate::process::ProcessSpec;
use crate::store::if_there;
use crate::tool_output::vboxmanage::{machine_folder, vms as parse_vms};

/// Whether the VM (or VM folder) named `name` is the run's whose key is `key`: the key is a word
/// of the name, a run of letters and digits between characters that are neither (or its ends).
pub(super) fn ours(name: &str, key: &str) -> bool {
    name.split(|c: char| !c.is_alphanumeric()).any(|word| word == key)
}

/// The run's key ([`crate::copy_key`]).
fn key(cx: &Cx<'_>) -> String {
    crate::runner::copy_key(cx.home(), cx.run.name())
}

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

/// Runs `spec`, a step towards a removal whose own failure says nothing (a VM that isn't running
/// can't be powered off): only traced.
async fn attempt(cx: &Cx<'_>, step: &Step, spec: &ProcessSpec) -> Result<()> {
    let out = call(cx, step.id.as_str(), spec, READ_LIMIT, Echo::Quiet).await?;
    if let Err(failed) = out.output() {
        tracing::debug!(
            call = spec.line(),
            reason = failed.reason,
            "a step of an orphan's removal failed (nothing to stop, or it can't be)"
        );
    }
    Ok(())
}

/// Runs `spec`, the call that removes `what`: said once it is done; when it fails, a warning
/// with why, since what it removes may still be there.
async fn removal(cx: &Cx<'_>, step: &Step, spec: &ProcessSpec, what: &str) -> Result<()> {
    let out = call(cx, step.id.as_str(), spec, READ_LIMIT, Echo::Quiet).await?;
    match out.output() {
        Ok(_) => removed(cx, what),
        Err(failed) => not_removed(cx, what, &failed.reason),
    }
    Ok(())
}

/// Says `what`, an orphan, was removed.
fn removed(cx: &Cx<'_>, what: &str) {
    cx.events.warning(WarningCode::OrphanRemoved, &format!("removed {what}"));
}

/// Says `what`, an orphan, couldn't be removed, and why.
fn not_removed(cx: &Cx<'_>, what: &str, reason: &str) {
    tracing::warn!(what, reason, "an orphan couldn't be removed");
    cx.events.warning(WarningCode::CleanupFailed, &format!("{what} couldn't be removed: {reason}"));
}

/// What `spec`, a listing of the hypervisor's VMs or settings, prints; `None` when it fails,
/// with a `cleanup_failed` warning, as every clean-up that fails has: no orphan it would have
/// found is removed.
async fn listed(cx: &Cx<'_>, step: &Step, spec: &ProcessSpec) -> Result<Option<String>> {
    let out = call(cx, step.id.as_str(), spec, READ_LIMIT, Echo::Quiet).await?;
    Ok(out
        .output()
        .inspect_err(|reason| {
            tracing::warn!(call = spec.line(), reason = %reason, "the hypervisor couldn't be read: no orphan it lists is removed");
            let said = format!(
                "what earlier runs left couldn't be listed (`{}`), so none of it is removed: {reason}",
                spec.line()
            );
            cx.events.warning(WarningCode::CleanupFailed, &said);
        })
        .ok())
}

/// VirtualBox: each registered VM of ours powered off and deleted (a saved one's state discarded
/// first: it can't be powered off or unregistered until then), then the folders a failed import
/// left under the machine folder, which make the next import fail with `VERR_ALREADY_EXISTS`.
async fn virtualbox(cx: &Cx<'_>, step: &Step) -> Result<()> {
    let key = key(cx);
    if let Some(vms) = listed(cx, step, &vboxmanage(&["list", "vms"])).await? {
        for (name, uuid) in parse_vms(&vms).into_iter().filter(|(n, _)| ours(n, &key)) {
            attempt(cx, step, &vboxmanage(&["discardstate", &uuid])).await?;
            attempt(cx, step, &vboxmanage(&["controlvm", &uuid, "poweroff"])).await?;
            let what = format!("a VM an earlier run left ({name})");
            removal(cx, step, &vboxmanage(&["unregistervm", &uuid, "--delete"]), &what).await?;
        }
    }
    let properties = listed(cx, step, &vboxmanage(&["list", "systemproperties"])).await?;
    let Some(base) = properties.as_deref().and_then(machine_folder) else {
        return Ok(());
    };
    let folders = leftover_folders(&base, &key).unwrap_or_else(|e| {
        let what = format!("the VM folders an earlier run left in {}", base.display());
        not_removed(cx, &what, &format!("the folder can't be read: {e}"));
        Vec::new()
    });
    for folder in folders {
        let what = format!("a VM folder an earlier run left ({})", folder.display());
        match crate::store::remove_path(&folder) {
            Ok(()) => removed(cx, &what),
            Err(e) => not_removed(cx, &what, &e.to_string()),
        }
    }
    Ok(())
}

/// Parallels: each VM of ours stopped and deleted.
async fn parallels(cx: &Cx<'_>, step: &Step) -> Result<()> {
    let key = key(cx);
    let prlctl = |args: &[&str]| ProcessSpec::new("prlctl", args.iter().copied());
    let Some(vms) = listed(cx, step, &prlctl(&["list", "-a", "--no-header", "-o", "name"])).await? else {
        return Ok(());
    };
    for name in vms.lines().map(str::trim).filter(|n| ours(n, &key)) {
        attempt(cx, step, &prlctl(&["stop", name, "--kill"])).await?;
        removal(cx, step, &prlctl(&["delete", name]), &format!("a VM an earlier run left ({name})")).await?;
    }
    Ok(())
}

/// The entries under `base` that are folders of run `key`'s VMs: VirtualBox names a VM's folder
/// after the VM (none when `base` isn't there).
///
/// # Errors
///
/// `base`, or an entry of it, can't be read.
pub(super) fn leftover_folders(base: &Path, key: &str) -> std::io::Result<Vec<PathBuf>> {
    let Some(entries) = if_there(std::fs::read_dir(base))? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        if (kind.is_dir() || kind.is_symlink()) && ours(&entry.file_name().to_string_lossy(), key) {
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
    fn a_vm_is_this_runs_by_its_key_alone() {
        assert!(ours("lab-k3x9a2 - dc01", "k3x9a2"));
        assert!(ours("lab-2-k3x9a2 - isoloom-controller", "k3x9a2"));
        assert!(ours("k3x9a2", "k3x9a2") && ours("lab_k3x9a2_dc01", "k3x9a2") && ours("(k3x9a2) dc01", "k3x9a2"));
        for theirs in [
            "lab-zz0000 - dc01",
            "lab - dc01",
            "lab-dc01",
            "lab-k3x9a2x - dc01",
            "labk3x9a2 - dc01",
            "lab-1k3x9a2",
        ] {
            assert!(!ours(theirs, "k3x9a2"), "{theirs}");
        }
    }

    #[test]
    fn leftover_folders_are_the_runs_by_its_key() {
        let base = tempfile::tempdir().unwrap();
        for f in ["lab-k3x9a2 - dc01", "lab-zz0000 - dc01", "lab - dc01", "unrelated"] {
            std::fs::create_dir_all(base.path().join(f)).unwrap();
        }
        std::fs::write(base.path().join("lab-k3x9a2 - ws01"), "a file, not a folder").unwrap();
        assert_eq!(leftover_folders(base.path(), "k3x9a2").unwrap(), [base.path().join("lab-k3x9a2 - dc01")]);
        assert_eq!(leftover_folders(&base.path().join("nope"), "k3x9a2").unwrap(), Vec::<PathBuf>::new());
        // Regression: a machine folder that couldn't be read read as one with nothing left.
        assert!(
            leftover_folders(&base.path().join("lab-k3x9a2 - ws01"), "k3x9a2").is_err(),
            "a file isn't a folder"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_leftover_link_is_removed_never_followed() {
        let base = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("keep.txt"), "x").unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), base.path().join("lab-k3x9a2 - dc01")).unwrap();
        for f in leftover_folders(base.path(), "k3x9a2").unwrap() {
            crate::store::remove_path(&f).unwrap();
        }
        assert!(elsewhere.path().join("keep.txt").is_file());
        assert!(std::fs::symlink_metadata(base.path().join("lab-k3x9a2 - dc01")).is_err());
    }
}
