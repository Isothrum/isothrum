//! A module taken down: its `destroy`, made again for what a provider says is gone, and the run's
//! state forgotten once nothing is left.

use super::{BACKUP_FILE, CliError, DATA_DIR, Failure, LOCK_INFO, Module, READ_LIMIT, Result, STATE_FILE, StateAt, Stream, WarningCode, tf};

impl Module<'_, '_> {
    /// `destroy`, with what a provider says is already gone dropped from the state and the
    /// destroy made again, once.
    pub(super) async fn destroy(&self, state: StateAt<'_>) -> Result<()> {
        let errors = match self.apply("destroy", state, tf::Change::Destroy).await {
            Ok(()) => return Ok(()),
            Err(CliError::Failed {
                failure: Failure::Errors(errors),
                ..
            }) => errors,
            Err(e) => return Err(self.error("destroy", e)),
        };
        let held = self.shown(state).await?;
        let mut gone: Vec<String> = errors
            .iter()
            .filter(|d| d.says_missing())
            .filter_map(|d| d.address.clone())
            .filter(|at| held.managed.iter().any(|r| &r.address == at))
            .collect();
        gone.sort();
        gone.dedup();
        if gone.is_empty() {
            return Err(self.error(
                "destroy",
                CliError::Failed {
                    tool: "terraform",
                    failure: Failure::Errors(errors),
                },
            ));
        }
        for at in &gone {
            let why = errors
                .iter()
                .find(|d| d.address.as_deref() == Some(at))
                .map(|d| d.summary.as_str())
                .unwrap_or_default();
            (self.cx.warn)(
                WarningCode::AlreadyGone,
                &self.mask(&format!("`{at}` is already gone ({why}): dropped from the state, then destroyed again")),
            );
        }
        let say = |s: Stream, l: &str| self.say(s, l);
        tf::state_rm(&self.call(READ_LIMIT, &say), state, &gone)
            .await
            .map_err(|e| self.error("state-rm", e))?;
        self.apply("destroy", state, tf::Change::Destroy).await.map_err(|e| self.error("destroy", e))
    }

    /// The run's own state files, after a destroy that ended well: the local state, its backup and
    /// the data folder. A backend's are the backend's, and its data folder stays (a later read of
    /// that backend needs it).
    pub(super) fn forget(&self, state: StateAt<'_>) -> Result<()> {
        if state == StateAt::Backend {
            return Ok(());
        }
        [STATE_FILE, BACKUP_FILE, LOCK_INFO, DATA_DIR]
            .iter()
            .try_for_each(|f| crate::store::remove(&self.cx.state_dir.join(f)))
    }
}
