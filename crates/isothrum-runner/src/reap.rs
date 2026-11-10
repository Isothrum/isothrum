//! The reaper's half in the runner, so an abandoned run can't keep billing:
//!
//! - [`Runner::expired`]: the recorded runs whose expiry passed and that may still hold
//!   something. Destroying one is its `down`, which replays the params it was applied with (the
//!   embedder gives the connection, as for any operation).
//! - [`Runner::orphans`]: what a cloud still holds tagged past its expiry that no recorded run's
//!   state covers, read through the cloud's own command line (its argv, never a shell). An orphan
//!   is only listed, never deleted: no state says what it belongs to, so a person removes it.
//!
//! The sweep's schedule is the embedder's (every few minutes, say; the `isothrum reap` command
//! sweeps once).

mod tags;

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};

use isoloom_manifest::Cloud;
pub use tags::{Place, Tagged};

use crate::Result;
use crate::clock::unix_seconds;
use crate::executor::{Progress, within};
use crate::process::{ProcessRunner, ProcessSpec};
use crate::runner::Runner;
use crate::state::RunState;
use crate::store::RunRecord;

/// How long a cloud's query may go without progress.
const QUERY_LIMIT: Duration = Duration::from_secs(120);
/// The id a query's timeout reads as.
const QUERY_STEP: &str = "reap";

/// What a cloud still holds past its expiry that no recorded run covers.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[non_exhaustive]
pub struct Orphans {
    /// The resources, as the cloud lists them.
    pub resources: Vec<Tagged>,
    /// What couldn't be read: a query whose command line is missing or fails, or whose answer is
    /// of another shape (the cloud wasn't checked there), and a resource no recorded run covers
    /// whose expiry tag isn't a time (whether it is past its expiry is unknown).
    pub unread: Vec<String>,
}

impl Orphans {
    /// Adds what of `found`, a query's answer, no recorded run covers (`covered`): a resource
    /// past its expiry at `now`, and one whose expiry can't be read.
    fn sort(&mut self, found: Vec<tags::Found>, now: u64, covered: &BTreeSet<String>) {
        for item in found {
            match item {
                Ok(t) if orphaned(&t, now, covered) => self.resources.push(t),
                Err(e) if !is_covered(e.instance.as_ref(), covered) => self.unread.push(e.to_string()),
                Ok(_) | Err(_) => {}
            }
        }
    }
}

impl Runner {
    /// The recorded runs whose expiry passed at `now` and whose last operation didn't leave them
    /// absent: the reaper's input. Reads files only.
    ///
    /// # Errors
    ///
    /// Fails with `io` when a record can't be read.
    pub fn expired(&self, now: SystemTime) -> Result<Vec<RunRecord>> {
        let now = unix_seconds(now);
        let records = self.store.records()?;
        Ok(records
            .into_iter()
            .filter(|r| r.state != RunState::Absent && r.expires_at.is_some_and(|at| at <= now))
            .collect())
    }

    /// What `cloud` holds in `place` tagged with an expiry past `now` that no recorded run's
    /// Terraform state covers. A command line that is missing or fails is said in
    /// [`Orphans::unread`], never an error: the sweep goes on.
    ///
    /// # Errors
    ///
    /// Fails with `io` when a record or a recorded run's Terraform state can't be read or parsed:
    /// what that state holds is unknown, so nothing could be told an orphan.
    pub async fn orphans(&self, cloud: Cloud, place: &Place, now: SystemTime) -> Result<Orphans> {
        let covered = self.covered(cloud)?;
        let now = unix_seconds(now);
        let mut out = Orphans::default();
        for spec in tags::queries(cloud, place) {
            let answer = query(self.config.processes.as_ref(), &spec).await.and_then(|text| tags::parse(cloud, &text));
            match answer {
                Ok(found) => out.sort(found, now, &covered),
                Err(why) => out.unread.push(format!("`{}`: {why}", spec.line())),
            }
        }
        Ok(out)
    }

    /// The environments the states of `cloud`'s recorded runs hold. A run with no state holds
    /// none; a state that exists but can't be read or parsed fails the sweep, since what it holds
    /// would otherwise read as orphans.
    fn covered(&self, cloud: Cloud) -> Result<BTreeSet<String>> {
        let mut out = BTreeSet::new();
        for record in self.store.records()?.into_iter().filter(|r| r.cloud == Some(cloud)) {
            let dir = self.store.run_dir(&record.name)?;
            if let Some(state) = crate::driver::terraform::state::read_json(&dir)? {
                tags::instances_in_state(&state, &mut out);
            }
        }
        Ok(out)
    }
}

/// Whether `t` is past its expiry at `now` and no recorded run covers its environment.
fn orphaned(t: &Tagged, now: u64, covered: &BTreeSet<String>) -> bool {
    t.expires_at.is_some_and(|at| at <= now) && !is_covered(t.instance.as_ref(), covered)
}

/// Whether a recorded run covers `instance`, a resource's environment (none when it doesn't say).
fn is_covered(instance: Option<&String>, covered: &BTreeSet<String>) -> bool {
    instance.is_some_and(|i| covered.contains(i))
}

/// What `spec` prints, when it ran to a success without going [`QUERY_LIMIT`] without progress
/// (as every tool call's limit counts); else why not.
async fn query(processes: &dyn ProcessRunner, spec: &ProcessSpec) -> std::result::Result<String, String> {
    let progress = Progress::new();
    let out = within(QUERY_STEP, QUERY_LIMIT, &progress, progress.capture(processes, spec))
        .await
        .map_err(|e| match e {
            crate::Error::Timeout { timed_out, .. } => timed_out.to_string(),
            other => other.to_string(),
        })?;
    out.output().map_err(|why| format!("failed: {why}"))
}

#[cfg(test)]
mod tests;
