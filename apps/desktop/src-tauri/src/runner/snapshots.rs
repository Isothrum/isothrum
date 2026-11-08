//! Snapshots: a machine's container saved as an image (`docker commit`), and brought back by
//! running the machine from that image. What's in the container is kept (files, a router's saved
//! configuration, a QEMU disk); named volumes aren't (`docker commit` leaves them out).

use std::path::Path;
use std::process::Command;

use super::machines::with_sidecar;
use super::{RunError, Runtime, compose, container, lab_spec, run, runtime};

/// The record of a lab's snapshots, in its folder.
const SNAPSHOTS: &str = "snapshots.json";
/// The Compose override that runs restored machines from their snapshot's image.
pub(super) const SNAPSHOT_OVERRIDE: &str = "snapshots.override.yml";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Snapshot {
    pub id: String,
    pub machine: String,
    pub image: String,
    pub label: String,
    /// Seconds since the epoch.
    pub created: u64,
    /// The machine runs from this snapshot now.
    #[serde(default)]
    pub in_use: bool,
}

pub(crate) fn snapshots(dir: &Path) -> Vec<Snapshot> {
    std::fs::read_to_string(dir.join(SNAPSHOTS))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_snapshots(dir: &Path, list: &[Snapshot]) -> Result<(), RunError> {
    let text = serde_json::to_string_pretty(list).map_err(|e| RunError::Io(e.to_string()))?;
    std::fs::write(dir.join(SNAPSHOTS), text).map_err(|e| RunError::Io(e.to_string()))?;
    // The override: each machine in use from a snapshot, on that snapshot's image.
    let used: Vec<&Snapshot> = list.iter().filter(|s| s.in_use).collect();
    let path = dir.join(SNAPSHOT_OVERRIDE);
    if used.is_empty() {
        drop(std::fs::remove_file(path));
        return Ok(());
    }
    let mut yaml = String::from("# Machines restored from a snapshot (Isothrum).\nservices:\n");
    for s in used {
        yaml.push_str("  ");
        yaml.push_str(&s.machine);
        yaml.push_str(":\n    image: ");
        yaml.push_str(&s.image);
        yaml.push('\n');
    }
    std::fs::write(path, yaml).map_err(|e| RunError::Io(e.to_string()))
}

/// Saves a machine's container as an image (the machine pauses for the copy).
pub(crate) fn take_snapshot(
    dir: &Path,
    lab: &str,
    machine: &str,
    label: &str,
) -> Result<Snapshot, RunError> {
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut list = snapshots(dir);
    let stamp = unique_stamp(&list, machine, created);
    let image = match runtime(dir) {
        Runtime::Docker => commit_container(dir, lab, machine, label, &stamp)?,
        rt => save_vm(dir, rt, machine, &stamp)?,
    };
    let snap = Snapshot {
        id: format!("{machine}-{stamp}"),
        machine: machine.into(),
        image,
        label: match label.trim() {
            "" => "snapshot".into(),
            l => l.into(),
        },
        created,
        in_use: false,
    };
    list.push(snap.clone());
    save_snapshots(dir, &list)?;
    Ok(snap)
}

/// What tells a machine's snapshot apart: the second it was taken, plus `-2`, `-3`… when the
/// machine already has one from that second.
fn unique_stamp(list: &[Snapshot], machine: &str, created: u64) -> String {
    let taken = |stamp: &str| list.iter().any(|s| s.id == format!("{machine}-{stamp}"));
    let mut n = 1u64;
    loop {
        let stamp = if n == 1 {
            created.to_string()
        } else {
            format!("{created}-{n}")
        };
        if !taken(&stamp) {
            return stamp;
        }
        n += 1;
    }
}

/// Saves a machine's container as an image; returns the image.
fn commit_container(
    dir: &Path,
    lab: &str,
    machine: &str,
    label: &str,
    stamp: &str,
) -> Result<String, RunError> {
    let id = container(dir, machine)?;
    let image = format!("isothrum-snapshots/{lab}-{machine}:{stamp}");
    let mut c = Command::new("docker");
    c.args(["commit", "--message", label, &id, &image]);
    run(c, "Saving the machine")?;
    Ok(image)
}

/// Saves a VM with Vagrant's own snapshot (the provider's), named like the record.
fn save_vm(dir: &Path, rt: Runtime, machine: &str, stamp: &str) -> Result<String, RunError> {
    let spec = lab_spec(dir)?;
    if crate::vms::in_host(&spec, rt, machine) {
        return Err(RunError::Spec(
            "snapshots of a container in a lab with VMs come later: snapshot its VMs".into(),
        ));
    }
    let name = format!("{machine}-{stamp}");
    crate::vms::snapshot_save(dir, rt, machine, &name)?;
    Ok(format!("vagrant:{name}"))
}

/// Runs the machine from a snapshot (or, with `None`, from its own image again), recreating it
/// with its sidecar.
pub(crate) fn use_snapshot(
    dir: &Path,
    machine: &str,
    id: Option<&str>,
) -> Result<String, RunError> {
    let mut list = snapshots(dir);
    if let Some(id) = id
        && !list.iter().any(|s| s.id == id && s.machine == machine)
    {
        return Err(RunError::Spec(format!("no snapshot `{id}` of `{machine}`")));
    }
    // A VM goes back to the snapshot's state and carries on from there: nothing to undo later.
    let out = if let rt @ (Runtime::Vagrant | Runtime::Hybrid) = runtime(dir) {
        match id {
            Some(id) => crate::vms::snapshot_restore(dir, rt, machine, id)?,
            None => return Ok(String::new()),
        }
    } else {
        for s in list.iter_mut().filter(|s| s.machine == machine) {
            s.in_use = Some(s.id.as_str()) == id;
        }
        save_snapshots(dir, &list)?;
        let services = with_sidecar(dir, machine)?;
        let names: Vec<&str> = services.iter().map(String::as_str).collect();
        run(
            compose(
                dir,
                &[&["up", "-d", "--no-deps", "--force-recreate"][..], &names].concat(),
            ),
            "Bringing the machine back",
        )?
    };
    // The machine is back with its ports up: its suspended links are cut again.
    super::links::reapply_links(dir, &lab_spec(dir)?, machine)?;
    Ok(out)
}

/// Forgets a snapshot and removes its image (not while the machine runs from it).
pub(crate) fn delete_snapshot(dir: &Path, id: &str) -> Result<(), RunError> {
    let mut list = snapshots(dir);
    let Some(i) = list.iter().position(|s| s.id == id) else {
        return Err(RunError::Spec(format!("no snapshot `{id}`")));
    };
    if let Some(s) = list.get(i)
        && s.in_use
    {
        return Err(RunError::Spec(format!(
            "`{}` runs from this snapshot: bring it back to its own image first",
            s.machine
        )));
    }
    let snap = list.remove(i);
    if let rt @ (Runtime::Vagrant | Runtime::Hybrid) = runtime(dir) {
        crate::vms::snapshot_delete(dir, rt, &snap.machine, &snap.id)?;
        return save_snapshots(dir, &list);
    }
    drop(run(
        {
            let mut c = Command::new("docker");
            c.args(["rmi", &snap.image]);
            c
        },
        "Removing the snapshot",
    ));
    save_snapshots(dir, &list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{down, prepare, up};

    #[test]
    fn two_snapshots_in_one_second_get_their_own_ids() {
        let snap = |id: &str| Snapshot {
            id: id.into(),
            machine: "pc".into(),
            image: String::new(),
            label: String::new(),
            created: 100,
            in_use: false,
        };
        assert_eq!(unique_stamp(&[], "pc", 100), "100");
        assert_eq!(unique_stamp(&[snap("pc-100")], "pc", 100), "100-2");
        assert_eq!(
            unique_stamp(&[snap("pc-100"), snap("pc-100-2")], "pc", 100),
            "100-3"
        );
        assert_eq!(unique_stamp(&[snap("pc-100")], "srv", 100), "100");
    }

    /// Needs Docker (run with `--ignored`): a snapshot keeps what was in the machine.
    #[test]
    #[ignore = "needs Docker; run with --ignored"]
    fn a_snapshot_brings_a_machine_back_as_it_was() {
        let dir = std::env::temp_dir().join(format!("isothrum-snap-{}", std::process::id()));
        let yaml = "version: 1\nname: snap\nnetworks:\n  a: { cidr: 10.62.1.0/24 }\nmachines:\n  pc: { networks: { a: 10 }, app: { image: \"alpine:3.20\" }, command: [sleep, infinity] }\n";
        prepare(&dir, yaml, &[]).unwrap();
        up(&dir).unwrap();
        let sh = |cmd: &str| {
            run(
                compose(&dir, &["exec", "-T", "pc", "sh", "-c", cmd]),
                "exec",
            )
        };
        sh("echo kept > /root/note").unwrap();
        let snap = take_snapshot(&dir, "snap", "pc", "before the change").unwrap();
        sh("rm /root/note").unwrap();
        use_snapshot(&dir, "pc", Some(&snap.id)).unwrap();
        assert_eq!(sh("cat /root/note").unwrap().trim(), "kept", "restored");
        assert!(snapshots(&dir)[0].in_use);
        assert!(delete_snapshot(&dir, &snap.id).is_err(), "not while in use");
        use_snapshot(&dir, "pc", None).unwrap();
        assert!(sh("cat /root/note").is_err(), "its own image again");
        assert!(!dir.join(SNAPSHOT_OVERRIDE).exists());
        delete_snapshot(&dir, &snap.id).unwrap();
        assert!(snapshots(&dir).is_empty());
        let gone = Command::new("docker")
            .args(["image", "inspect", &snap.image])
            .output()
            .unwrap();
        assert!(!gone.status.success(), "its image is removed");
        down(&dir).unwrap();
        drop(std::fs::remove_dir_all(&dir));
    }
}
