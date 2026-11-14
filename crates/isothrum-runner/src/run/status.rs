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
        let record = self.runner.store.record(&self.name)?;
        let recorded = record.as_ref().map_or(RunState::Absent, |r| r.state);
        let row = self.runner.store.db().read(|tx| tx.run(&self.name))?;
        if self.runner.op_of(&self.name).is_some() || recorded.is_moving() {
            return Ok(status_of(&Seen::default(), recorded, false, row.as_ref()));
        }
        let emitter = Emitter::silent(self.name.as_str(), self.runner.config.clock.as_ref());
        let opts = RunOptions::new();
        let (seen, derived, screens, native) = self
            .with_cx(&opts, &emitter, self.runner.config.processes.as_ref(), async |cx: &ToolCx<'_>| {
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
