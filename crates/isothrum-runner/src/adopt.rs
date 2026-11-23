//! Adopting an existing Terraform deployment as a run ([`Runner::adopt`]): a deployment applied
//! from a module outside the runner (by hand, by another tool, from a home that is gone), whose
//! local state is in a folder of its own, becomes a run of that module, which `status`, `update`,
//! `down` and the reaper then act on like any other.
//!
//! The deployment's state moves into the run's state folder (`terraform.tfstate`, and its
//! `.backup` when there is one), and the run's record says it is up. Terraform's data folder stays
//! behind: it names the old state's path as its backend, which a run's `init` would refuse to
//! change without a prompt; `init` makes a fresh one. A state in a remote backend isn't adopted:
//! pull it into a local file first (`terraform state pull > terraform.tfstate`).

use std::path::{Path, PathBuf};

use crate::Result;
use crate::error::Error;
use crate::name::RunName;
use crate::project::Format;
use crate::runner::Runner;
use crate::state::RunState;
use crate::store::RunRecord;

/// An existing Terraform deployment to adopt as a run ([`Runner::adopt`]): where its local state
/// is, and what the runner can't read from that state (where it is, when it expires).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Adoption {
    /// The run it becomes.
    pub name: RunName,
    /// The folder holding its Terraform state.
    pub state_from: PathBuf,
    /// What the caller calls the place it is on (an account's id), as an operation's
    /// `host_label`.
    pub host_label: Option<String>,
    /// When it may be removed, in seconds since the Unix epoch (what the reaper reads).
    pub expires_at: Option<u64>,
    /// When it was started (RFC 3339), when known (else the time it is taken over).
    pub started_at: Option<String>,
}

impl Adoption {
    /// The deployment whose state is in `state_from`, as run `name`: no place or expiry.
    #[must_use]
    pub fn new(name: RunName, state_from: impl Into<PathBuf>) -> Adoption {
        Adoption {
            name,
            state_from: state_from.into(),
            host_label: None,
            expires_at: None,
            started_at: None,
        }
    }

    /// On the place the caller calls `label` (an operation's `host_label`).
    #[must_use]
    pub fn with_host_label(mut self, label: impl Into<String>) -> Adoption {
        self.host_label = Some(label.into());
        self
    }

    /// Expiring at `unix_seconds`.
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
}

impl Runner {
    /// Adopts the existing Terraform deployment `adoption` describes, one applied from the
    /// module in folder `module` outside the runner, as a run of that module: its state moves
    /// into the run's state folder, and the run is recorded as up; `status`, `update`, `down` and
    /// the reaper then act on it like any other run. A run whose record an earlier `down` left
    /// absent adopts one too, its record replaced. Once adopted, the old folder holds no state:
    /// adopting it again is refused, never a second copy.
    ///
    /// # Errors
    ///
    /// Fails with `project_invalid` when `module` isn't a Terraform module; with `adopt_refused`
    /// when the run already has a record that isn't absent or a state, or when the old folder
    /// holds no state; with `locked` while another operation holds the run; and with `io` when a
    /// file can't be moved or the record written (the state is put back then).
    pub async fn adopt(&self, module: &Path, adoption: Adoption) -> Result<RunRecord> {
        let run = self.open_as(module, Format::Terraform, Some(adoption.name.clone()))?;
        let name = adoption.name.clone();
        let refuse = |reason: String| Error::Adopt { run: name.to_string(), reason };
        self.store.run_dir(&name)?;
        let _lock = self.locks.acquire(&name, &self.store.lock_path(&name)).await?;
        // A record an earlier `down` left absent holds nothing to lose: the run is taken over.
        if self.store.record(&name)?.is_some_and(|r| r.state != RunState::Absent) {
            return Err(refuse("the run has a record already".into()));
        }
        let moved = run.take_state(&adoption.state_from)?;
        let mut record = RunRecord::fresh(name.clone(), run.project().dir.clone(), Format::Terraform);
        record.host_label = adoption.host_label;
        record.expires_at = adoption.expires_at;
        record.started_at = Some(adoption.started_at.unwrap_or_else(|| crate::clock::rfc3339(self.config.clock.now())));
        record.state = RunState::Ready;
        if let Err(e) = self.store.save_adopted(&record) {
            run.give_state_back(&adoption.state_from, &moved);
            return Err(e);
        }
        Ok(record)
    }
}
