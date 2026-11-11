//! Adopting an existing Terraform deployment as a run ([`Runner::adopt`]): a deployment applied
//! from a run manifest's module outside the runner (by hand, by another tool, from a home that is
//! gone), whose local state is in a folder of its own, becomes a run of that manifest, which
//! `status`, `update`, `down` and the reaper then act on like any other.
//!
//! The deployment's state moves into the run's state folder (`terraform.tfstate`, and its
//! `.backup` when there is one), and the run's record says it is up, with the params it was
//! applied with: a later `down` replays them (the region it was created in, never the caller's
//! current one). Terraform's data folder stays behind: it names the old state's path as its
//! backend, which a run's `init` would refuse to change without a prompt; `init` makes a fresh
//! one. A state in a remote backend isn't adopted: pull it into a local file first
//! (`terraform state pull > terraform.tfstate`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use isoloom_manifest::{Driver, SetBy};

use crate::Result;
use crate::error::Error;
use crate::manifest::{LoadedManifest, load_manifest};
use crate::name::RunName;
use crate::runner::Runner;
use crate::slot::Asked;
use crate::state::RunState;
use crate::store::RunRecord;

/// The state file a deployment must hold to be taken over.
const STATE: &str = crate::driver::terraform::state::STATE_FILE;

/// The files that move, the state first (the only one required).
const MOVED: [&str; 2] = [STATE, crate::driver::terraform::state::BACKUP_FILE];

/// An existing Terraform deployment to adopt as a run ([`Runner::adopt`]): where its local state
/// is, and what the runner can't read from that state (the params it was applied with, where it
/// is, when it expires).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Adoption {
    /// The run it becomes.
    pub name: RunName,
    /// The folder holding its Terraform state.
    pub state_from: PathBuf,
    /// The params it was applied with (the region, the account's profile or project): only the
    /// manifest's params a caller sets (`set_by: embedder`) are kept.
    pub params: BTreeMap<String, String>,
    /// What the caller calls the place it is on (an account's id), as an operation's
    /// `host_label`.
    pub host_label: Option<String>,
    /// When it stops itself, in seconds since the Unix epoch.
    pub expires_at: Option<u64>,
    /// When it was started (RFC 3339), when known (else the time it is taken over).
    pub started_at: Option<String>,
    /// Its slot on its place (`${run.slot}`), for a manifest that names one: the one it was
    /// applied with. Unset, the place's lowest free slot.
    pub slot: Option<u8>,
}

impl Adoption {
    /// The deployment whose state is in `state_from`, as run `name`: no param, place or expiry.
    #[must_use]
    pub fn new(name: RunName, state_from: impl Into<PathBuf>) -> Adoption {
        Adoption {
            name,
            state_from: state_from.into(),
            params: BTreeMap::new(),
            host_label: None,
            expires_at: None,
            started_at: None,
            slot: None,
        }
    }

    /// With the params it was applied with: a later operation replays them, a param left out
    /// taking its default, and one the manifest requires with none refused.
    #[must_use]
    pub fn with_params(mut self, params: impl IntoIterator<Item = (String, String)>) -> Adoption {
        self.params.extend(params);
        self
    }

    /// On the place the caller calls `label` (an operation's `host_label`).
    #[must_use]
    pub fn with_host_label(mut self, label: impl Into<String>) -> Adoption {
        self.host_label = Some(label.into());
        self
    }

    /// Stopping itself at `unix_seconds`.
    #[must_use]
    pub fn with_expires_at(mut self, unix_seconds: u64) -> Adoption {
        self.expires_at = Some(unix_seconds);
        self
    }

    /// Started at `rfc3339`.
    #[must_use]
    pub fn with_started_at(mut self, rfc3339: impl Into<String>) -> Adoption {
        self.started_at = Some(rfc3339.into());
        self
    }

    /// Applied in slot `slot` of its place (`${run.slot}`, 1 to 99): the run keeps it, as its
    /// resources are named after it; [`Runner::adopt`] refuses one out of range or another run on
    /// the place holds. Without it, the run takes the lowest free one.
    #[must_use]
    pub fn with_slot(mut self, slot: u8) -> Adoption {
        self.slot = Some(slot);
        self
    }
}

impl Runner {
    /// Adopts the existing Terraform deployment `adoption` describes, one applied from the
    /// module of the manifest at `manifest` outside the runner, as a run of that manifest: its
    /// state moves into the run's state folder, and the run is recorded as up, with the
    /// manifest's params among `adoption.params`; `status`, `update`, `down` and the reaper then
    /// act on it like any other run. A run whose record an earlier `down` left absent (its state
    /// gone with it) adopts one too, its record replaced. Once adopted, the old folder holds no
    /// state: adopting it again is refused, never a second copy.
    ///
    /// ```no_run
    /// # async fn adopt(runner: &isothrum_runner::Runner) -> isothrum_runner::Result<()> {
    /// use isothrum_runner::{Adoption, RunName};
    ///
    /// // A deployment applied by hand from `.isoloom/cloud-vm/aws/`, its state in `old/`.
    /// let adoption = Adoption::new(RunName::new("shop")?, "old")
    ///     .with_params([("region".to_owned(), "eu-west-3".to_owned())])
    ///     .with_started_at("2026-10-15T11:00:00Z");
    /// let record = runner.adopt(".isoloom/cloud-vm/aws/run.json".as_ref(), adoption).await?;
    /// // From now on, `runner.open(…)?.down(…)` destroys it as it was applied.
    /// # drop(record);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Fails with `adopt_refused` when the manifest isn't a Terraform one, when the run already
    /// has a record that isn't absent or a state, when the old folder holds no state, or when the
    /// slot it was applied in ([`Adoption::with_slot`]) is out of range or another run on its
    /// place holds it; with `slots_full` when it names no slot and none is free on its place;
    /// with `locked` while another operation holds the run or another process picks a slot past
    /// the wait; with `manifest_invalid` or `io` when the manifest can't be read; and with `io`
    /// when a file can't be moved or the record written (the state is put back then).
    pub async fn adopt(&self, manifest: &Path, adoption: Adoption) -> Result<RunRecord> {
        let loaded = load_manifest(manifest)?;
        let name = adoption.name.clone();
        let refuse = |reason: String| Error::Adopt { run: name.to_string(), reason };
        if loaded.manifest.driver != Driver::Terraform {
            return Err(refuse(format!("its manifest's driver is {}, not terraform", loaded.manifest.driver.id())));
        }
        let to = self.store.run_dir(&name)?;
        let _lock = self.locks.acquire(&name, &self.store.lock_path(&name)).await?;
        // A record an earlier `down` left absent holds nothing to lose: the run is taken over.
        if self.store.record(&name)?.is_some_and(|r| r.state != RunState::Absent) {
            return Err(refuse("the run has a record already".into()));
        }
        if to.join(STATE).exists() {
            return Err(refuse("the run has a Terraform state already".into()));
        }
        if !adoption.state_from.join(STATE).is_file() {
            return Err(refuse(format!("{} holds no {STATE}", adoption.state_from.display())));
        }
        let from = adoption.state_from.clone();
        // The slot is picked and recorded under the home's slots lock, as a start's is.
        let asked = Asked::Applied(adoption.slot);
        let slot = crate::slot::choose(&self.store, &loaded.manifest, &name, adoption.host_label.as_deref(), asked).await?;
        let mut record = self.adopted(loaded, adoption);
        slot.keep_in(&mut record);
        let moved = move_files(&from, &to)?;
        if let Err(e) = self.store.save(&record) {
            put_back(&moved, &from, &to);
            return Err(e);
        }
        drop(slot);
        Ok(record)
    }
}

impl Runner {
    /// The record of `adoption`, a run of `loaded`: up, with the manifest's params among its own;
    /// its slot is kept in it apart ([`crate::slot::choose`]).
    fn adopted(&self, loaded: LoadedManifest, adoption: Adoption) -> RunRecord {
        let manifest = &loaded.manifest;
        let mut record = RunRecord::fresh(adoption.name, loaded.path, manifest.target, manifest.driver);
        record.params = adoption
            .params
            .into_iter()
            .filter(|(k, _)| manifest.params.iter().any(|p| p.name.as_str() == k && p.set_by == SetBy::Embedder))
            .collect();
        record.cloud = manifest.cloud;
        record.host_label = adoption.host_label;
        record.expires_at = adoption.expires_at;
        // A recorded start is what makes a later operation replay the params.
        record.started_at = Some(adoption.started_at.unwrap_or_else(|| crate::clock::rfc3339(self.config.clock.now())));
        record.state = RunState::Ready;
        record
    }
}

/// Moves each of [`MOVED`] that `from` holds into `to`; the names moved. A failure puts back
/// what moved before it.
fn move_files(from: &Path, to: &Path) -> Result<Vec<&'static str>> {
    let mut moved = Vec::new();
    for name in MOVED {
        let source = from.join(name);
        if !source.is_file() {
            continue;
        }
        if let Err(e) = move_file(&source, &to.join(name)) {
            put_back(&moved, from, to);
            return Err(Error::io(&source, "moved")(e));
        }
        moved.push(name);
    }
    Ok(moved)
}

/// Moves `from` to `to`: a rename, else, only when they are on different file systems, a copy
/// then a removal ([`copy_then_remove`]). Any other failure to rename is the error.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => copy_then_remove(from, to),
        renamed => renamed,
    }
}

/// Copies `from` to `to`, then removes `from`; when `from` can't be removed, the copy goes too,
/// so the file is never in both places.
fn copy_then_remove(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::copy(from, to)?;
    std::fs::remove_file(from).inspect_err(|_| {
        if let Err(e) = std::fs::remove_file(to) {
            tracing::warn!(error = %e, file = %to.display(), "a copy of a taken-over state couldn't be removed");
        }
    })
}

/// Moves `names` back from `to` to `from`, as best it can (an error is already on its way).
fn put_back(names: &[&str], from: &Path, to: &Path) {
    for name in names {
        if let Err(e) = move_file(&to.join(name), &from.join(name)) {
            tracing::warn!(error = %e, file = name, "a taken-over state couldn't be put back");
        }
    }
}

#[cfg(test)]
mod tests;
