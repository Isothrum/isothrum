//! Taking over a Terraform deployment made before the runner: an embedder whose old runtime kept
//! its own state (in a folder of its own) hands it to the runner once, so a
//! running cloud run stays destroyable after the switch, by one code path (`down`, the
//! reaper).
//!
//! The deployment's state moves into the run's state folder (`terraform.tfstate`, and its
//! `.backup` when there is one), and the run's record says it is up, with the params it was
//! applied with: a later `down` replays them (the region it was created in, never the
//! embedder's current one). Terraform's data folder stays behind: it names the old state's path
//! as its backend, which a run's `init` would refuse to change without a prompt; `init` makes a
//! fresh one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use isoloom_manifest::{Driver, SetBy};

use crate::Result;
use crate::error::Error;
use crate::manifest::{LoadedManifest, load_manifest};
use crate::name::RunName;
use crate::runner::Runner;
use crate::state::RunState;
use crate::store::RunRecord;

/// The state file a deployment must hold to be taken over.
const STATE: &str = "terraform.tfstate";

/// The files that move, the state first (the only one required).
const MOVED: [&str; 2] = [STATE, "terraform.tfstate.backup"];

/// A deployment the runner takes over ([`Runner::adopt`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Adoption {
    /// The run it becomes.
    pub name: RunName,
    /// The folder holding its Terraform state.
    pub state_from: PathBuf,
    /// The params it was applied with (the region, the account's profile or project): only the
    /// manifest's params the embedder sets are kept.
    pub params: BTreeMap<String, String>,
    /// What the embedder calls the place it is on (the account's id).
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

    /// With the params it was applied with.
    #[must_use]
    pub fn with_params(mut self, params: impl IntoIterator<Item = (String, String)>) -> Adoption {
        self.params.extend(params);
        self
    }

    /// On the place the embedder calls `label`.
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

    /// Applied in slot `slot` of its place.
    #[must_use]
    pub fn with_slot(mut self, slot: u8) -> Adoption {
        self.slot = Some(slot);
        self
    }
}

impl Runner {
    /// Takes over the Terraform deployment `adoption` describes as a run of the manifest at
    /// `manifest` (`pub async fn adopt(&self, manifest: &Path, adoption: Adoption) ->
    /// Result<RunRecord>`): its state moves into the run's state folder, and the run is recorded
    /// as up, with the manifest's params among `adoption.params`. A run whose record an earlier
    /// `down` left absent (its state gone with it) is taken over too, its record replaced. Once
    /// taken over, the old folder holds no state: taking it over again is refused, never a second
    /// copy.
    ///
    /// # Errors
    ///
    /// Fails with `adopt_refused` when the manifest isn't a Terraform one, when the run already
    /// has a record that isn't absent or a state, or when the old folder holds no state; with `locked` while
    /// another operation holds the run; with `manifest_invalid` or `io` when the manifest can't
    /// be read; and with `io` when a file can't be moved or the record written (the state is
    /// put back then).
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
        let record = self.adopted(loaded, adoption)?;
        let moved = move_files(&from, &to)?;
        if let Err(e) = self.store.save(&record) {
            put_back(&moved, &from, &to);
            return Err(e);
        }
        Ok(record)
    }
}

impl Runner {
    /// The record of `adoption`, a run of `loaded`: up, with the manifest's params among its own,
    /// and its slot when the manifest names one.
    fn adopted(&self, loaded: LoadedManifest, adoption: Adoption) -> Result<RunRecord> {
        let manifest = &loaded.manifest;
        let mut record = RunRecord::fresh(adoption.name, loaded.path, manifest.target, manifest.driver);
        record.params = adoption
            .params
            .into_iter()
            .filter(|(k, _)| manifest.params.iter().any(|p| p.name.as_str() == k && p.set_by == SetBy::Embedder))
            .collect();
        if crate::slot::needed(manifest) {
            let place = adoption.host_label.as_deref();
            let taken = crate::slot::taken(&self.store.records()?, &record.name, place);
            record.slot = Some(crate::slot::pick(adoption.slot, &taken, place)?);
        }
        record.cloud = manifest.cloud;
        record.host_label = adoption.host_label;
        record.expires_at = adoption.expires_at;
        // A recorded start is what makes a later operation replay the params.
        record.started_at = Some(adoption.started_at.unwrap_or_else(|| crate::clock::rfc3339(self.config.clock.now())));
        record.state = RunState::Ready;
        Ok(record)
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

/// Moves `from` to `to`: a rename, else (another file system) a copy then a removal.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)?;
    std::fs::remove_file(from)
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
