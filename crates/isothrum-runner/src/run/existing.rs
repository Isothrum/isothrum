//! The operations on a run that exists: `down`, `park`, `resume`, `check` and `provision`. Each
//! replays the params the run was started with.

use serde_json::json;

use super::Running;
use super::operation::Done;
use super::up::ready_run;
use crate::Result;
use crate::checks::{self, CheckReport};
use crate::error::Error;
use crate::events::{Emitter, Events};
use crate::executor::{Cx, run_steps};
use crate::options::RunOptions;
use crate::params::params_for_existing;
use crate::secrets::sensitive_values;
use crate::state::{Op, Park, RunState};
use crate::status::{MachineState, Started};
use crate::store::RunRecord;
use crate::{ready, tools};

impl Running<'_> {
    /// Removes everything the run created (containers, networks, volumes).
    ///
    /// # Errors
    ///
    /// Fails, after its `error` event, when the run is recorded elsewhere, a `down` step fails, or
    /// the run's state can't be locked or written.
    pub async fn down(&self, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        let opts = &self.connected(opts);
        let mask = sensitive_values(self.manifest(), &opts.secrets);
        self.operate(Op::Down, events, mask, async |e: &Emitter<'_>| {
            let (mut record, mut cx) = self.existing(opts, e).await?;
            self.begin(&mut record, Op::Down)?;
            run_steps(&mut cx, "lifecycle.down", self.manifest().lifecycle.down.as_slice()).await?;
            self.driver.after_down(&cx)?;
            crate::executor::state_files::forget(&cx)?;
            self.forget_seal()?;
            crate::archive::forget(&cx)?;
            Ok(Done {
                value: (),
                ok: true,
                data: json!({ "state": RunState::Absent }),
                state: Some(RunState::Absent),
            })
        })
        .await
    }

    /// Stops the run, keeping what it created, with the manifest's `park` steps for `mode`.
    ///
    /// # Errors
    ///
    /// Fails with `not_parkable` when the manifest has no steps for `mode`, with `not_running` when
    /// nothing of the run is there, and as [`Running::down`] does for a step or the state.
    pub async fn park(&self, mode: Park, events: &dyn Events) -> Result<()> {
        let opts = self.connected(&RunOptions::default());
        let mask = sensitive_values(self.manifest(), &opts.secrets);
        self.operate(Op::Park, events, mask, async |e: &Emitter<'_>| {
            let park = self.manifest().lifecycle.park.as_ref();
            let steps = park.and_then(|p| if mode == Park::Pause { p.pause.as_ref() } else { p.shutdown.as_ref() });
            let steps = steps.ok_or_else(|| Error::NotParkable {
                run: self.name().to_string(),
                mode: mode.as_str(),
            })?;
            let (mut record, mut cx) = self.existing(&opts, e).await?;
            if !self.exists(&mut cx).await? {
                return Err(Error::NotRunning { run: self.name().to_string() });
            }
            self.begin(&mut record, Op::Park)?;
            run_steps(&mut cx, "lifecycle.park", steps).await?;
            Ok(Done {
                value: (),
                ok: true,
                data: json!({ "state": RunState::Parked }),
                state: Some(RunState::Parked),
            })
        })
        .await
    }

    /// Starts a parked run again, as it was: nothing is re-created, so inputs aren't needed again.
    ///
    /// # Errors
    ///
    /// Fails with `nothing_to_resume` when nothing is left (the parked mark is dropped), and as
    /// [`Running::up`] does for a tool, a step, a ready condition or the state.
    pub async fn resume(&self, opts: &RunOptions, events: &dyn Events) -> Result<Started> {
        let opts = &self.connected(opts);
        let mask = sensitive_values(self.manifest(), &opts.secrets);
        self.operate(Op::Resume, events, mask, async |e: &Emitter<'_>| self.resume_work(opts, e, true).await)
            .await
    }

    /// Resumes the run; `probe`: its tools are checked first (an `up` that resumes did it).
    pub(crate) async fn resume_work(&self, opts: &RunOptions, events: &Emitter<'_>, probe: bool) -> Result<Done<Started>> {
        let (mut record, mut cx) = self.existing(opts, events).await?;
        if probe {
            tools::probe(&cx).await?;
        }
        if !self.exists(&mut cx).await? {
            // Nothing left: the parked mark goes, and a fresh start is the way back.
            record.state = RunState::Absent;
            self.runner.store.save(&record)?;
            return Err(Error::NothingToResume { run: self.name().to_string() });
        }
        self.begin(&mut record, Op::Resume)?;
        run_steps(&mut cx, "lifecycle.resume", &self.manifest().lifecycle.resume).await?;
        ready::check(&mut cx, None).await?;
        let (outputs, message) = self.finish(&mut cx).await?;
        record.state = RunState::Ready;
        Ok(ready_run(record, outputs, message))
    }

    /// Runs the run's checks against the running run.
    ///
    /// # Errors
    ///
    /// Fails with `not_running` when no target machine runs, and when a setup or teardown step
    /// fails. A failing check isn't an error: the report says it (`ok: false`).
    pub async fn check(&self, opts: &RunOptions, events: &dyn Events) -> Result<CheckReport> {
        let opts = &self.connected(opts);
        let mask = sensitive_values(self.manifest(), &opts.secrets);
        self.operate(Op::Check, events, mask, async |e: &Emitter<'_>| {
            let (_, mut cx) = self.existing(opts, e).await?;
            let observed = self.observe_running(&mut cx).await?;
            // A runner's step may name an output (`${outputs.ip}`, the router it goes through).
            self.bind_observed(&mut cx, observed);
            let report = checks::run(&mut cx).await?;
            let data = json!({ "passed": report.results.iter().filter(|r| r.ok).count(), "failed": report.results.iter().filter(|r| !r.ok).count(), "unfinished": report.unfinished });
            Ok(Done { ok: report.ok, value: report, data, state: None })
        })
        .await
    }

    /// Runs the run's set-up again on its running machines, every one or `machine` (a run machine
    /// of the manifest), without rebuilding them: the manifest's `provision` steps. The values
    /// given at launch aren't kept by the runner: pass them again in `opts`.
    ///
    /// # Errors
    ///
    /// Fails with `machine_unknown` for a machine the run doesn't have, `not_running` when no
    /// target machine runs, `unsupported` when the manifest has no provision steps, and as
    /// [`Running::up`] does for a step or the state.
    pub async fn provision(&self, machine: Option<&str>, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        let opts = &self.connected(opts);
        let mask = sensitive_values(self.manifest(), &opts.secrets);
        self.operate(Op::Provision, events, mask, async |e: &Emitter<'_>| {
            let provision = self.manifest().lifecycle.provision.as_ref().ok_or_else(|| Error::Unsupported {
                what: "provisioning this run (its manifest has no provision steps)".into(),
            })?;
            let (_, mut cx) = self.existing(opts, e).await?;
            if let Some(m) = machine {
                // Only a target machine is provisioned: another (the run's router, its checks)
                // is as unknown here as a name the run doesn't have.
                if self.machine_named(m)?.role != isoloom_manifest::Role::Target {
                    return Err(Error::MachineUnknown {
                        run: self.name().to_string(),
                        machine: m.to_owned(),
                    });
                }
                cx.vars.set_machine(m);
            }
            self.observe_running(&mut cx).await?;
            let (list, steps) = match machine {
                Some(_) => ("lifecycle.provision.machine", &provision.machine),
                None => ("lifecycle.provision.all", &provision.all),
            };
            run_steps(&mut cx, list, steps).await?;
            Ok(Done {
                value: (),
                ok: true,
                data: json!({ "machine": machine }),
                state: None,
            })
        })
        .await
    }

    /// The record and the context of an operation on a run that exists: refused when the run is
    /// recorded elsewhere, its recorded params replayed.
    pub(crate) async fn existing<'a>(&'a self, opts: &'a RunOptions, events: &'a Emitter<'a>) -> Result<(RunRecord, Cx<'a>)> {
        let record = self.record_or_fresh()?;
        self.refuse_elsewhere(&record, opts.host_label.as_deref())?;
        let recorded = (record.started_at.is_some()).then_some(&record.params);
        let provider = record.provider.clone().or_else(|| opts.provider.clone());
        let params = params_for_existing(self.manifest(), recorded, &opts.params, &self.scope(provider.clone()))?;
        // A run taken over from an embedder's old runtime, or acted on from another home, may
        // name a key this runner hasn't made yet.
        if crate::ssh::needed(self.manifest()) {
            crate::ssh::ensure_key(self.runner.config.processes.as_ref(), &self.runner.config.home).await?;
        }
        let mut cx = self.acting(self.cx(params, &opts.secrets, events, provider)?)?;
        Self::bind_record(&mut cx, &record);
        self.driver.prepare(&cx)?;
        crate::executor::state_files::write(&cx, false)?;
        self.relay(&mut cx).await?;
        Ok((record, cx))
    }

    /// Whether anything of the run is there.
    async fn exists(&self, cx: &mut Cx<'_>) -> Result<bool> {
        Ok(self.observe(cx).await?.machines.iter().any(|m| m.state != MachineState::Absent))
    }
}
