//! Starting a run: the occupied-slot rules, the start steps with their recovery retry, the ready
//! conditions, then outputs and the message.

use isoloom_manifest::Recovery;
use serde_json::json;

use super::Running;
use super::operation::Done;
use crate::Result;
use crate::driver::Observed;
use crate::error::Error;
use crate::events::WarningCode;
use crate::events::{Emitter, Events};
use crate::executor::{Cx, Ran, run_steps};
use crate::options::{Occupied, RunOptions};
use crate::params::{params_for_existing, params_for_start};
use crate::secrets::check_secrets;
use crate::state::{Op, RunState};
use crate::status::{OutputValue, Started, any_member_running, any_present};
use crate::store::RunRecord;
use crate::{clock, outputs, ready, tools};

/// What `up` finds where it would start the run.
enum Occupancy {
    /// Nothing of the run exists.
    Free,
    /// Something is left (a failed start, stopped machines): removed before the start.
    Leftovers,
    /// The run is parked, and the embedder resumes parked runs.
    Parked,
}

impl Running<'_> {
    /// Starts the run: refused while it runs or runs elsewhere; a parked run is resumed (with
    /// [`Occupied::ResumeParked`]); leftovers are removed first.
    ///
    /// # Errors
    ///
    /// Fails, after its `error` event, when the run is up already (`already_running`) or recorded
    /// elsewhere (`running_elsewhere`); when a param, secret or tool is missing or refused; when a
    /// host port is taken (`port_in_use`); when a step fails or runs out of time (`tool_failed`,
    /// `tool_missing`, `timeout`); when a ready condition doesn't hold (`not_ready`); or when the
    /// run's state can't be locked or written (`locked`, `io`).
    pub async fn up(&self, opts: &RunOptions, events: &dyn Events) -> Result<Started> {
        self.operate(Op::Up, opts, events, async |connected: &RunOptions, e: &Emitter<'_>| {
            self.up_work(connected, e).await
        })
        .await
    }

    async fn up_work(&self, opts: &RunOptions, events: &Emitter<'_>) -> Result<Done<Started>> {
        let scope = self.scope(opts.provider.clone());
        check_secrets(self.manifest(), &opts.secrets, &scope)?;
        let mut record = self.record_or_fresh()?;
        self.refuse_elsewhere(&record, opts.host_label.as_deref())?;
        let params = params_for_start(self.manifest(), &opts.params, &scope)?;
        self.ensure_ssh_key().await?;
        let mut cx = self.acting(self.cx(params, &opts.secrets, events, opts.provider.clone())?)?;
        // Leftovers are removed with the names they were made with.
        Self::bind_record(&mut cx, &record);
        self.relay(&mut cx).await?;
        self.driver.preflight(&cx).await?;
        self.driver.prepare(&cx)?;
        tools::probe(&cx).await?;
        let observed = self.observe(&mut cx).await?;
        match self.occupancy(&observed, &record, opts.occupied)? {
            Occupancy::Parked => return self.resume_parked(opts, events).await,
            Occupancy::Leftovers => self.clear(&mut cx, &record, opts).await?,
            Occupancy::Free => {}
        }
        self.record_start(&mut record, &cx, opts)?;
        // The slot is picked and recorded (the run `starting`) under the home's lock: a start
        // in another process waits, then sees it taken.
        let slots = self.hold_slots().await?;
        self.place(&mut record, opts)?;
        Self::bind_record(&mut cx, &record);
        self.begin(&mut record, Op::Up)?;
        drop(slots);
        crate::executor::state_files::write(&cx, true)?;
        self.converge(&mut cx).await?;
        self.seal(&mut cx, &mut record).await?;
        self.ready(&mut cx, record).await
    }

    /// What a fresh start of `cx`'s run with `opts` keeps in its `record`: the host ports it
    /// pins, the manifest it runs from, its params, cloud, place, provider and start time. Fails
    /// when no free host port is left to pin one to, or the loopback can't be bound to find one.
    fn record_start(&self, record: &mut RunRecord, cx: &Cx<'_>, opts: &RunOptions) -> Result<()> {
        // A driver that forwards host ports it picks itself (Vagrant, locally) pins them now, into
        // the record, so a resume forwards the same ports for the VM's whole life; `bind_record`
        // binds them into `cx` for the start's own forwards and status.
        record.vm_host_ports = self.driver.pin_vm_ports(cx)?;
        // Every VM the start boots, boots with the Vagrantfile of this manifest.
        self.runs_from_this(record);
        record.params = cx.vars.params().clone();
        record.cloud = self.manifest().cloud;
        record.host_label.clone_from(&opts.host_label);
        record.provider.clone_from(&opts.provider);
        record.started_at = Some(clock::rfc3339(self.runner.config.clock.now()));
        Ok(())
    }

    /// `up`'s steps ([`Running::start`]), then what follows them at `up` and `update`: the
    /// guests' secrets (what they wait for once they exist, where no store holds them), the
    /// outputs (a ready condition may name one: `${outputs.ip}`, the run host) and the ready
    /// conditions.
    pub(super) async fn converge(&self, cx: &mut Cx<'_>) -> Result<()> {
        let ran = self.start(cx).await?;
        crate::guest::deliver(cx, None).await?;
        self.read_outputs(cx).await?;
        ready::check(cx, Some(&ran)).await
    }

    /// The end of an operation that leaves the run ready: its outputs and message read
    /// ([`Running::finish`]), and `record` marked ready.
    pub(super) async fn ready(&self, cx: &mut Cx<'_>, mut record: RunRecord) -> Result<Done<Started>> {
        let (outputs, message) = self.finish(cx).await?;
        record.state = RunState::Ready;
        Ok(ready_run(record, outputs, message))
    }

    /// Resumes the parked run an `up` found, as an operation on the run that exists: its tools
    /// were checked by the `up`.
    async fn resume_parked(&self, opts: &RunOptions, events: &Emitter<'_>) -> Result<Done<Started>> {
        let (record, mut cx) = self.existing(opts, events).await?;
        self.resume_work(record, &mut cx).await
    }

    /// Fails when the run's record puts it on another place than `asked`.
    pub(crate) fn refuse_elsewhere(&self, record: &RunRecord, asked: Option<&str>) -> Result<()> {
        match (record.host_label.as_deref(), asked) {
            (Some(recorded), Some(asked)) if recorded != asked && record.state != RunState::Absent => Err(Error::RunningElsewhere {
                run: self.name().to_string(),
                recorded: recorded.into(),
                asked: asked.into(),
            }),
            _ => Ok(()),
        }
    }

    /// What `up` finds of the run (`observed`, `record`) where it would start it, and what
    /// `occupied` makes of it; refused while it runs.
    fn occupancy(&self, observed: &Observed, record: &RunRecord, occupied: Occupied) -> Result<Occupancy> {
        let exists = any_present(&observed.machines);
        let parked = record.state == RunState::Parked;
        let running = any_member_running(&observed.machines);
        let already = || Err(Error::AlreadyRunning { run: self.name().to_string() });
        match occupied {
            _ if !exists => Ok(Occupancy::Free),
            Occupied::Replace => Ok(Occupancy::Leftovers),
            // A start that failed with its machines up: what it left goes, then a fresh start.
            _ if record.state == RunState::Failed => Ok(Occupancy::Leftovers),
            _ if running && !parked => already(),
            Occupied::ResumeParked if parked => Ok(Occupancy::Parked),
            _ => Ok(Occupancy::Leftovers),
        }
    }

    /// Removes what is left of the run: its `down` steps, with the params it was started with
    /// (the region it was applied in, its seal), never the ones of the start to come.
    async fn clear(&self, cx: &mut Cx<'_>, record: &RunRecord, opts: &RunOptions) -> Result<()> {
        cx.events
            .warning(WarningCode::LeftoversRemoved, "removing what an earlier run left before starting");
        let fresh = cx.vars.params().clone();
        if let Some(replayed) = record.replayed_params() {
            cx.vars
                .set_params(params_for_existing(self.manifest(), Some(replayed), &opts.params, &cx.scope)?);
        }
        let cleared = crate::executor::run_down_steps(cx).await.map(drop);
        cx.vars.set_params(fresh);
        cleared
    }

    /// The home's slots, held while this start picks its own, when the run needs one.
    async fn hold_slots(&self) -> Result<Option<crate::store::SlotsLock>> {
        if crate::slot::needed(self.manifest()) {
            return self.runner.store.hold_slots().await.map(Some);
        }
        Ok(None)
    }

    /// Where the run goes on its place, kept in `record`: its slot (when the manifest names
    /// `${run.slot}`), its SSH jump, and its expiry (when the manifest has one).
    fn place(&self, record: &mut RunRecord, opts: &RunOptions) -> Result<()> {
        let place = opts.host_label.as_deref();
        if crate::slot::needed(self.manifest()) {
            let taken = crate::slot::taken(&self.runner.store.records()?, self.name(), place);
            record.slot = Some(crate::slot::pick(record.slot, &taken, place)?);
        }
        record.ssh_jump.clone_from(&opts.ssh_jump);
        record.expires_at = self.manifest().expiry.as_ref().map(|e| {
            let minutes = u64::from(e.default_minutes.unwrap_or(DEFAULT_LIFETIME_MINUTES));
            let life = opts.lifetime.map_or(minutes * 60, |l| l.as_secs());
            clock::unix_seconds(self.runner.config.clock.now()).saturating_add(life)
        });
        Ok(())
    }

    /// The start steps; a failure after a step printed one of `recovery.stale_markers` (any line
    /// of an argv or `ssh.run` step's, see [`crate::executor::LineWatch`]) runs the recovery steps
    /// and starts again, up to `recovery.retries` times. A genuine failure never retries.
    pub(super) async fn start(&self, cx: &mut Cx<'_>) -> Result<Ran> {
        let up = self.manifest().lifecycle.up.as_slice();
        let none = Recovery {
            stale_markers: Vec::new(),
            steps: Vec::new(),
            retries: isoloom_manifest::Retries::default(),
            apply_retries: None,
        };
        let recovery = self.manifest().recovery.as_ref().unwrap_or(&none);
        let mut left = recovery.retries.get();
        let mut applies = recovery.apply_retries.map_or(0, isoloom_manifest::Retries::get);
        loop {
            cx.stale.take();
            match crate::executor::run_up_steps(cx, up).await {
                Err(e) if left > 0 && !cx.stale.take().is_empty() => {
                    left -= 1;
                    cx.events
                        .warning(WarningCode::Recovering, &format!("clearing stale state, then starting again: {e}"));
                    run_steps(cx, "recovery.steps", &recovery.steps).await?;
                }
                // A module that converges on a later apply (a cloud's eventual consistency); any
                // other step's failure is the start's.
                Err(e @ Error::ToolFailed { .. }) if applies > 0 && applied(up, &e) => {
                    applies -= 1;
                    cx.events.warning(WarningCode::Retrying, &format!("applying again: {e}"));
                }
                other => return other,
            }
        }
    }

    /// Reads the run's published ports and outputs (a warning for each output whose format
    /// couldn't be filled), and fills its message.
    async fn finish(&self, cx: &mut Cx<'_>) -> Result<(Vec<OutputValue>, Option<String>)> {
        let read = self.read_outputs(cx).await?;
        outputs::emit(cx.events, &read);
        let message = outputs::message(self.manifest(), &cx.vars, &read.values);
        Ok((read.values, message))
    }
}

/// Whether `e` is the failure of a step of `up` that runs `terraform apply`.
fn applied(up: &[isoloom_manifest::Step], e: &Error) -> bool {
    let Error::ToolFailed { step, .. } = e else { return false };
    up.iter().filter(|s| s.id.as_str() == step).any(|s| match &s.run {
        isoloom_manifest::Run::Argv(argv) => argv_applies(argv),
        isoloom_manifest::Run::Action { .. } => false,
    })
}

/// Whether `argv` (on any host) is `terraform apply …` (or `tofu`), global options such as
/// `-chdir=<module>` before the subcommand.
fn argv_applies(argv: &isoloom_manifest::HostArgv) -> bool {
    let applies = |a: &isoloom_manifest::Argv| crate::executor::plan::terraform_subcommand(a).is_some_and(|(_, s)| s == "apply");
    crate::executor::plan::every_argv(argv).any(applies)
}

/// How long a run with an `expiry` lives when neither the embedder nor the manifest says.
const DEFAULT_LIFETIME_MINUTES: u32 = 240;

/// A run that came up, as the operation returns it and its `result` event says.
fn ready_run(record: RunRecord, outputs: Vec<OutputValue>, message: Option<String>) -> Done<Started> {
    let shown: Vec<serde_json::Value> = outputs.iter().map(|o| json!({ "name": o.name, "role": o.role, "value": o.value })).collect();
    let data = json!({ "state": RunState::Ready, "outputs": shown, "message": message });
    Done {
        value: Started { record, outputs, message },
        ok: true,
        data,
        state: Some(RunState::Ready),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(json: &str) -> isoloom_manifest::HostArgv {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn an_apply_is_terraforms_apply_subcommand_wherever_its_module() {
        assert!(argv_applies(&argv(r#"["terraform", "apply", "-auto-approve"]"#)));
        assert!(argv_applies(&argv(
            r#"["terraform", "-chdir=${run.state_dir}/module", "apply", "-auto-approve"]"#
        )));
        assert!(argv_applies(&argv(r#"["tofu", "apply"]"#)));
        assert!(!argv_applies(&argv(r#"["terraform", "destroy", "-auto-approve"]"#)));
        assert!(
            !argv_applies(&argv(r#"["terraform", "-chdir=apply", "init"]"#)),
            "a folder named apply isn't the subcommand"
        );
        assert!(!argv_applies(&argv(r#"["ssh", "apply"]"#)));
        assert!(argv_applies(&argv(r#"{"default": ["terraform", "apply"], "windows": ["terraform", "apply"]}"#)));
    }
}
