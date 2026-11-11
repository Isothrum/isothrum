//! The operations on a run that exists: `down`, `park`, `resume`, `check` and `provision`. Each
//! replays the params the run was started with.

use serde_json::json;

use super::Running;
use super::operation::Done;
use crate::Result;
use crate::checks::{self, CheckReport};
use crate::error::Error;
use crate::events::{Emitter, Events};
use crate::executor::{Cx, run_steps};
use crate::options::RunOptions;
use crate::params::params_for_existing;
use crate::state::{Op, Park, RunState};
use crate::status::Started;
use crate::store::RunRecord;
use crate::{ready, tools};

impl Running<'_> {
    /// Removes everything the run created, through its `down` steps: containers, networks and
    /// volumes, VMs, a module's resources.
    ///
    /// # Errors
    ///
    /// Fails, after its `error` event, when the run is recorded elsewhere, a `down` step fails, or
    /// the run's state can't be locked or written.
    pub async fn down(&self, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        self.operate(Op::Down, opts, events, async |connected: &RunOptions, e: &Emitter<'_>| {
            let (mut record, mut cx) = self.existing(connected, e).await?;
            self.begin(&mut record, Op::Down)?;
            crate::executor::run_down_steps(&mut cx).await?;
            self.driver.after_down(&cx)?;
            crate::executor::state_files::forget(&cx)?;
            crate::ssh::forget(&cx)?;
            self.forget_seal()?;
            crate::archive::forget(&cx)?;
            // A status while the next start holds the run shows no machine of this one.
            self.runner.store.forget_seen(self.name())?;
            Ok(Done {
                value: (),
                ok: true,
                data: json!({ "state": RunState::Absent }),
                state: Some(RunState::Absent),
            })
        })
        .await
    }

    /// Stops the run, keeping what it created, with the manifest's `park` steps for `mode`; `opts`
    /// as the other operations take them (the place the run is on, the params and secrets its steps
    /// read). Its SSH goes through the jump the run recorded at `up`.
    ///
    /// # Errors
    ///
    /// Fails with `not_parkable` when the manifest has no steps for `mode`, with `not_running` when
    /// nothing of the run is there, and as [`Running::down`] does for a run recorded elsewhere, a
    /// step or the state.
    pub async fn park(&self, mode: Park, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        self.operate(Op::Park, opts, events, async |connected: &RunOptions, e: &Emitter<'_>| {
            let park = self.manifest().lifecycle.park.as_ref();
            let steps = park.and_then(|p| if mode == Park::Pause { p.pause.as_ref() } else { p.shutdown.as_ref() });
            let steps = steps.ok_or_else(|| Error::NotParkable {
                run: self.name().to_string(),
                mode: mode.as_str(),
            })?;
            let (mut record, mut cx) = self.existing(connected, e).await?;
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
        self.operate(Op::Resume, opts, events, async |connected: &RunOptions, e: &Emitter<'_>| {
            let (record, mut cx) = self.existing(connected, e).await?;
            tools::probe(&cx).await?;
            self.resume_work(record, &mut cx).await
        })
        .await
    }

    /// Resumes the run of `record` in `cx` (the context of an operation on the run that exists,
    /// [`Running::existing`]), its tools already checked.
    pub(crate) async fn resume_work(&self, mut record: RunRecord, cx: &mut Cx<'_>) -> Result<Done<Started>> {
        if !self.exists(cx).await? {
            // Nothing left: the parked mark goes, and a fresh start is the way back.
            record.state = RunState::Absent;
            self.runner.store.save(&record)?;
            return Err(Error::NothingToResume { run: self.name().to_string() });
        }
        self.begin(&mut record, Op::Resume)?;
        run_steps(cx, "lifecycle.resume", &self.manifest().lifecycle.resume).await?;
        ready::check(cx, None).await?;
        self.ready(cx, record).await
    }

    /// Runs the run's checks against the running run.
    ///
    /// # Errors
    ///
    /// Fails with `not_running` when no member machine runs, and when a setup or teardown step
    /// fails. A failing check isn't an error: the report says it (`ok: false`).
    pub async fn check(&self, opts: &RunOptions, events: &dyn Events) -> Result<CheckReport> {
        self.operate(Op::Check, opts, events, async |connected: &RunOptions, e: &Emitter<'_>| {
            let (_, mut cx) = self.existing(connected, e).await?;
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
    /// member machine runs, `unsupported` when the manifest has no provision steps, and as
    /// [`Running::up`] does for a step or the state.
    pub async fn provision(&self, machine: Option<&str>, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        self.operate(Op::Provision, opts, events, async |connected: &RunOptions, e: &Emitter<'_>| {
            let provision = self.manifest().lifecycle.provision.as_ref().ok_or_else(|| Error::Unsupported {
                what: "provisioning this run (its manifest has no provision steps)".into(),
            })?;
            let (_, mut cx) = self.existing(connected, e).await?;
            if let Some(m) = machine {
                // Only a member machine is provisioned: another (the run's router, its checks)
                // is as unknown here as a name the run doesn't have.
                if self.machine_named(m)?.role != isoloom_manifest::Role::Member {
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
        let provider = record.provider.or(opts.provider);
        let params = params_for_existing(self.manifest(), record.replayed_params(), &opts.params, &self.scope(provider))?;
        // An adopted run ([`crate::Runner::adopt`]), or one acted on from another home, may
        // name a key this runner hasn't made yet.
        self.ensure_ssh_key().await?;
        let mut cx = self.acting(self.cx(params, &opts.secrets, events, provider)?)?;
        Self::bind_record(&mut cx, &record);
        self.driver.prepare(&cx)?;
        crate::graphical::chosen::ensure(&cx)?;
        crate::executor::state_files::write(&cx, false)?;
        self.relay(&mut cx).await?;
        Ok((record, cx))
    }

    /// Whether anything of the run is there.
    async fn exists(&self, cx: &mut Cx<'_>) -> Result<bool> {
        Ok(crate::status::any_present(&self.observe(cx).await?.machines))
    }
}
