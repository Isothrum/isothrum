//! A run's status: what its tool reports now, reconciled with its record.

use super::{Emitter, LastObserved, OutputValue, Result, RunOptions, RunState, Running, Seen, Status, ToolCx, machine_statuses, status_of};

impl Running<'_> {
    /// The run as its tool reports it now, reconciled with its record: an operation of this
    /// process on the run leaves the machines as last read (not live).
    ///
    /// # Errors
    ///
    /// As the tool fails to answer; the store's.
    pub async fn status(&self) -> Result<Status> {
        self.status_with(&RunOptions::new()).await
    }

    /// [`Running::status`], its tools given `opts`' values and secrets beside the recorded ones:
    /// what a read of the project's own servers needs (a Proxmox VE module's API token, for its
    /// VMs' states on the server).
    ///
    /// # Errors
    ///
    /// As [`Running::status`]; `server_auth_failed` when a server refuses the credentials.
    pub async fn status_with(&self, opts: &RunOptions) -> Result<Status> {
        if self.runner.op_of(&self.name).is_none() {
            self.cut_off_dead_holder()?;
        }
        let record = self.runner.store.record(&self.name)?;
        let recorded = record.as_ref().map_or(RunState::Absent, |r| r.state);
        let row = self.runner.store.db().read(|tx| tx.run(&self.name))?;
        if self.runner.op_of(&self.name).is_some() || recorded.is_moving() {
            return Ok(status_of(&Seen::default(), recorded, false, row.as_ref()));
        }
        let emitter = Emitter::silent(self.name.as_str(), self.runner.config.clock.as_ref());
        let (seen, derived, screens, native) = self
            .with_cx(opts, &emitter, self.runner.config.processes.as_ref(), async |cx: &ToolCx<'_>| {
                let (seen, derived) = self.driver.observe(cx).await?;
                let screens = self.screens(cx, &seen).await;
                let native = self.driver.native_app(cx, &seen);
                Ok((seen, derived, screens, native))
            })
            .await?;
        let observed = LastObserved {
            machines: machine_statuses(&seen.machines),
        };
        let state = if row.is_some() {
            self.runner
                .store
                .db()
                .write(|tx| tx.observe(&self.name, &observed, derived))?
                .map_or(recorded, |t| t.to())
        } else {
            derived
        };
        let settled = self.runner.store.db().read(|tx| tx.run(&self.name))?;
        let mut status = status_of(&seen, state, true, settled.as_ref());
        for (machine, console) in status.machines.iter_mut().zip(screens) {
            machine.console = console;
        }
        status.native = native;
        Ok(status)
    }

    /// A run held moving (`creating`, `parking`, …) by an operation whose process is gone (killed,
    /// crashed, another boot) is cut off: that operation ends `abandoned` and the run moves to
    /// `failed` (`cancelled`), as the next operation on it would at its start, so a status says so
    /// instead of `creating` for good, and the next `up` takes it over. A run an operation of a
    /// live process holds stays as it is.
    fn cut_off_dead_holder(&self) -> Result<()> {
        let name = &self.name;
        let at = crate::clock::rfc3339(self.runner.config.clock.now());
        self.runner.store.db().write(|tx| {
            let Some(row) = tx.run(name)? else { return Ok(()) };
            if !row.state.is_moving() {
                return Ok(());
            }
            let started: Vec<_> = tx.unfinished(name)?.into_iter().filter(|o| o.started_at.is_some()).collect();
            let Some(last) = started.last() else { return Ok(()) };
            if started.iter().any(|o| o.owner.is_alive()) {
                return Ok(());
            }
            for left in &started {
                tx.end_operation(left.id, crate::store::Outcome::Abandoned, Some(crate::ErrorCode::Cancelled))?;
            }
            tx.apply(name, crate::state::Transition::cut_off(row.state, last.id)?)?;
            let mut record = row.record;
            record.last = Some(crate::store::LastOp::ended(last.op, Some(crate::ErrorCode::Cancelled), at));
            tx.save_record(&record, row.desired)
        })
    }

    /// Reads the run's status when its record says `state`, so the record says what its tool
    /// reports now (a parked run whose machines were removed reads absent). A read that fails
    /// leaves the record as it was: the operation then meets the tool's failure itself.
    pub(super) async fn refresh_if(&self, state: RunState, opts: &RunOptions) {
        let recorded = self.runner.store.record(&self.name).ok().flatten().map(|r| r.state);
        if recorded == Some(state)
            && let Err(e) = self.status_with(opts).await
        {
            tracing::debug!(error = %e, "the run's status couldn't be read before its operation");
        }
    }

    /// What the project gives back now (a module's outputs), read from the tool.
    pub(super) async fn outputs(&self, opts: &RunOptions) -> Result<Vec<OutputValue>> {
        let emitter = Emitter::silent(self.name.as_str(), self.runner.config.clock.as_ref());
        let (seen, _) = self
            .with_cx(opts, &emitter, self.runner.config.processes.as_ref(), async |cx: &ToolCx<'_>| {
                self.driver.observe(cx).await
            })
            .await?;
        Ok(status_of(&seen, RunState::Ready, true, None).outputs)
    }
}
