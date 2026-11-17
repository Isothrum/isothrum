//! A run's status: what its tool reports now, reconciled with its record; and what its machines
//! use now, as their engine samples it ([`Running::usage`]).

use super::{Emitter, LastObserved, OutputValue, Result, RunOptions, RunState, Running, Seen, Status, ToolCx};
use crate::formats::SeenMachine;
use crate::status::{MachineState, MachineStatus, PublishedPort};

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
        if self.held(recorded)? {
            return Ok(as_last_read(recorded, row.as_ref(), &[]));
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
        let now = row.as_ref().map_or(recorded, |r| r.state);
        let derived = self.unless_kept_failed(derived, now, record.as_ref());
        let derived = unless_only_added(derived, now, &seen.machines, row.as_ref());
        // A machine another process acts on (Vagrant's lock refused the read): the run as last read,
        // that machine busy, nothing reconciled.
        let busy: Vec<&str> = seen
            .machines
            .iter()
            .filter(|m| m.state == MachineState::Busy)
            .map(|m| m.name.as_str())
            .collect();
        if !busy.is_empty() {
            return Ok(as_last_read(recorded, row.as_ref(), &busy));
        }
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
        if opts.reveal {
            status.outputs = status.outputs.into_iter().map(OutputValue::unmasked).collect();
        }
        for (machine, console) in status.machines.iter_mut().zip(screens) {
            machine.console = console;
        }
        status.native = native;
        Ok(status)
    }

    /// `derived`, the state the tool's read gives, unless the run, `now` failed, stays so until an
    /// operation ends well: its tool can't read readiness live (#113), or a start failed it whose
    /// work the tool can't tell finished (Vagrant's: a VM running says nothing of its
    /// provisioning). `record` says how its last operation ended.
    fn unless_kept_failed(&self, derived: RunState, now: RunState, record: Option<&crate::store::RunRecord>) -> RunState {
        let failed_start = record
            .and_then(|r| r.last.as_ref())
            .is_some_and(|last| !last.ok && super::op_goal(last.op) == Some(RunState::Ready));
        if derived == RunState::Ready && now == RunState::Failed && !self.driver.ready_by_observation(failed_start) {
            RunState::Failed
        } else {
            derived
        }
    }

    /// Whether an operation holds the run, so its tool isn't asked (it would contend with it, as
    /// Vagrant's locks do): one of this process, one the record says moves it, or one another live
    /// process runs on it now (an attach, a provision, a retire move no state).
    pub(super) fn held(&self, recorded: RunState) -> Result<bool> {
        if self.runner.op_of(&self.name).is_some() || recorded.is_moving() {
            return Ok(true);
        }
        let running = self.runner.store.db().read(|tx| tx.unfinished(&self.name))?;
        Ok(running.iter().any(|o| o.started_at.is_some() && o.owner.is_alive()))
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

    /// What each running machine of the run uses now, by name, as its engine samples it once:
    /// a Compose project's containers (`docker stats`); none for a format whose tool reports no
    /// per-machine figure (Vagrant, Terraform). A machine not running has none. `opts` gives
    /// what the tools get. Emits nothing and never locks the run.
    ///
    /// # Errors
    ///
    /// As the tool fails: its engine doesn't answer, or its answer can't be read.
    pub async fn usage(&self, opts: &RunOptions) -> Result<Vec<crate::MachineUsage>> {
        let emitter = Emitter::silent(self.name.as_str(), self.runner.config.clock.as_ref());
        self.with_cx(opts, &emitter, self.runner.config.processes.as_ref(), async |cx: &ToolCx<'_>| {
            self.driver.usage(cx).await
        })
        .await
    }
}

/// `derived`, unless the run, `now` ready, reads failed for machines alone that it never had: each
/// absent now, and absent or unlisted at the last read (one its files declare since its last
/// operation, the update that makes it still to come), every other machine as that read saw it.
/// The run stays ready then, so that update isn't refused as `not_running`. A machine removed
/// outside the run (seen there before, absent now), or any other change, fails it as before; a
/// run never read (an operation's own read counts) is as `derived` says.
fn unless_only_added(derived: RunState, now: RunState, machines: &[SeenMachine], row: Option<&crate::store::RunRow>) -> RunState {
    let Some(last) = row.and_then(|r| r.observed.as_ref()) else {
        return derived;
    };
    if derived != RunState::Failed || now != RunState::Ready {
        return derived;
    }
    let was = |name: &str| last.machines.iter().find(|m| m.name == name).map(|m| &m.state);
    let added = |m: &SeenMachine| m.state == MachineState::Absent && was(&m.name).is_none_or(|s| *s == MachineState::Absent);
    let unchanged = |m: &SeenMachine| was(&m.name) == Some(&m.state);
    if machines.iter().any(added) && machines.iter().all(|m| added(m) || unchanged(m)) {
        RunState::Ready
    } else {
        derived
    }
}

/// The run in `state` as last read (not live): its machines as the last read saw them, those of
/// `busy` busy (another process acts on them). A run absent has none.
fn as_last_read(state: RunState, row: Option<&crate::store::RunRow>, busy: &[&str]) -> Status {
    let mut status = status_of(&Seen::default(), state, false, row);
    if state != RunState::Absent {
        status.machines = row.and_then(|r| r.observed.as_ref()).map(|o| o.machines.clone()).unwrap_or_default();
    }
    for name in busy {
        match status.machines.iter_mut().find(|m| m.name == *name) {
            Some(m) => m.state = MachineState::Busy,
            None => status.machines.push(MachineStatus {
                name: (*name).to_owned(),
                state: MachineState::Busy,
                published: Vec::new(),
                address: None,
                console: None,
            }),
        }
    }
    status
}

/// The status of the run in `state` with `seen` as read.
pub(super) fn status_of(seen: &Seen, state: RunState, live: bool, row: Option<&crate::store::RunRow>) -> Status {
    let outputs = seen
        .outputs
        .iter()
        .map(|(name, o)| OutputValue::new(name.clone(), o.sensitive, Some(o.value.clone())))
        .collect();
    Status {
        state,
        live,
        desired: row.map(|r| r.desired),
        failed_cause: row.and_then(|r| r.failed_cause),
        provider: row.and_then(|r| r.record.provider),
        machines: machine_statuses(&seen.machines),
        outputs,
        native: None,
    }
}

/// `machines` as the status reports them. A port whose protocol the tool doesn't say has none,
/// never a guessed one.
pub(super) fn machine_statuses(machines: &[SeenMachine]) -> Vec<MachineStatus> {
    machines
        .iter()
        .map(|m| MachineStatus {
            name: m.name.clone(),
            state: m.state.clone(),
            published: m
                .published
                .iter()
                .map(|p| PublishedPort {
                    port: p.guest,
                    protocol: p.protocol,
                    host_port: p.host,
                })
                .collect(),
            address: m.address.clone(),
            console: None,
        })
        .collect()
}
