//! A run of a project folder: any Compose project, Vagrantfile or Terraform module, driven by its
//! format's driver ([`crate::formats`]). Each operation is journaled, holds the run's lock, moves
//! the run's state, records the resources the tool reports and says it all as events, the same
//! way for every format; only the driver knows its tool. `operate` is what every operation does
//! around its own work, `recovery` what it does about what dead operations left.

mod graphical;
mod operate;
mod ports;
pub(crate) mod recovery;
mod status;
use status::status_of;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;

use crate::Result;
use crate::console::ShellCommand;
use crate::error::Error;
use crate::events::{Emitter, Events, ResultData, ShownOutput, Stream, WarningCode};
use crate::formats::any::AnyFormat;
use crate::formats::{Seen, ToolCx, ledger};
use crate::name::RunName;
use crate::options::RunOptions;
use crate::process::ProcessRunner;
use crate::project::{Format, Project};
use crate::runner::Runner;
use crate::state::{Op, Park, RunState};
use crate::status::{OutputValue, Started, Status};
use crate::store::{LastObserved, OperationId};

use operate::Done;

/// What a command run in a machine of the run said ([`Running::exec`]).
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

/// What an operation that moves the run asks of its tool ([`Running::drive`]).
#[derive(Debug, Clone, Copy)]
enum Act<'a> {
    /// Make and start everything.
    Up,
    /// Stop the machines and keep them.
    Park(Park),
    /// Start the parked machines again.
    Resume,
    /// Bring the run to the project's files.
    Update,
    /// Run the set-up again, on one machine or all.
    Provision(Option<&'a str>, Option<&'a str>),
    /// Start a machine the project leaves down.
    Attach(&'a str),
    /// Remove a machine.
    Retire(&'a str),
}

impl Act<'_> {
    /// The operation it is.
    fn op(self) -> Op {
        match self {
            Act::Up => Op::Up,
            Act::Park(_) => Op::Park,
            Act::Resume => Op::Resume,
            Act::Update => Op::Update,
            Act::Provision(..) => Op::Provision,
            Act::Attach(_) => Op::Attach,
            Act::Retire(_) => Op::Retire,
        }
    }
}

/// What a tool context that says nothing and picks no port holds of its run: a console's, a
/// state folder's move.
struct Quiet {
    key: String,
    state_dir: PathBuf,
    ssh_key: PathBuf,
}

impl Quiet {
    fn of(run: &Running<'_>) -> Quiet {
        Quiet {
            key: run.runner.copy_key(&run.name),
            state_dir: run.state_dir(),
            ssh_key: crate::ssh::key_path(&run.runner.config.home),
        }
    }

    /// The context of `run` on these paths, its tools given `env`.
    fn cx<'a>(&'a self, run: &'a Running<'_>, env: Vec<(String, String)>) -> ToolCx<'a> {
        ToolCx {
            project: &run.project,
            run: &run.name,
            key: &self.key,
            state_dir: &self.state_dir,
            provider: None,
            env,
            host_env: &run.runner.config.host_env,
            processes: run.runner.config.processes.as_ref(),
            ssh_key: &self.ssh_key,
            ports: &crate::formats::NoPorts,
            sink: &crate::formats::discard,
            warn: &unwarned,
            made: crate::formats::NOTHING_MADE,
        }
    }
}

/// Says nothing of a warning: a quiet context's.
fn unwarned(_: WarningCode, _: &str) {}

/// The host ports run `run` pins, reserved through its runner for the operation.
struct RunPorts<'a> {
    runner: &'a Runner,
    run: &'a RunName,
}

impl crate::formats::PickPorts for RunPorts<'_> {
    fn pick(&self, protocol: crate::project::platform::Protocol) -> std::io::Result<Option<crate::net::HeldPort>> {
        self.runner.pick_reserved(self.run, protocol)
    }

    fn claim(&self, ports: &[(crate::port::HostPort, crate::project::platform::Protocol)]) -> Result<()> {
        self.runner.claim_ports(self.run, ports.iter().copied())
    }
}

/// Removes a recorded resource through the run's driver, by the id its tool gave.
struct ById<'a, 'c> {
    driver: AnyFormat,
    cx: &'a ToolCx<'c>,
}

impl recovery::Remover for ById<'_, '_> {
    async fn remove(&mut self, row: &crate::store::ResourceRow) -> Result<()> {
        let id = row.native_id.as_deref().unwrap_or_default();
        self.driver.destroy_by_id(self.cx, &row.key, row.kind, id).await
    }
}

/// A run of a project folder.
pub struct Running<'r> {
    runner: &'r Runner,
    project: Project,
    name: RunName,
    driver: AnyFormat,
}

impl<'r> Running<'r> {
    /// Run `name` of `project` on `runner`.
    pub(crate) fn new(runner: &'r Runner, project: Project, name: RunName) -> Running<'r> {
        let driver = AnyFormat::of(project.format);
        Running { runner, project, name, driver }
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
        // The values the run's record keeps, under those the caller gives now.
        let mut values = self.runner.store.record(&self.name)?.map(|r| r.env).unwrap_or_default();
        values.extend(opts.env.iter().map(|(k, v)| (k.clone(), v.clone())));
        if self.project.format == Format::Terraform {
            // Every run of the runner shares one provider cache, unless the user has their own.
            let cache = crate::formats::terraform::plugin_cache(&self.runner.config);
            std::fs::create_dir_all(&cache).map_err(Error::io(&cache, "create"))?;
            env.push(("TF_PLUGIN_CACHE_DIR".to_owned(), cache.to_string_lossy().into_owned()));
        }
        env.extend(crate::project::inputs::environment(self.project.format, &values, &opts.secrets)?);
        if self.project.format == Format::Vagrant {
            // The ports the run kept, for a Vagrantfile that pins its forwards to them.
            env.extend(ports::env(&ports::kept(&self.state_dir())?));
        }
        Ok(env)
    }

    /// The hypervisor the run takes: the one the caller names, else the one its start recorded (a
    /// `down` names none, and must reach the VMs its start made on their provider).
    fn provider(&self, opts: &RunOptions) -> Option<crate::Provider> {
        opts.provider
            .or_else(|| self.runner.store.record(&self.name).ok().flatten().and_then(|r| r.provider))
    }

    /// The process runner of an operation: its tools recorded in the run's state folder while they
    /// run, so a killed operation's are ended by the next ([`crate::process::ledger`]).
    fn operation_processes(&self, opts: &RunOptions) -> Arc<dyn ProcessRunner> {
        let tools = self.state_dir().join(crate::process::ledger::DIR);
        let recorded: Arc<dyn ProcessRunner> = Arc::new(crate::process::Recording::new(
            Arc::clone(&self.runner.config.processes),
            tools,
            self.runner.operation_of(&self.name),
        ));
        let windows = self.runner.config.host == crate::project::platform::HostOs::Windows;
        if windows && self.driver.elevates(self.provider(opts)) {
            let env = self.runner.config.host_env.for_elevation();
            return Arc::new(crate::process::Elevated::new(recorded, self.state_dir().join("elevated"), env));
        }
        recorded
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
        let ssh_key = crate::ssh::key_path(&self.runner.config.home);
        let step = emitter.operation_name();
        let sink = |stream: Stream, line: &str| emitter.log(step, stream, line);
        let warn = |code: WarningCode, message: &str| emitter.warning(code, message);
        let ports = RunPorts {
            runner: self.runner,
            run: &self.name,
        };
        let made = || self.made();
        let cx = ToolCx {
            project: &self.project,
            run: &self.name,
            key: &key,
            state_dir: &state_dir,
            provider: self.provider(opts),
            env: self.tool_env(opts)?,
            processes,
            ssh_key: &ssh_key,
            ports: &ports,
            host_env: &self.runner.config.host_env,
            sink: &sink,
            warn: &warn,
            made: &made,
        };
        work(&cx).await
    }

    /// The keys of the run's resources the store records as made by its tool: the provider's,
    /// with their id, not gone.
    fn made(&self) -> Result<Vec<String>> {
        let rows = self.runner.store.db().read(|tx| tx.live_resources(&self.name))?;
        Ok(rows
            .into_iter()
            .filter(|r| r.scope == crate::store::Scope::Provider && r.native_id.is_some())
            .map(|r| r.key)
            .collect())
    }

    /// Records what the tool now reports of the run's resources against the store's rows, and
    /// says each row that moved.
    async fn record_found(&self, cx: &ToolCx<'_>, emitter: &Emitter<'_>, by: OperationId) -> Result<()> {
        let found = self.driver.found(cx).await?;
        let db = self.runner.store.db();
        let recorded = db.write(|tx| ledger::record_found(tx, &self.name, by, &found))?;
        let moved: Vec<_> = recorded.made.iter().chain(&recorded.gone).copied().collect();
        crate::run::recovery::say(emitter, db, &moved);
        // A machine made again (destroyed outside, recovered, re-made by an update) is a new VM,
        // maybe at its old address, with new host keys: the pins go, and the next console pins
        // the keys it meets (#110).
        let machines = db.read(|tx| moved.iter().map(|id| tx.resource(*id)).collect::<Result<Vec<_>>>())?;
        if machines.into_iter().flatten().any(|r| r.kind == crate::store::ResourceKind::Vm) {
            crate::ssh::forget(&self.state_dir())?;
        }
        Ok(())
    }

    /// Removes, by recorded id, what dead operations of the run left, before this one acts.
    async fn recover(&self, cx: &ToolCx<'_>, emitter: &Emitter<'_>, by: OperationId) -> Result<()> {
        let db = self.runner.store.db();
        let rows = db.read(|tx| tx.left_by_dead_operations(&self.name, by))?;
        if rows.is_empty() {
            return Ok(());
        }
        let left = recovery::destroy_recorded(db, emitter, by, &rows, ById { driver: self.driver, cx }).await?;
        for not in &left {
            emitter.warning(
                WarningCode::CleanupFailed,
                &format!("`{}` an earlier operation left wasn't removed: {}", not.row.key, not.error),
            );
        }
        Ok(())
    }

    /// Drives the tool as `act` says.
    async fn act(&self, cx: &ToolCx<'_>, act: Act<'_>) -> Result<()> {
        match act {
            Act::Up => self.driver.up(cx).await,
            Act::Park(mode) => self.driver.park(cx, mode).await,
            Act::Resume => self.driver.resume(cx).await,
            Act::Update => {
                self.driver.update(cx).await?;
                if self.project.format == Format::Vagrant {
                    // Compose's --remove-orphans and Terraform's apply take what the files no
                    // longer declare themselves; a service under a profile the run attached isn't
                    // in what Compose's files start, and is no orphan.
                    self.retire_undeclared(cx).await?;
                }
                Ok(())
            }
            Act::Provision(machine, with) => self.driver.provision(cx, machine, with).await,
            Act::Attach(machine) => self.driver.attach(cx, machine).await,
            Act::Retire(machine) => self.driver.retire(cx, machine).await,
        }
    }

    /// After an update: the running machines the project's files no longer declare, brought down
    /// by the driver's re-apply (a Vagrant machine whose `define` left the Vagrantfile, which
    /// `vagrant up` leaves running; Compose's `--remove-orphans` and Terraform's apply leave none).
    async fn retire_undeclared(&self, cx: &ToolCx<'_>) -> Result<()> {
        let declared = self.driver.machines(cx).await?;
        let (seen, _) = self.driver.observe(cx).await?;
        let removed: Vec<String> = seen.machines.into_iter().map(|m| m.name).filter(|m| !declared.contains(m)).collect();
        if removed.is_empty() {
            return Ok(());
        }
        let change = crate::formats::Change {
            removed,
            ..crate::formats::Change::default()
        };
        self.driver.reapply(cx, &change).await
    }

    /// Runs `act` as an operation: what dead operations left removed, the tool probed and driven,
    /// then the resources it reports recorded and where the run stands read.
    async fn drive(&self, act: Act<'_>, opts: &RunOptions, events: &dyn Events) -> Result<RunState> {
        let op = act.op();
        self.operate(op, opts, events, async |given: &RunOptions, emitter: &Emitter<'_>, by: OperationId| {
            let processes = self.operation_processes(given);
            self.with_cx(given, emitter, processes.as_ref(), async |cx: &ToolCx<'_>| {
                self.recover(cx, emitter, by).await?;
                self.driver.probe(cx).await?;
                self.act(cx, act).await?;
                self.record_found(cx, emitter, by).await?;
                let (seen, state) = self.driver.observe(cx).await?;
                operate::not_whole(op, &seen, state)?;
                if matches!(op, Op::Up | Op::Resume | Op::Update) {
                    ports::keep(&self.state_dir(), &seen, cx.warn)?;
                }
                Ok((seen, state))
            })
            .await
            .map(|(seen, state)| {
                let goal = op_goal(op).unwrap_or(state);
                let data = if op == Op::Up {
                    // A start says what the project gives back (a module's outputs), masked.
                    let outputs = status_of(&seen, goal, true, None).outputs;
                    ResultData::Started {
                        state: goal,
                        outputs: outputs.into_iter().map(|o| ShownOutput::new(o.name, o.value)).collect(),
                    }
                } else {
                    ResultData::Moved { state: goal }
                };
                Done::settled(goal, data)
            })
        })
        .await
    }

    /// Starts the run: makes and starts everything, and returns once the project says it's ready.
    ///
    /// # Errors
    ///
    /// Fails, after its `error` event, as the run's tool does (missing, too old, failing, out of
    /// time), with `port_in_use` for a host port taken, or as the run's state refuses an `up`:
    /// `already_running` for a run that is ready and live (its tool says so now), before any tool
    /// acts. A run recorded ready whose machines are gone or stopped meanwhile starts again.
    pub async fn up(&self, opts: &RunOptions, events: &dyn Events) -> Result<Started> {
        self.refresh_if(RunState::Ready, opts).await;
        self.drive(Act::Up, opts, events).await?;
        let record = self
            .runner
            .store
            .record(&self.name)?
            .ok_or_else(|| Error::NotRunning { run: self.name.to_string() })?;
        let outputs = self.outputs(opts).await.unwrap_or_default();
        Ok(Started { record, outputs })
    }

    /// Removes everything the run made, the tunnels of its consoles ended first (whichever
    /// process holds them), and forgets its machines' pinned SSH host keys.
    ///
    /// # Errors
    ///
    /// As [`Running::up`]'s tool failures.
    pub async fn down(&self, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        self.operate(Op::Down, opts, events, async |given: &RunOptions, emitter: &Emitter<'_>, by: OperationId| {
            let processes = self.operation_processes(given);
            self.with_cx(given, emitter, processes.as_ref(), async |cx: &ToolCx<'_>| {
                // The consoles' tunnels to the machines that go, whichever process holds them.
                crate::process::ledger::end_tunnels(&self.state_dir().join(crate::process::ledger::DIR)).await;
                self.driver.probe(cx).await?;
                self.driver.down(cx).await?;
                // The machines' pinned host keys go with them, and the ports they kept.
                crate::ssh::forget(&self.state_dir())?;
                ports::forget(&self.state_dir())?;
                // What the tool's own down left that the run made, by recorded id.
                self.record_found(cx, emitter, by).await?;
                let db = self.runner.store.db();
                let left = db.read(|tx| ledger::left_to_remove(tx, &self.name))?;
                recovery::destroy_recorded(db, emitter, by, &left, ById { driver: self.driver, cx }).await?;
                // A status while the next start holds the run shows none of these machines.
                db.write(|tx| tx.forget_observed(&self.name))?;
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
        self.drive(Act::Park(mode), opts, events).await.map(drop)
    }

    /// Starts the parked machines again.
    ///
    /// # Errors
    ///
    /// As the tool fails, or as the run's state refuses a resume: `nothing_to_resume` for a run
    /// of which nothing is left (never started, brought down, or its parked machines removed
    /// meanwhile: its record then reads absent), before any tool acts.
    pub async fn resume(&self, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        self.refresh_if(RunState::Parked, opts).await;
        self.drive(Act::Resume, opts, events).await.map(drop)
    }

    /// Brings the running run to what the project's files now say.
    ///
    /// # Errors
    ///
    /// As the tool fails, or as the run's state refuses an update.
    pub async fn update(&self, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        self.drive(Act::Update, opts, events).await.map(drop)
    }

    /// Runs the project's set-up again (its provisioners), on `machine` or on every machine.
    ///
    /// # Errors
    ///
    /// As the tool fails; `unsupported` where the format has no set-up of its own to run again.
    /// `with`, when given, runs only the provisioner of that name (a Vagrant project's `checks`).
    pub async fn provision(&self, machine: Option<&str>, with: Option<&str>, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        self.drive(Act::Provision(machine, with), opts, events).await.map(drop)
    }

    /// Starts `machine`, one the project leaves down at `up`.
    ///
    /// # Errors
    ///
    /// As the tool fails; `unsupported` where the format has no such machine.
    pub async fn attach(&self, machine: &str, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        self.drive(Act::Attach(machine), opts, events).await.map(drop)
    }

    /// Removes `machine` from the running run.
    ///
    /// # Errors
    ///
    /// As the tool fails.
    pub async fn retire(&self, machine: &str, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        self.drive(Act::Retire(machine), opts, events).await.map(drop)
    }

    /// Runs `argv` in `machine`, with no terminal: what it said and how it ended. A command that
    /// runs and fails is a report, not an error.
    ///
    /// # Errors
    ///
    /// When the command can't be run at all (the tool missing, the machine refused).
    pub async fn exec(&self, machine: &str, argv: &[String], opts: &RunOptions, events: &dyn Events) -> Result<ExecReport> {
        self.operate(Op::Check, opts, events, async |given: &RunOptions, emitter: &Emitter<'_>, _by: OperationId| {
            let processes = self.operation_processes(given);
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
    pub async fn console_command(&self, machine: &str, user: Option<&str>) -> Result<ShellCommand> {
        let user = user.map(crate::console::check_name).transpose()?;
        let paths = Quiet::of(self);
        let cx = paths.cx(self, self.tool_env(&RunOptions::new())?);
        self.driver.console(&cx, machine, user).await
    }
}

impl Running<'_> {
    /// A tool context of this run for work that runs no tool and says nothing (moving files in
    /// its state folder).
    fn quiet<T>(&self, work: impl FnOnce(&ToolCx<'_>) -> T) -> T {
        let paths = Quiet::of(self);
        let cx = paths.cx(self, Vec::new());
        work(&cx)
    }

    /// Moves the Terraform state in `from` into the run's state folder ([`crate::Runner::adopt`]):
    /// the files moved.
    pub(crate) fn take_state(&self, from: &std::path::Path) -> Result<Vec<&'static str>> {
        self.quiet(|cx| crate::formats::terraform::Terraform::adopt(cx, from))
    }

    /// Puts the state files `moved` back in `from`, best effort (an adoption that failed after).
    pub(crate) fn give_state_back(&self, from: &std::path::Path, moved: &[&str]) {
        self.quiet(|cx| crate::formats::terraform::Terraform::give_back(cx, from, moved));
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
