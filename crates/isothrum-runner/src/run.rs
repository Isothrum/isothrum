//! One manifest opened under one run name: the operations an embedder calls, and the read-only
//! query `status` here (`console_command`, the other one, is in `console`). `operation` is what every operation does around its own work,
//! `up` starts a run, `existing` holds the operations on a run that exists (down, park, resume,
//! check, provision), `update` brings a running run up to date with its manifest, `boot` restarts
//! the VMs whose boot-time settings it changed, `retire` takes a machine out ahead of it, `attach`
//! hot-adds and removes the run's on-demand access machine (attach, detach), `console` builds the
//! command that opens a console into a machine, `seal` seals a run once it is ready, and `kept`
//! lists what a run keeps on this computer and when each thing goes.

mod attach;
mod boot;
mod console;
mod existing;
mod graphical;
mod kept;
pub(crate) mod recovery;

pub(crate) use kept::keep;
mod operation;
mod retire;
mod seal;
#[cfg(test)]
pub(crate) mod tests;
mod up;
mod update;

pub use attach::{AccessPick, AttachReport};

use std::collections::BTreeMap;
use std::path::Path;

use isoloom_manifest::{Manifest, Provider, RunVar};

use crate::Result;
use crate::driver::AnyDriver;
use crate::encoding::wire_name;
use crate::error::Error;
use crate::events::Emitter;
use crate::executor::{Cx, LineWatch};
use crate::manifest::LoadedManifest;
use crate::name::RunName;
use crate::options::RunOptions;
use crate::outputs;
use crate::params::params_for_existing;
use crate::runner::Runner;
use crate::scope::Scope;
use crate::secrets::Secrets;
use crate::status::{self, Status};

/// A run: a manifest under a name, on a runner.
pub struct Running<'r> {
    pub(crate) runner: &'r Runner,
    loaded: LoadedManifest,
    name: RunName,
    pub(crate) driver: AnyDriver,
    /// Values of the manifest's connection slots, given once ([`Running::with_connection`]).
    connection: Secrets,
}

impl std::fmt::Debug for Running<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Running")
            .field("name", &self.name)
            .field("manifest", &self.loaded.path)
            .finish_non_exhaustive()
    }
}

/// Makes the run's state folder of `cx`, owner-only, for what an operation (or a status read)
/// writes there; a query that writes nothing leaves no folder behind.
fn create_state_dir(cx: &Cx<'_>) -> Result<()> {
    crate::store::create_dir(cx.state_dir())
}

impl<'r> Running<'r> {
    pub(crate) fn new(runner: &'r Runner, loaded: LoadedManifest, name: RunName) -> Result<Running<'r>> {
        if !loaded.manifest.hosts.contains(&runner.config.host) {
            return Err(Error::Unsupported {
                what: format!("target `{}` on this host", wire_name(&loaded.manifest.target)),
            });
        }
        let driver = AnyDriver::for_manifest(&loaded.manifest)?;
        Ok(Running {
            runner,
            loaded,
            name,
            driver,
            connection: Secrets::new(),
        })
    }

    /// The run with `secrets`, the values of its connection slots (an ESXi host's password),
    /// delivered to every operation, status (which takes no options) too. Kept in memory only,
    /// never stored; a value an operation's options give wins.
    #[must_use]
    pub fn with_connection(mut self, secrets: Secrets) -> Running<'r> {
        self.connection = secrets;
        self
    }

    /// `opts` with the connection's values its secrets lack.
    pub(crate) fn with_connection_secrets(&self, opts: &RunOptions) -> RunOptions {
        let mut out = opts.clone();
        for (name, value) in self.connection.iter() {
            if out.secrets.get(name).is_none() {
                out.secrets.insert(name, value.clone());
            }
        }
        out
    }

    /// The manifest.
    #[must_use]
    pub fn manifest(&self) -> &Manifest {
        &self.loaded.manifest
    }

    /// The run's name.
    #[must_use]
    pub fn name(&self) -> &RunName {
        &self.name
    }

    /// Reserves `port` over `protocol` for this run while it starts; false when another run
    /// holds it.
    pub(crate) fn reserve(&self, port: u16, protocol: isoloom_manifest::Protocol) -> bool {
        self.runner.reserve(&self.name, port, protocol)
    }

    /// A free loopback port over `protocol`, reserved for this run.
    ///
    /// # Errors
    ///
    /// The loopback couldn't be bound.
    pub(crate) fn pick_reserved(&self, protocol: isoloom_manifest::Protocol) -> std::io::Result<Option<crate::net::HeldPort>> {
        self.runner.pick_reserved(&self.name, protocol)
    }

    /// Each `ports` entry must be free before this run starts.
    ///
    /// # Errors
    ///
    /// Fails with `port_in_use` at the first port that is reserved elsewhere or already bound.
    pub(crate) fn ensure_ports_free(&self, ports: impl IntoIterator<Item = (u16, isoloom_manifest::Protocol)>) -> Result<()> {
        self.runner.ensure_ports_free(&self.name, ports)
    }

    /// Whether an operation (this process's or another's) holds this run's lock now.
    ///
    /// # Errors
    ///
    /// The lock file couldn't be read.
    pub(crate) fn held(&self) -> Result<bool> {
        self.runner.store.held(&self.name)
    }

    /// The manifest's file.
    pub(crate) fn path(&self) -> &Path {
        &self.loaded.path
    }

    /// The manifest's folder (`${run.dir}`).
    pub(crate) fn dir(&self) -> &Path {
        &self.loaded.dir
    }

    /// A silent context of the run, with `events` and what its start kept (its record bound: its
    /// SSH jump, its slot, its forwards; its provider; its params replayed, a fresh run's
    /// defaults otherwise), so a way in is the run's: for the read-only queries. No lock.
    fn silent_cx<'a>(&'a self, events: &'a Emitter<'a>) -> Result<(Option<crate::store::RunRecord>, Cx<'a>)> {
        let record = self.runner.store.record(&self.name)?;
        let provider = record.as_ref().and_then(|r| r.provider);
        let params = params_for_existing(
            self.manifest(),
            record.as_ref().and_then(crate::store::RunRecord::replayed_params),
            &BTreeMap::new(),
            &self.scope(provider),
        )?;
        // Nothing is emitted, but its errors say what a tool printed: the connection's secrets
        // masked there, as an operation's are.
        events.hide(crate::secrets::sensitive_values(self.manifest(), &self.connection));
        let mut cx = self.cx(params, &self.connection, events, provider)?.into_query();
        if let Some(r) = &record {
            cx.bind_record(r);
        }
        Ok((record, cx))
    }

    /// Runs `f` on a silent context of the run ([`Running::silent_cx`]), emitting nothing, its state
    /// folder not made: for the queries that run no step.
    pub(crate) fn query<T>(&self, f: impl FnOnce(&Cx<'_>) -> Result<T>) -> Result<T> {
        let events = self.silent();
        let (_, cx) = self.silent_cx(&events)?;
        f(&cx)
    }

    /// An emitter of the run that says nothing.
    fn silent(&self) -> Emitter<'_> {
        Emitter::silent(self.name.as_str(), self.runner.config.clock.as_ref())
    }

    /// The run as it stands: never locked, and never an event. It changes nothing of the run, but
    /// readies its state folder as an operation does for the tools it asks (the folder; Compose's
    /// empty pinned-ports file, which every Compose call names; Terraform's plugin cache); it
    /// never writes a secret (the consoles the runner chooses are an operation's to choose). While an operation holds the run
    /// (this runner's, or another process's), the engine isn't asked: that operation acts on the
    /// run with the same tools, which may refuse a read until it is done (Vagrant locks each VM
    /// for the whole of a `vagrant up`, provisioning included). The status is then the
    /// operation's state, with the machines as last read ([`Status::live`] false).
    ///
    /// # Errors
    ///
    /// Fails when the engine can't be asked (the status step fails or runs out of time) or the
    /// run's record can't be read. Another process acting on a machine (Vagrant's lock on it,
    /// past a short wait) is no failure: that machine is [`MachineState::Busy`](crate::MachineState::Busy),
    /// the others as last read, and the status isn't live.
    pub async fn status(&self) -> Result<Status> {
        let events = self.silent();
        let (record, mut cx) = self.silent_cx(&events)?;
        let provider = cx.scope.provider;
        let read = self.read_for_query(&mut cx, record.as_ref()).await;
        let (state, mut observed, live) = read.map_err(|e| self.classify_server_refusal(e, &RunOptions::new().with_params(cx.vars.params().clone())))?;
        if live {
            // The status read now is the run's state: the store follows it where the state
            // machine allows (a VM stopped outside the runner: ready to failed).
            if let Err(e) = self.runner.store.reconcile(&self.name, &observed.seen(), state) {
                tracing::warn!(error = %e, "the run's observed state wasn't recorded");
            }
        }
        let row = self.runner.store.db().read(|tx| tx.run(&self.name))?;
        let mut machines = std::mem::take(&mut observed.machines);
        self.offer_consoles(&cx, &mut machines).await;
        let outputs = self.bind_observed(&mut cx, observed).values;
        let message = (state == crate::RunState::Ready)
            .then(|| outputs::message(self.manifest(), &cx.vars, &outputs))
            .flatten();
        Ok(Status {
            state,
            live,
            desired: row.as_ref().map(|r| r.desired),
            failed_cause: row.and_then(|r| r.failed_cause),
            provider,
            machines,
            networks: self.networks(&cx),
            outputs,
            message,
            native: crate::graphical::native::app(self.manifest(), provider, cx.scope.host),
        })
    }

    /// Each machine's console offer, as it stands now: what its declaration offers on this
    /// provider, and for a running machine whether the console is there and answering. Its own
    /// step of a status read.
    async fn offer_consoles(&self, cx: &Cx<'_>, machines: &mut [crate::MachineStatus]) {
        let known = cx.scope.provider;
        for m in machines {
            let Some(declared) = self.manifest().machines.iter().find(|x| x.name.as_str() == m.name) else {
                continue;
            };
            m.console = crate::graphical::ConsoleOffer::of(declared, known);
            if m.state.runs()
                && let Some(offer) = m.console.as_mut()
                && let Some(console) = declared.console(known).filter(|c| c.source.is_some())
            {
                offer.unavailable = crate::graphical::unavailable(cx, declared, console).await;
            }
        }
    }

    /// Where the run stands, its machines and whether they were read now, for a query (`cx`, a
    /// silent context, `record` its run's record): while an operation holds the run, its state
    /// and the machines as last read; else as the engine reads them, unless another process
    /// acting on some of them refuses the read ([`crate::driver::Observation::Busy`]): then those
    /// are busy, the others as last read, and the run where an operation holding it now, or its
    /// record, says.
    pub(crate) async fn read_for_query(
        &self,
        cx: &mut Cx<'_>,
        record: Option<&crate::store::RunRecord>,
    ) -> Result<(crate::RunState, crate::driver::Observed, bool)> {
        if let Some(state) = self.state_while_held(record)? {
            return Ok((state, self.last_seen(), false));
        }
        create_state_dir(cx)?;
        self.driver.prepare(cx)?;
        self.relay(cx).await?;
        let busy = match self.observe_now(cx).await? {
            crate::driver::Observation::Read(observed) => return Ok((status::derive_state(&observed.machines, record), observed, true)),
            crate::driver::Observation::Busy(busy) => busy,
        };
        let recorded = record.map_or(crate::RunState::Absent, |r| r.state);
        let state = self.state_while_held(record)?.unwrap_or(recorded);
        let mut seen = self.last_seen();
        for m in seen.machines.iter_mut().filter(|m| busy.contains(&m.name)) {
            m.state = crate::MachineState::Busy;
        }
        Ok((state, seen, false))
    }

    /// Where the run stands while an operation holds it, if one does: the moving state of this
    /// runner's operation on it (`creating` from the first instant of an `up`), else what its
    /// record says (another process's operation records its moving state once it begins acting;
    /// an update leaves the run `ready`).
    ///
    /// Two reads, not one atomic look: an operation can take the run between them, or between
    /// this and the query's own read of the engine. Tolerated, not locked out: a status is
    /// advisory, never waits for an operation, and the next one reads the run as it then stands.
    fn state_while_held(&self, record: Option<&crate::store::RunRecord>) -> Result<Option<crate::RunState>> {
        let recorded = record.map_or(crate::RunState::Absent, |r| r.state);
        if let Some(op) = self.runner.op_of(&self.name) {
            return Ok(Some(op.during().unwrap_or(recorded)));
        }
        Ok(self.held()?.then_some(recorded))
    }

    /// The run as the runner last read it ([`crate::store::LastObserved`]); nothing before it ever did,
    /// or when what it kept can't be read (traced).
    fn last_seen(&self) -> crate::driver::Observed {
        match self.runner.store.last_observed(&self.name) {
            Ok(seen) => crate::driver::Observed::from_seen(seen.unwrap_or_default()),
            Err(e) => {
                tracing::warn!(error = %e, "what the runner last saw of the run can't be read: no machine is shown");
                crate::driver::Observed::default()
            }
        }
    }

    /// What `params`, `secrets` and `events` give this run's steps.
    pub(crate) fn cx<'a>(
        &'a self,
        params: BTreeMap<String, String>,
        secrets: &'a Secrets,
        events: &'a Emitter<'a>,
        provider: Option<Provider>,
    ) -> Result<Cx<'a>> {
        let state_dir = self.runner.store.run_dir_path(&self.name);
        let mut vars = crate::vars::Vars::default();
        vars.set_run(RunVar::Name, self.name.as_str());
        vars.set_run(RunVar::Dir, self.dir().display().to_string());
        vars.set_run(RunVar::StateDir, state_dir.display().to_string());
        vars.set_run(RunVar::KnownHosts, state_dir.join(crate::ssh::KNOWN_HOSTS).display().to_string());
        vars.set_run(RunVar::SshKey, crate::ssh::key_path(&self.runner.config.home).display().to_string());
        if let Some(key) = crate::ssh::public_key(&self.runner.config.home)? {
            vars.set_run(RunVar::SshPublicKey, key);
        }
        if let Some(p) = &provider {
            vars.set_run(RunVar::Provider, crate::wire_name(p));
        }
        vars.set_params(params);
        let stale = self
            .manifest()
            .recovery
            .as_ref()
            .map(|r| LineWatch::any_case(&crate::tool_output::leftover::markers_of(&r.leftovers)))
            .unwrap_or_default();
        Ok(Cx {
            run: self,
            state_dir,
            vars,
            secrets,
            events,
            scope: self.scope(provider),
            stale,
            gone: LineWatch::default(),
            processes: std::sync::Arc::clone(&self.runner.config.processes),
            env: std::collections::BTreeMap::new(),
            bound: crate::executor::Bound::default(),
        })
    }

    /// `cx` for an operation that acts on the run (never a status read): on a provider whose
    /// management commands need elevation (Hyper-V on Windows), its tools run through one
    /// elevated session, kept in the run's state folder.
    pub(crate) fn with_operation_processes<'a>(&'a self, mut cx: Cx<'a>) -> Result<Cx<'a>> {
        create_state_dir(&cx)?;
        // Its tools recorded in the run's state folder while they run: this process killed, the
        // next operation ends them ([`crate::process::ledger`]).
        let tools = cx.state_dir().join(crate::process::ledger::DIR);
        let recorded: std::sync::Arc<dyn crate::ProcessRunner> = std::sync::Arc::new(crate::process::Recording::new(
            std::sync::Arc::clone(&self.runner.config.processes),
            tools,
            self.runner.operation_of(&self.name),
        ));
        cx.processes = std::sync::Arc::clone(&recorded);
        if cx.scope.host == isoloom_manifest::HostOs::Windows && self.driver.elevates(&cx.scope) {
            let dir = cx.state_dir().join("elevated");
            cx.processes = std::sync::Arc::new(crate::process::Elevated::new(recorded, dir, self.elevated_env()));
        }
        Ok(cx)
    }

    /// What an elevated session is handed of the environment the runner read when it was
    /// configured ([`RunnerConfig::host_env`](crate::RunnerConfig) and its `PATH`).
    pub(crate) fn elevated_env(&self) -> Vec<(String, String)> {
        let config = &self.runner.config;
        config.host_env.for_elevation(config.search_path.as_deref())
    }

    /// Makes the runner's SSH key when the manifest reaches a machine over SSH and it has none yet
    /// (an adopted run, or one acted on from another home, may need it).
    pub(crate) async fn ensure_ssh_key(&self) -> Result<()> {
        if crate::ssh::needed(self.manifest()) {
            crate::ssh::ensure_key(self.runner.config.processes.as_ref(), &self.runner.config.home).await?;
        }
        Ok(())
    }

    /// Starts the macOS relays `cx`'s run needs (Vagrant on an ESXi host), for the operation.
    pub(crate) async fn relay(&self, cx: &mut Cx<'_>) -> Result<()> {
        let config = &self.runner.config;
        // Only Vagrant drives an ESXi host through the relays.
        if self.manifest().driver != isoloom_manifest::Driver::Vagrant {
            return Ok(());
        }
        let params = cx.vars.params();
        let Some((host, port)) = crate::relay::esxi_host(&cx.scope, params)? else {
            return Ok(());
        };
        let no_proxy = config.host_env.text("no_proxy").or_else(|| config.host_env.text("NO_PROXY"));
        let relayed = crate::relay::prepare(host, port, &config.home, config.search_path.as_deref(), no_proxy).await;
        cx.relay_through(relayed?);
        Ok(())
    }

    /// This host, with `provider`.
    pub(crate) fn scope(&self, provider: Option<Provider>) -> Scope {
        Scope::new(self.runner.config.host, provider)
    }

    /// Reads what the engine gives back (published ports, outputs) and binds it, so a template
    /// can name it (`${outputs.ip}`): the run's outputs.
    pub(crate) async fn read_outputs(&self, cx: &mut Cx<'_>) -> Result<outputs::Read> {
        let observed = self.observe(cx).await?;
        Ok(self.bind_observed(cx, observed))
    }

    /// The run as its engine reports it: the driver's machines, then, when the run's containers
    /// run inside one of its VMs (`docker-vm`'s guest), those containers as read in the VM.
    /// What it read is kept as what the runner last saw of the run ([`crate::store::LastObserved`]).
    pub(crate) async fn observe(&self, cx: &mut Cx<'_>) -> Result<crate::driver::Observed> {
        match self.observe_now(cx).await? {
            crate::driver::Observation::Read(observed) => Ok(observed),
            // Only a query is refused a read: an operation holds the run, so this never happens
            // to one; said as the run held, never as machines absent.
            crate::driver::Observation::Busy(_) => Err(Error::Locked { run: self.name().to_string() }),
        }
    }

    /// [`Running::observe`], or, for a query another process's lock refused, the machines it
    /// acts on: nothing read, what the runner last saw stands.
    async fn observe_now(&self, cx: &mut Cx<'_>) -> Result<crate::driver::Observation> {
        let mut observed = match self.driver.observe(cx).await? {
            crate::driver::Observation::Read(observed) => observed,
            busy @ crate::driver::Observation::Busy(_) => return Ok(busy),
        };
        if let Some(guest) = &self.manifest().guest {
            crate::in_vm::observe(cx, &self.driver, guest, &mut observed).await;
        }
        // An operation's read is kept even before its run has a row (a start's first read).
        let kept = self
            .runner
            .op_of(&self.name)
            .is_some()
            .then(|| self.record_or_fresh())
            .transpose()
            .and_then(|fresh| self.runner.store.keep_seen(&self.name, &observed.seen(), fresh));
        if let Err(e) = kept {
            tracing::warn!(error = %e, "what the runner saw of the run wasn't kept");
        }
        Ok(crate::driver::Observation::Read(observed))
    }

    /// [`Running::observe`], refused with `not_running` when no member machine runs.
    pub(crate) async fn observe_running(&self, cx: &mut Cx<'_>) -> Result<crate::driver::Observed> {
        let observed = self.observe(cx).await?;
        if !status::any_member_running(&observed.machines) {
            return Err(Error::NotRunning { run: self.name().to_string() });
        }
        Ok(observed)
    }

    /// The manifest's machine `name`, refused with `machine_unknown` when the run has none.
    pub(crate) fn machine_named(&self, name: &str) -> Result<&isoloom_manifest::Machine> {
        self.manifest()
            .machines
            .iter()
            .find(|m| m.name.as_str() == name)
            .ok_or_else(|| Error::MachineUnknown {
                run: self.name().to_string(),
                machine: name.to_owned(),
            })
    }

    /// Binds what `observed` gives back (published ports, outputs) into `cx`: the run's outputs.
    pub(crate) fn bind_observed(&self, cx: &mut Cx<'_>, observed: crate::driver::Observed) -> outputs::Read {
        cx.vars.set_published(observed.published);
        cx.vars.set_hosts(observed.hosts);
        let read = outputs::read(self.manifest(), &cx.vars, &observed.outputs, &observed.sensitive);
        // A value the engine or the manifest keeps secret is masked in every later event.
        cx.events
            .hide(read.values.iter().filter(|o| o.sensitive).filter_map(|o| o.revealed().map(str::to_owned)));
        outputs::bind(&mut cx.vars, &read.values);
        read
    }

    /// The run's networks, their native names filled; one that can't be is left out, and traced.
    fn networks(&self, cx: &Cx<'_>) -> Vec<status::NetworkView> {
        status::networks(self.manifest(), |t| {
            cx.vars
                .render(t, "networks.native")
                .inspect_err(|e| tracing::warn!(error = %e, "a network's native name couldn't be filled: it is left out"))
                .ok()
        })
    }
}
