//! One manifest opened under one run name: the operations an embedder calls, and the read-only
//! queries (status, outputs) here. `operation` is what every operation does around its own work,
//! `up` starts a run, `existing` holds the operations on a run that exists (down, park, resume,
//! check, provision), `update` brings a running run up to date with its manifest, `boot` restarts
//! the VMs whose boot-time settings it changed, `retire` takes a machine out ahead of it, `attach`
//! hot-adds and removes the run's on-demand access machine (attach, detach), `console` builds the
//! command that opens a console into a machine, and `seal` seals a run once it is ready.

mod attach;
mod boot;
mod console;
mod existing;
mod operation;
mod retire;
mod seal;
#[cfg(test)]
mod tests;
mod up;
mod update;

pub use attach::{AccessPick, AttachReport};

use std::collections::BTreeMap;
use std::path::Path;

use isoloom_manifest::{Manifest, RunVar};

use crate::Result;
use crate::driver::AnyDriver;
use crate::encoding::wire_name;
use crate::error::Error;
use crate::events::Emitter;
use crate::executor::{Cx, Stale};
use crate::manifest::LoadedManifest;
use crate::name::RunName;
use crate::options::RunOptions;
use crate::outputs;
use crate::params::params_for_existing;
use crate::runner::Runner;
use crate::scope::Scope;
use crate::secrets::Secrets;
use crate::state::Op;
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
    pub(crate) fn connected(&self, opts: &RunOptions) -> RunOptions {
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

    /// The manifest's file.
    pub(crate) fn path(&self) -> &Path {
        &self.loaded.path
    }

    /// The manifest's folder (`${run.dir}`).
    pub(crate) fn dir(&self) -> &Path {
        &self.loaded.dir
    }

    /// A quiet context of the run, with `events` and what its start kept (its record bound: its
    /// SSH jump, its slot, its forwards; its provider; its params replayed, a fresh run's
    /// defaults otherwise), so a way in is the run's: for the read-only queries. No lock.
    fn quiet<'a>(&'a self, events: &'a Emitter<'a>) -> Result<(Option<crate::store::RunRecord>, Cx<'a>)> {
        let record = self.runner.store.record(&self.name)?;
        let provider = record.as_ref().and_then(|r| r.provider.clone());
        let params = params_for_existing(
            self.manifest(),
            record.as_ref().map(|r| &r.params),
            &BTreeMap::new(),
            &self.scope(provider.clone()),
        )?;
        let mut cx = self.cx(params, &self.connection, events, provider)?;
        if let Some(r) = &record {
            Self::bind_record(&mut cx, r);
        }
        Ok((record, cx))
    }

    /// Runs `f` on a quiet context of the run ([`Running::quiet`]), emitting nothing: for the
    /// pure queries.
    pub(crate) fn query<T>(&self, f: impl FnOnce(&Cx<'_>) -> Result<T>) -> Result<T> {
        let events = self.silent();
        let (_, cx) = self.quiet(&events)?;
        f(&cx)
    }

    /// An emitter of the run that says nothing.
    fn silent(&self) -> Emitter<'_> {
        Emitter::silent(self.name.as_str(), self.runner.config.clock.as_ref())
    }

    /// The run as it stands: read-only, never locked, and never an event.
    ///
    /// # Errors
    ///
    /// Fails when the engine can't be asked (the status step fails or runs out of time) or the
    /// run's record can't be read.
    pub async fn status(&self) -> Result<Status> {
        let events = self.silent();
        let (record, mut cx) = self.quiet(&events)?;
        let provider = cx.scope.provider.clone();
        self.driver.prepare(&cx)?;
        self.relay(&mut cx).await?;
        let mut observed = self.observe(&mut cx).await?;
        let state = match self.runner.op_of(&self.name).and_then(Op::during) {
            Some(during) => during,
            None => status::derive_state(&observed.machines, record.as_ref()),
        };
        let machines = std::mem::take(&mut observed.machines);
        let outputs = self.bind_observed(&mut cx, observed).values;
        let message = (state == crate::RunState::Ready)
            .then(|| outputs::message(self.manifest(), &cx.vars, &outputs))
            .flatten();
        Ok(Status {
            state,
            provider,
            machines,
            networks: self.networks(&cx),
            outputs,
            message,
        })
    }

    /// What `params`, `secrets` and `events` give this run's steps.
    pub(crate) fn cx<'a>(
        &'a self,
        params: BTreeMap<String, String>,
        secrets: &'a Secrets,
        events: &'a Emitter<'a>,
        provider: Option<String>,
    ) -> Result<Cx<'a>> {
        let state_dir = self.runner.store.run_dir(&self.name)?;
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
            vars.set_run(RunVar::Provider, p.clone());
        }
        vars.set_params(params);
        let stale = self.manifest().recovery.as_ref().map(|r| Stale::new(&r.stale_markers)).unwrap_or_default();
        Ok(Cx {
            run: self,
            vars,
            secrets,
            events,
            scope: self.scope(provider),
            stale,
            processes: std::sync::Arc::clone(&self.runner.config.processes),
            ssh_jump: None,
            expires_at: None,
            relayed: None,
            set_up: std::collections::BTreeSet::new(),
        })
    }

    /// `cx` for an operation that acts on the run (never a status read): on a provider whose
    /// management commands need elevation (Hyper-V on Windows), its tools run through one
    /// elevated session, kept in the run's state folder.
    pub(crate) fn acting<'a>(&'a self, mut cx: Cx<'a>) -> Result<Cx<'a>> {
        if cx.scope.host == isoloom_manifest::HostOs::Windows && self.driver.elevates(&cx.scope) {
            let dir = self.runner.store.run_dir(&self.name)?.join("elevated");
            let inner = std::sync::Arc::clone(&self.runner.config.processes);
            cx.processes = std::sync::Arc::new(crate::process::Elevated::new(inner, dir, self.elevated_env()));
        }
        Ok(cx)
    }

    /// What an elevated session is handed of the environment the runner read when it was
    /// configured ([`RunnerConfig::host_env`](crate::RunnerConfig) and its `PATH`).
    pub(crate) fn elevated_env(&self) -> Vec<(String, String)> {
        let config = &self.runner.config;
        config.host_env.for_elevation(config.path.as_deref())
    }

    /// Makes the runner's SSH key when the manifest reaches a machine over SSH and it has none yet
    /// (a run taken over from another runtime, or acted on from another home, may need it).
    pub(crate) async fn ensure_ssh_key(&self) -> Result<()> {
        if crate::ssh::needed(self.manifest()) {
            crate::ssh::ensure_key(self.runner.config.processes.as_ref(), &self.runner.config.home).await?;
        }
        Ok(())
    }

    /// Starts the macOS relays `cx`'s run needs (Vagrant on an ESXi host), for the operation.
    pub(crate) async fn relay(&self, cx: &mut Cx<'_>) -> Result<()> {
        let config = &self.runner.config;
        let vagrant = self.manifest().driver == isoloom_manifest::Driver::Vagrant;
        let params = cx.vars.params();
        let Some((host, port)) = crate::relay::esxi_host(&cx.scope, vagrant, params)? else {
            return Ok(());
        };
        let no_proxy = config.host_env.get("no_proxy").or_else(|| config.host_env.get("NO_PROXY"));
        let relayed = crate::relay::prepare(host, port, &config.home, config.path.as_deref(), no_proxy).await;
        cx.relayed = Some(relayed.map_err(Error::io(config.home.join("relay"), "set up"))?);
        Ok(())
    }

    /// Binds what `record` keeps of the run into `cx`: its slot (`${run.slot}`), its SSH jump and
    /// its expiry.
    pub(crate) fn bind_record(cx: &mut Cx<'_>, record: &crate::store::RunRecord) {
        if let Some(slot) = record.slot {
            cx.vars.set_run(RunVar::Slot, slot.to_string());
        }
        cx.ssh_jump.clone_from(&record.ssh_jump);
        cx.expires_at = record.expires_at;
        // The host ports a fresh start pinned: a resume forwards the same ones, and status reads
        // them back (the Vagrant driver reports its forwards from here, not the manifest's number).
        cx.vars.set_forwards(record.vm_host_ports.clone());
    }

    /// This host, with `provider`.
    pub(crate) fn scope(&self, provider: Option<String>) -> Scope {
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
    pub(crate) async fn observe(&self, cx: &mut Cx<'_>) -> Result<crate::driver::Observed> {
        let mut observed = self.driver.observe(cx).await?;
        if let Some(guest) = &self.manifest().guest {
            crate::in_vm::observe(cx, guest, &mut observed).await;
        }
        Ok(observed)
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

    /// The run's networks, their native names filled.
    fn networks(&self, cx: &Cx<'_>) -> Vec<status::NetworkView> {
        status::networks(self.manifest(), |t| cx.vars.render(t, "networks.native").ok())
    }
}
