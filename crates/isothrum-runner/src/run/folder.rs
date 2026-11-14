//! A run of a project folder: any Compose project, Vagrantfile or Terraform module, driven by its
//! format's driver ([`crate::formats`]), with no run manifest. Each operation is journaled, holds
//! the run's lock, moves the run's state, records the resources the tool reports and says it all
//! as events, the same way for every format; only the driver knows its tool.
//!
//! It runs beside the manifest-driven [`crate::Running`] until every format's driver is wired,
//! then takes its place.

mod operate;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;

use crate::Result;
use crate::console::ShellCommand;
use crate::error::Error;
use crate::events::{Emitter, Events, ResultData, Stream, WarningCode};
use crate::formats::any::AnyFormat;
use crate::formats::{Seen, SeenMachine, ToolCx, ledger};
use crate::name::RunName;
use crate::options::RunOptions;
use crate::process::ProcessRunner;
use crate::project::{Format, Project};
use crate::runner::Runner;
use crate::state::{Op, Park, RunState};
use crate::status::{MachineStatus, OutputValue, PublishedPort, Started, Status};
use crate::store::{LastObserved, OperationId};

use super::operation::Done;

/// What a command run in a machine of the run said ([`FolderRun::exec`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExecReport {
    /// The machine it ran in.
    pub machine: String,
    /// Its exit status (`-1` when it was killed by a signal).
    pub exit: i32,
    /// Its standard output.
    pub stdout: String,
    /// The end of its standard error.
    pub stderr: String,
}

/// A run of a project folder.
pub struct FolderRun<'r> {
    runner: &'r Runner,
    project: Project,
    name: RunName,
    driver: AnyFormat,
}

impl<'r> FolderRun<'r> {
    /// Run `name` of `project` on `runner`.
    pub(crate) fn new(runner: &'r Runner, project: Project, name: RunName) -> Result<FolderRun<'r>> {
        let driver = AnyFormat::of(project.format)?;
        Ok(FolderRun { runner, project, name, driver })
    }

    /// The run's name.
    #[must_use]
    pub fn name(&self) -> &RunName {
        &self.name
    }

    /// The project it runs.
    #[must_use]
    pub fn project(&self) -> &Project {
        &self.project
    }

    /// The folder the runner keeps the run's own files in.
    fn state_dir(&self) -> PathBuf {
        self.runner.store.run_dir_path(&self.name)
    }

    /// Every variable the run's tools get: the run's key, then its values and secrets as the
    /// format reads them ([`crate::project::inputs`]).
    fn tool_env(&self, opts: &RunOptions) -> Result<Vec<(String, String)>> {
        let mut env = vec![(crate::RUN_KEY_VAR.to_owned(), self.runner.copy_key(&self.name))];
        env.extend(crate::project::inputs::environment(self.project.format, &opts.env, &opts.secrets)?);
        Ok(env)
    }

    /// The process runner of an operation: its tools recorded in the run's state folder while they
    /// run, so a killed operation's are ended by the next ([`crate::process::ledger`]).
    fn operation_processes(&self) -> Arc<dyn ProcessRunner> {
        let tools = self.state_dir().join(crate::process::ledger::DIR);
        Arc::new(crate::process::Recording::new(
            Arc::clone(&self.runner.config.processes),
            tools,
            self.runner.operation_of(&self.name),
        ))
    }

    /// Runs `work` with a tool context for this run, whose tool lines and warnings go to
    /// `emitter` (the lines as the operation's own step).
    async fn with_cx<T>(
        &self,
        opts: &RunOptions,
        emitter: &Emitter<'_>,
        processes: &dyn ProcessRunner,
        work: impl AsyncFnOnce(&ToolCx<'_>) -> Result<T>,
    ) -> Result<T> {
        let state_dir = self.state_dir();
        let key = self.runner.copy_key(&self.name);
        let step = emitter.operation_name();
        let sink = |stream: Stream, line: &str| emitter.log(step, stream, line);
        let warn = |code: WarningCode, message: &str| emitter.warning(code, message);
        let cx = ToolCx {
            project: &self.project,
            run: &self.name,
            key: &key,
            state_dir: &state_dir,
            provider: opts.provider.map(Into::into),
            env: self.tool_env(opts)?,
            processes,
            sink: &sink,
            warn: &warn,
        };
        work(&cx).await
    }

    /// Records what the tool now reports of the run's resources against the store's rows, and
    /// says each row that moved.
    async fn record_found(&self, cx: &ToolCx<'_>, emitter: &Emitter<'_>, by: OperationId) -> Result<()> {
        let found = self.driver.found(cx).await?;
        let db = self.runner.store.db();
        let recorded = db.write(|tx| ledger::record_found(tx, &self.name, by, &found))?;
        let moved: Vec<_> = recorded.made.iter().chain(&recorded.gone).copied().collect();
        crate::run::recovery::say(emitter, db, &moved);
        Ok(())
    }

    /// Removes, by recorded id, what dead operations of the run left, before this one acts.
    async fn recover(&self, cx: &ToolCx<'_>, emitter: &Emitter<'_>, by: OperationId) -> Result<()> {
        let db = self.runner.store.db();
        let rows = db.read(|tx| tx.left_by_dead_operations(&self.name, by))?;
        if rows.is_empty() {
            return Ok(());
        }
        let left = crate::run::recovery::destroy_recorded(db, emitter, by, &rows, async |row| {
            let id = row.native_id.as_deref().unwrap_or_default();
            self.driver.destroy_by_id(cx, &row.key, row.kind, id).await
        })
        .await?;
        for not in &left {
            emitter.warning(
                WarningCode::CleanupFailed,
                &format!("`{}` an earlier operation left wasn't removed: {}", not.row.key, not.error),
            );
        }
        Ok(())
    }

    /// Runs `op` as an operation whose work drives the tool with `act`, then records the resources
    /// the tool reports and reads where the run stands.
    async fn drive(&self, op: Op, opts: &RunOptions, events: &dyn Events, act: impl AsyncFnOnce(&ToolCx<'_>) -> Result<()>) -> Result<RunState> {
        self.operate(op, opts, events, async |given: &RunOptions, emitter: &Emitter<'_>, by: OperationId| {
            let processes = self.operation_processes();
            self.with_cx(given, emitter, processes.as_ref(), async |cx: &ToolCx<'_>| {
                self.recover(cx, emitter, by).await?;
                self.driver.probe(cx).await?;
                act(cx).await?;
                self.record_found(cx, emitter, by).await?;
                let (_, state) = self.driver.observe(cx).await?;
                Ok(state)
            })
            .await
            .map(|state| {
                let goal = op_goal(op).unwrap_or(state);
                Done::settled(goal, op_goal(op), ResultData::Moved { state: goal })
            })
        })
        .await
    }

    /// Starts the run: makes and starts everything, and returns once the project says it's ready.
    ///
    /// # Errors
    ///
    /// Fails, after its `error` event, as the run's tool does (missing, too old, failing, out of
    /// time), with `port_in_use` for a host port taken, or as the run's state refuses an `up`.
    pub async fn up(&self, opts: &RunOptions, events: &dyn Events) -> Result<Started> {
        self.drive(Op::Up, opts, events, async |cx: &ToolCx<'_>| self.driver.up(cx).await).await?;
        let record = self
            .runner
            .store
            .record(&self.name)?
            .ok_or_else(|| Error::NotRunning { run: self.name.to_string() })?;
        let outputs = self.outputs(opts).await.unwrap_or_default();
        Ok(Started {
            record,
            outputs,
            message: None,
        })
    }

    /// Removes everything the run made.
    ///
    /// # Errors
    ///
    /// As [`FolderRun::up`]'s tool failures.
    pub async fn down(&self, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        self.operate(Op::Down, opts, events, async |given: &RunOptions, emitter: &Emitter<'_>, by: OperationId| {
            let processes = self.operation_processes();
            self.with_cx(given, emitter, processes.as_ref(), async |cx: &ToolCx<'_>| {
                self.driver.probe(cx).await?;
                self.driver.down(cx).await?;
                // What the tool's own down left that the run made, by recorded id.
                self.record_found(cx, emitter, by).await?;
                let db = self.runner.store.db();
                let left = db.read(|tx| ledger::left_to_remove(tx, &self.name))?;
                crate::run::recovery::destroy_recorded(db, emitter, by, &left, async |row| {
                    let id = row.native_id.as_deref().unwrap_or_default();
                    self.driver.destroy_by_id(cx, &row.key, row.kind, id).await
                })
                .await?;
                Ok(())
            })
            .await
            .map(|()| Done::moved(RunState::Absent))
        })
        .await
    }

    /// Stops the machines and keeps them, as `mode` says.
    ///
    /// # Errors
    ///
    /// As the tool fails, or as the run's state refuses a park.
    pub async fn park(&self, mode: Park, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        self.drive(Op::Park, opts, events, async |cx: &ToolCx<'_>| self.driver.park(cx, mode).await)
            .await
            .map(drop)
    }

    /// Starts the parked machines again.
    ///
    /// # Errors
    ///
    /// As the tool fails, or as the run's state refuses a resume.
    pub async fn resume(&self, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        self.drive(Op::Resume, opts, events, async |cx: &ToolCx<'_>| self.driver.resume(cx).await)
            .await
            .map(drop)
    }

    /// Brings the running run to what the project's files now say.
    ///
    /// # Errors
    ///
    /// As the tool fails, or as the run's state refuses an update.
    pub async fn update(&self, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        self.drive(Op::Update, opts, events, async |cx: &ToolCx<'_>| self.driver.update(cx).await)
            .await
            .map(drop)
    }

    /// Starts `machine`, one the project leaves down at `up`.
    ///
    /// # Errors
    ///
    /// As the tool fails; `unsupported` where the format has no such machine.
    pub async fn attach(&self, machine: &str, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        self.drive(Op::Attach, opts, events, async |cx: &ToolCx<'_>| self.driver.attach(cx, machine).await)
            .await
            .map(drop)
    }

    /// Removes `machine` from the running run.
    ///
    /// # Errors
    ///
    /// As the tool fails.
    pub async fn retire(&self, machine: &str, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        self.drive(Op::Retire, opts, events, async |cx: &ToolCx<'_>| self.driver.retire(cx, machine).await)
            .await
            .map(drop)
    }

    /// Runs `argv` in `machine`, with no terminal: what it said and how it ended. A command that
    /// runs and fails is a report, not an error.
    ///
    /// # Errors
    ///
    /// When the command can't be run at all (the tool missing, the machine refused).
    pub async fn exec(&self, machine: &str, argv: &[String], opts: &RunOptions, events: &dyn Events) -> Result<ExecReport> {
        self.operate(Op::Check, opts, events, async |given: &RunOptions, emitter: &Emitter<'_>, _by: OperationId| {
            let processes = self.operation_processes();
            let ran = self
                .with_cx(given, emitter, processes.as_ref(), async |cx: &ToolCx<'_>| {
                    self.driver.exec(cx, machine, argv).await
                })
                .await?;
            let report = ExecReport {
                machine: machine.to_owned(),
                exit: ran.exit,
                stdout: ran.stdout,
                stderr: ran.stderr,
            };
            let data = ResultData::Machine {
                machine: Some(machine.to_owned()),
            };
            Ok(Done::checked(report.exit == 0, report, data))
        })
        .await
    }

    /// The command that opens an interactive console into `machine`, as `user` when given. A
    /// query: nothing runs.
    ///
    /// # Errors
    ///
    /// Where the format has no way in.
    pub fn console_command(&self, machine: &str, user: Option<&str>) -> Result<ShellCommand> {
        let user = user.map(crate::console::check_name).transpose()?;
        let state_dir = self.state_dir();
        let key = self.runner.copy_key(&self.name);
        let quiet = |_: Stream, _: &str| {};
        let unwarned = |_: WarningCode, _: &str| {};
        let opts = RunOptions::new();
        let cx = ToolCx {
            project: &self.project,
            run: &self.name,
            key: &key,
            state_dir: &state_dir,
            provider: None,
            env: self.tool_env(&opts)?,
            processes: self.runner.config.processes.as_ref(),
            sink: &quiet,
            warn: &unwarned,
        };
        self.driver.console(&cx, machine, user)
    }

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
        let (seen, derived) = self
            .with_cx(&opts, &emitter, self.runner.config.processes.as_ref(), async |cx: &ToolCx<'_>| {
                self.driver.observe(cx).await
            })
            .await?;
        let observed = LastObserved {
            machines: machine_statuses(&seen.machines),
            published: Vec::new(),
            hosts: std::collections::BTreeMap::new(),
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
        Ok(status_of(&seen, state, true, settled.as_ref()))
    }

    /// What the project gives back now (a module's outputs), read from the tool.
    async fn outputs(&self, opts: &RunOptions) -> Result<Vec<OutputValue>> {
        let emitter = Emitter::silent(self.name.as_str(), self.runner.config.clock.as_ref());
        let (seen, _) = self
            .with_cx(opts, &emitter, self.runner.config.processes.as_ref(), async |cx: &ToolCx<'_>| {
                self.driver.observe(cx).await
            })
            .await?;
        Ok(status_of(&seen, RunState::Ready, true, None).outputs)
    }
}

/// The status of the run in `state` with `seen` as read.
fn status_of(seen: &Seen, state: RunState, live: bool, row: Option<&crate::store::RunRow>) -> Status {
    let outputs = seen
        .outputs
        .iter()
        .map(|(name, o)| OutputValue::new(name.clone(), isoloom_manifest::OutputRole::Info, o.sensitive, Some(o.value.clone())))
        .collect();
    Status {
        state,
        live,
        desired: row.map(|r| r.desired),
        failed_cause: row.and_then(|r| r.failed_cause),
        provider: None,
        machines: machine_statuses(&seen.machines),
        networks: Vec::new(),
        outputs,
        message: None,
        native: None,
    }
}

/// Where `op`, done, leaves the run; none for an op that leaves it where it was.
fn op_goal(op: Op) -> Option<RunState> {
    match op {
        Op::Up | Op::Resume | Op::Update => Some(RunState::Ready),
        Op::Park => Some(RunState::Parked),
        Op::Down => Some(RunState::Absent),
        Op::Check | Op::Provision | Op::Attach | Op::Detach | Op::Retire => None,
    }
}

/// `machines` as the status reports them. A port whose protocol the tool doesn't say is left out
/// of the status, never guessed.
fn machine_statuses(machines: &[SeenMachine]) -> Vec<MachineStatus> {
    machines
        .iter()
        .map(|m| MachineStatus {
            name: m.name.clone(),
            role: isoloom_manifest::Role::Member,
            state: m.state.clone(),
            addresses: std::collections::BTreeMap::new(),
            published: m
                .published
                .iter()
                .filter_map(|p| {
                    p.protocol.map(|protocol| PublishedPort {
                        port: p.guest,
                        protocol,
                        host_port: p.host,
                    })
                })
                .collect(),
            console: None,
        })
        .collect()
}

/// The format a recorded run was started with, read from its record's driver while records still
/// carry the manifest's words.
pub(crate) fn recorded_format(driver: isoloom_manifest::Driver) -> Format {
    match driver {
        isoloom_manifest::Driver::Compose => Format::Compose,
        isoloom_manifest::Driver::Vagrant => Format::Vagrant,
        isoloom_manifest::Driver::Terraform => Format::Terraform,
    }
}
