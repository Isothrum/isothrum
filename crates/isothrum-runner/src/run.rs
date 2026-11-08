//! One manifest opened under one run name: the operations an embedder calls. `operation` is what
//! every operation does around its own work, `up` starts a run, `existing` holds the operations on
//! a run that exists (down, park, resume, check, provision).

mod attach;
mod existing;
mod operation;
mod seal;
#[cfg(test)]
mod tests;
mod up;

pub use attach::{AttachReport, BoxPick};

use std::collections::BTreeMap;
use std::path::Path;

use isoloom_manifest::{Manifest, RunVar};

use crate::Result;
use crate::driver::AnyDriver;
use crate::error::Error;
use crate::events::Emitter;
use crate::events::WarningCode;
use crate::executor::{Cx, Stale};
use crate::hooks::{Edge, RunView};
use crate::manifest::LoadedManifest;
use crate::name::RunName;
use crate::options::RunOptions;
use crate::outputs;
use crate::params::params_for_existing;
use crate::runner::Runner;
use crate::scope::Scope;
use crate::secrets::Secrets;
use crate::state::Op;
use crate::status::{self, OutputValue, Status};

/// What a read-only query runs with: the run's record, its provider, and its params.
type QueryPrelude = (Option<crate::store::RunRecord>, Option<String>, BTreeMap<String, String>);

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
            let target = serde_json::to_value(loaded.manifest.target).unwrap_or_default();
            return Err(Error::Unsupported {
                what: format!("target {target} on this host"),
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
    /// delivered to every operation: status and park, which take no options, too. Kept in memory
    /// only, never stored; a value an operation's options give wins.
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
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.loaded.path
    }

    /// The manifest's folder (`${run.dir}`).
    pub(crate) fn dir(&self) -> &Path {
        &self.loaded.dir
    }

    /// The manifest's SHA-256.
    pub(crate) fn loaded_sha256(&self) -> &str {
        &self.loaded.sha256
    }

    /// What a read-only query runs with: the run's record, the provider it was started on, and
    /// its params (recorded params replayed; a fresh run's defaults otherwise). No lock, no event.
    pub(crate) fn query_prelude(&self) -> Result<QueryPrelude> {
        let record = self.runner.store.record(&self.name)?;
        let provider = record.as_ref().and_then(|r| r.provider.clone());
        let params = params_for_existing(
            self.manifest(),
            record.as_ref().map(|r| &r.params),
            &BTreeMap::new(),
            &self.scope(provider.clone()),
        )?;
        Ok((record, provider, params))
    }

    /// The run as it stands: read-only, never locked, and never an event.
    ///
    /// # Errors
    ///
    /// Fails when the engine can't be asked (the status step fails or runs out of time) or the
    /// run's record can't be read.
    pub async fn status(&self) -> Result<Status> {
        let (record, provider, params) = self.query_prelude()?;
        let silent = |_: &crate::events::Event| {};
        let events = Emitter::new(self.name.as_str(), Op::Check, self.runner.config.clock.as_ref(), &silent);
        let secrets = self.connection.clone();
        let mut cx = self.cx(params, &secrets, &events, provider.clone())?;
        if let Some(r) = &record {
            Self::bind_record(&mut cx, r);
        }
        self.driver.prepare(&cx)?;
        self.relay(&mut cx).await?;
        let observed = self.observe(&mut cx).await?;
        cx.vars.set_published(observed.published.clone());
        cx.vars.set_hosts(observed.hosts.clone());
        let state = match self.runner.op_of(&self.name).and_then(Op::during) {
            Some(during) => during,
            None => status::derive_state(&observed.machines, record.as_ref()),
        };
        let outputs = outputs::read(self.manifest(), &cx.vars, &observed.outputs, &observed.sensitive);
        outputs::bind(&mut cx.vars, &outputs);
        let message = (state == crate::RunState::Ready)
            .then(|| outputs::message(self.manifest(), &cx.vars, &outputs))
            .flatten();
        Ok(Status {
            state,
            provider,
            machines: observed.machines,
            networks: self.networks(&cx),
            outputs,
            message,
        })
    }

    /// The run's outputs, read live.
    ///
    /// # Errors
    ///
    /// Fails as [`Running::status`] does.
    pub async fn outputs(&self) -> Result<Vec<OutputValue>> {
        Ok(self.status().await?.outputs)
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
        vars.set_run(RunVar::KnownHosts, state_dir.join("known_hosts").display().to_string());
        vars.set_run(RunVar::SshKey, crate::ssh::key_path(&self.runner.config.home).display().to_string());
        if let Some(key) = crate::ssh::public_key(&self.runner.config.home) {
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
        })
    }

    /// `cx` for an operation that acts on the run (never a status read): on a provider whose
    /// management commands need elevation (Hyper-V on Windows), its tools run through one
    /// elevated session, kept in the run's state folder.
    pub(crate) fn acting<'a>(&'a self, mut cx: Cx<'a>) -> Result<Cx<'a>> {
        if cx.scope.host == isoloom_manifest::HostOs::Windows && self.driver.elevates(&cx.scope) {
            let dir = self.runner.store.run_dir(&self.name)?.join("elevated");
            let own = std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)));
            let inner = std::sync::Arc::clone(&self.runner.config.processes);
            cx.processes = std::sync::Arc::new(crate::process::Elevated::new(inner, dir, own));
        }
        Ok(cx)
    }

    /// Starts the macOS relays `cx`'s run needs (Vagrant on an ESXi host), for the operation.
    pub(crate) async fn relay(&self, cx: &mut Cx<'_>) -> Result<()> {
        let config = &self.runner.config;
        let vagrant = self.manifest().driver == isoloom_manifest::Driver::Vagrant;
        let params = cx.vars.params();
        let Some(host) = crate::relay::esxi_host(config.host == isoloom_manifest::HostOs::Macos, vagrant, cx.scope.provider.as_deref(), params) else {
            return Ok(());
        };
        let port = params.get("ESXI_HOSTPORT").and_then(|p| p.parse().ok()).unwrap_or(22);
        let relayed = crate::relay::prepare(host, port, &config.home, config.path.as_deref()).await;
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
        Scope {
            host: self.runner.config.host,
            provider,
        }
    }

    /// Reads what the engine gives back (published ports, outputs) and binds it, so a template
    /// can name it (`${outputs.ip}`): the run's outputs.
    pub(crate) async fn read_outputs(&self, cx: &mut Cx<'_>) -> Result<Vec<status::OutputValue>> {
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

    /// Binds what `observed` gives back (published ports, outputs) into `cx`: the run's outputs.
    pub(crate) fn bind_observed(&self, cx: &mut Cx<'_>, observed: crate::driver::Observed) -> Vec<status::OutputValue> {
        cx.vars.set_published(observed.published);
        cx.vars.set_hosts(observed.hosts);
        let outputs = outputs::read(self.manifest(), &cx.vars, &observed.outputs, &observed.sensitive);
        // A value the engine or the manifest keeps secret is masked in every later event.
        cx.events.hide(outputs.iter().filter(|o| o.sensitive).filter_map(|o| o.value.clone()));
        outputs::bind(&mut cx.vars, &outputs);
        outputs
    }

    /// The run's networks, their native names filled.
    fn networks(&self, cx: &Cx<'_>) -> Vec<status::NetworkView> {
        status::networks(self.manifest(), |t| cx.vars.render(t, "networks.native").ok())
    }

    /// Runs the embedder's hooks at `edge`; each failure is a warning.
    pub(crate) async fn hooks(&self, cx: &Cx<'_>, edge: Edge, outputs: &[OutputValue]) {
        if self.runner.hooks.is_empty() {
            return;
        }
        let networks = self.networks(cx);
        let view = RunView {
            name: &self.name,
            manifest: self.manifest(),
            networks: &networks,
            outputs,
        };
        for failure in edge.run(&self.runner.hooks, &view).await {
            cx.events.warning(WarningCode::HookFailed, &failure);
        }
    }
}
