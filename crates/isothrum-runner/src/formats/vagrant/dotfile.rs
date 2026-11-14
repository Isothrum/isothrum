//! Vagrant's own record of the run: its dotfile folder (`VAGRANT_DOTFILE_PATH`), kept in the run's
//! state folder so a replaced project folder never loses track of its VMs. Vagrant writes each
//! machine it made as `machines/<machine>/<provider>/id`, the hypervisor's own id of its VM: what
//! the driver reports as the run's resources, and what it removes a VM by after a crash.

use std::path::{Path, PathBuf};

use crate::Result;
use crate::error::Error;
use crate::store::if_there;

/// The variable Vagrant reads its dotfile folder from.
pub(super) const VAR: &str = "VAGRANT_DOTFILE_PATH";

/// The dotfile folder's name in the run's state folder.
const FOLDER: &str = "vagrant";

/// One machine Vagrant recorded as made.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Record {
    /// Its define name in the Vagrantfile.
    pub(super) machine: String,
    /// The provider it was made on, as Vagrant names it (`virtualbox`).
    pub(super) provider: String,
    /// Its VM's id at the hypervisor.
    pub(super) id: String,
}

/// The run's dotfile folder.
#[derive(Debug, Clone)]
pub(super) struct Dotfile {
    /// Where it is, absolute.
    pub(super) path: PathBuf,
}

impl Dotfile {
    /// The dotfile folder of a run whose state folder is `state_dir`.
    pub(super) fn in_state(state_dir: &Path) -> Dotfile {
        Dotfile { path: state_dir.join(FOLDER) }
    }

    /// Every machine Vagrant recorded as made, on any provider, sorted. A provider folder with no
    /// `id` (a destroy, or a mere `vagrant status`, leaves one) or an empty one is no machine, and
    /// a file where a machine's folder would be (a `.DS_Store`) is none either. No `machines`
    /// folder is no record; one that can't be read is an error, never nothing recorded.
    pub(super) fn recorded(&self) -> Result<Vec<Record>> {
        let folder = self.path.join("machines");
        let read = |p: &Path| Error::io(p.to_path_buf(), "read");
        let Some(machines) = if_there(std::fs::read_dir(&folder)).map_err(read(&folder))? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for machine in machines {
            let machine = machine.map_err(read(&folder))?.path();
            if !machine.is_dir() {
                continue;
            }
            let Some(providers) = if_there(std::fs::read_dir(&machine)).map_err(read(&machine))? else {
                continue;
            };
            for provider in providers {
                let provider = provider.map_err(read(&machine))?;
                let id_file = provider.path().join("id");
                let Some(id) = if_there(std::fs::read_to_string(&id_file)).map_err(read(&id_file))? else {
                    continue;
                };
                let id = id.trim();
                if id.is_empty() {
                    continue;
                }
                out.push(Record {
                    machine: file_name(&machine),
                    provider: provider.file_name().to_string_lossy().into_owned(),
                    id: id.to_owned(),
                });
            }
        }
        out.sort();
        Ok(out)
    }

    /// Writes `record` back as Vagrant would have (`machines/<machine>/<provider>/id`), so Vagrant
    /// acts on a VM whose record was lost (a dotfile reset after a crash) by its own id.
    pub(super) fn write_back(&self, record: &Record) -> Result<()> {
        let dir = self.path.join("machines").join(&record.machine).join(&record.provider);
        crate::store::create_dir(&dir)?;
        crate::store::write(&dir.join("id"), record.id.as_bytes())
    }

    /// Drops `machine`'s record on `provider` (its VM removed some other way than by Vagrant).
    pub(super) fn forget(&self, machine: &str, provider: &str) -> Result<()> {
        crate::store::remove(&self.path.join("machines").join(machine).join(provider))
    }

    /// Removes the whole folder (a lock a killed process left, a record of VMs that are gone).
    pub(super) fn reset(&self) -> Result<()> {
        crate::store::remove(&self.path)
    }
}

/// A path's last part, as text.
fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn only_a_provider_folder_with_an_id_is_a_machine() {
        let state = tempfile::tempdir().unwrap();
        let dot = Dotfile::in_state(state.path());
        assert_eq!(dot.recorded().unwrap(), Vec::new(), "no folder, no record");
        write(&dot.path, "machines/web/libvirt/id", "uuid-web\n");
        write(&dot.path, "machines/db/libvirt/index_uuid", "x");
        write(&dot.path, "machines/old/virtualbox/id", "  ");
        write(&dot.path, "machines/.DS_Store", "");
        write(&dot.path, "machines/attack/virtualbox/id", "uuid-a");
        assert_eq!(
            dot.recorded().unwrap(),
            [
                Record {
                    machine: "attack".into(),
                    provider: "virtualbox".into(),
                    id: "uuid-a".into()
                },
                Record {
                    machine: "web".into(),
                    provider: "libvirt".into(),
                    id: "uuid-web".into()
                },
            ]
        );
    }

    #[test]
    fn a_record_written_back_reads_back_and_a_reset_forgets_everything() {
        let state = tempfile::tempdir().unwrap();
        let dot = Dotfile::in_state(state.path());
        let r = Record {
            machine: "web".into(),
            provider: "virtualbox".into(),
            id: "u1".into(),
        };
        dot.write_back(&r).unwrap();
        assert_eq!(dot.recorded().unwrap(), std::slice::from_ref(&r));
        dot.forget("web", "virtualbox").unwrap();
        assert_eq!(dot.recorded().unwrap(), Vec::<Record>::new());
        dot.write_back(&r).unwrap();
        dot.reset().unwrap();
        assert!(!dot.path.exists());
        dot.reset().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_machines_folder_that_cant_be_read_is_an_error_never_nothing_recorded() {
        let state = tempfile::tempdir().unwrap();
        let dot = Dotfile::in_state(state.path());
        write(&dot.path, "machines", "a file, not a folder");
        assert!(dot.recorded().is_err());
    }
}
