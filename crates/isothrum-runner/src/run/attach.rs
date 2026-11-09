//! Hot-adding a lab's own on-demand access machine into a running run, and hot-removing it: a
//! manifest machine marked `on_demand`, which the initial `up` left down (a Compose `access`
//! profile, a Vagrant `autostart: false`). `attach` brings it up on a chosen network with the
//! learner's picked image, `detach` takes it back out. A lab that declares no access machine has
//! nothing to attach: an embedder adds a box of its own as an ordinary machine of the lab
//! ([`Running::update`], [`Running::retire`], [`Running::console_command`]).

use isoloom_manifest::{Machine, Native};
use serde::Serialize;
use serde_json::json;

use super::Running;
use super::operation::Done;
use crate::Result;
use crate::error::Error;
use crate::events::{Emitter, Events};
use crate::options::RunOptions;
use crate::secrets::sensitive_values;
use crate::state::Op;
use crate::status::ShellCommand;

/// What a hot-add produced: the attacker box brought up, and where to reach it.
#[derive(Debug, Clone, Serialize)]
pub struct AttachReport {
    /// The box's name, as status reports it: the access machine's manifest name.
    pub machine: String,
    /// Its native name (the Compose service / Vagrant define).
    pub native: String,
    /// The lab network it is on (the chosen one, else its first).
    pub network: Option<String>,
    /// Its address on that network, for the learner to reach it.
    pub address: Option<String>,
    /// The image or box it was brought up with, when the learner picked one.
    pub image: Option<String>,
}

/// Which box an attach adds, where, from what: as [`Running::attach`] takes them.
#[derive(Debug, Clone, Copy, Default)]
pub struct BoxPick<'a> {
    /// The on-demand machine, when the lab has more than one.
    pub machine: Option<&'a str>,
    /// The lab network it joins (default: its first).
    pub network: Option<&'a str>,
    /// What it runs from (default: the lab's).
    pub image: Option<&'a str>,
}

impl Running<'_> {
    /// Hot-adds the lab's on-demand access machine into the running run, brought up with the
    /// learner-picked `image` (its baked default otherwise), on `network` when given. `image` is a
    /// container image for a Compose access service, the Vagrant box for an access VM. `machine`
    /// names which on-demand machine, needed only when the lab has more than one.
    ///
    /// # Errors
    ///
    /// Fails with `machine_unknown` for a named machine the lab doesn't have, `unsupported` when
    /// the lab declares no access machine, or several and none is named (or the target brings
    /// none up later: a server or a cloud), `not_running` when the run isn't up, and as the other
    /// operations do for a step or the state.
    pub async fn attach(&self, machine: Option<&str>, network: Option<&str>, image: Option<&str>, events: &dyn Events) -> Result<AttachReport> {
        self.attach_with(&RunOptions::default(), BoxPick { machine, network, image }, events).await
    }

    /// [`Running::attach`] with `opts`: the params and secrets the run's operations take.
    ///
    /// # Errors
    ///
    /// As [`Running::attach`].
    pub async fn attach_with(&self, opts: &RunOptions, pick: BoxPick<'_>, events: &dyn Events) -> Result<AttachReport> {
        let BoxPick { machine, network, image } = pick;
        let opts = self.connected(opts);
        let mask = sensitive_values(self.manifest(), &opts.secrets);
        self.operate(Op::Attach, events, mask, async |e: &Emitter<'_>| {
            let target = self.attacker(machine)?;
            let (_, mut cx) = self.existing(&opts, e).await?;
            self.observe_running(&mut cx).await?;
            self.driver.attach(&cx, native_name(target), image, network).await?;
            let report = report(target, network, image);
            let data = json!({ "machine": report.machine, "native": report.native, "network": report.network, "address": report.address });
            Ok(Done {
                value: report,
                ok: true,
                data,
                state: None,
            })
        })
        .await
    }

    /// Hot-removes the attacker box from the running run, leaving the rest up.
    ///
    /// # Errors
    ///
    /// Fails as [`Running::attach`] does for the box choice and the target, and as the other
    /// operations do for a step or the state.
    pub async fn detach(&self, machine: Option<&str>, events: &dyn Events) -> Result<()> {
        self.detach_with(&RunOptions::default(), machine, events).await
    }

    /// [`Running::detach`] with `opts`, as [`Running::attach_with`].
    ///
    /// # Errors
    ///
    /// As [`Running::detach`].
    pub async fn detach_with(&self, opts: &RunOptions, machine: Option<&str>, events: &dyn Events) -> Result<()> {
        let opts = self.connected(opts);
        let mask = sensitive_values(self.manifest(), &opts.secrets);
        self.operate(Op::Detach, events, mask, async |e: &Emitter<'_>| {
            let target = self.attacker(machine)?;
            let (_, cx) = self.existing(&opts, e).await?;
            self.driver.detach(&cx, native_name(target)).await?;
            Ok(Done {
                value: (),
                ok: true,
                data: json!({ "machine": target.name.as_str(), "native": native_name(target) }),
                state: None,
            })
        })
        .await
    }

    /// The command that opens an interactive shell into the lab's access machine: the runner
    /// builds it from the run's runtime (the Compose project, the Vagrant define), so the embedder
    /// opens the box by selection and never reconstructs a container name. A pure query — it runs
    /// nothing, emits nothing, and doesn't check the box is up (the embedder runs what it returns
    /// in its own terminal once the box is attached).
    ///
    /// # Errors
    ///
    /// Fails as [`Running::attach`] does for the box choice and the target.
    pub fn shell_command(&self, machine: Option<&str>) -> Result<ShellCommand> {
        let target = self.attacker(machine)?;
        self.query(|cx| self.driver.shell_command(cx, native_name(target)))
    }

    /// The command that opens an interactive console into `machine`, any machine of the run (an
    /// SSH login on a cloud's): a pure query, as [`Running::shell_command`].
    ///
    /// # Errors
    ///
    /// Fails with `machine_unknown` for a machine the run doesn't have, `not_running` when the
    /// run gives it no address yet, and `unsupported` where the target has no way in yet.
    pub fn console_command(&self, machine: &str) -> Result<ShellCommand> {
        let target = self.machine_named(machine)?;
        self.query(|cx| self.driver.console_command(cx, target))
    }

    /// Runs `f` on a quiet context of the run, with what its start kept (its SSH jump, its slot),
    /// so a way in is the run's: for the pure queries.
    fn query<T>(&self, f: impl FnOnce(&crate::executor::Cx<'_>) -> Result<T>) -> Result<T> {
        let (record, provider, params) = self.query_prelude()?;
        let silent = |_: &crate::events::Event| {};
        let emit = Emitter::new(self.name().as_str(), Op::Check, self.runner.config.clock.as_ref(), &silent);
        let mut cx = self.cx(params, &self.connection, &emit, provider)?;
        if let Some(r) = &record {
            Self::bind_record(&mut cx, r);
        }
        f(&cx)
    }

    /// Which box an attach targets: the on-demand machine `machine` names, or the only one.
    fn attacker(&self, machine: Option<&str>) -> Result<&Machine> {
        let mut on_demand = self.manifest().machines.iter().filter(|m| m.on_demand);
        if let Some(name) = machine {
            return on_demand.find(|m| m.name.as_str() == name).ok_or_else(|| Error::MachineUnknown {
                run: self.name().to_string(),
                machine: name.to_owned(),
            });
        }
        match (on_demand.next(), on_demand.next()) {
            (Some(one), None) => Ok(one),
            (None, _) => Err(Error::Unsupported {
                what: "hot-adding an access machine (this lab declares no access machine)".to_owned(),
            }),
            _ => Err(Error::Unsupported {
                what: format!("hot-adding an access machine: this lab has several ({}); name which", self.on_demand_names()),
            }),
        }
    }

    /// The lab's on-demand access machines, by name, for the "name which" message.
    fn on_demand_names(&self) -> String {
        self.manifest()
            .machines
            .iter()
            .filter(|m| m.on_demand)
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The report of an attach of `m`: its identity and address from the manifest, with the picked
/// `network` and `image`.
fn report(m: &Machine, network: Option<&str>, image: Option<&str>) -> AttachReport {
    let (network, address) = address_on(m, network);
    AttachReport {
        machine: m.name.as_str().to_owned(),
        native: native_name(m).to_owned(),
        network,
        address,
        image: image.map(str::to_owned),
    }
}

/// The native name the driver addresses the box by.
fn native_name(m: &Machine) -> &str {
    match &m.native {
        Some(Native::Compose { service }) => service.as_str(),
        Some(Native::Vagrant { vagrant, .. }) => vagrant.as_str(),
        _ => m.name.as_str(),
    }
}

/// The box's static address on `network` (its first when none is named), from the manifest.
fn address_on(m: &Machine, network: Option<&str>) -> (Option<String>, Option<String>) {
    match network {
        Some(n) => (
            Some(n.to_owned()),
            m.addresses.iter().find(|(k, _)| k.as_str() == n).map(|(_, a)| a.to_string()),
        ),
        None => m
            .addresses
            .iter()
            .next()
            .map_or((None, None), |(k, a)| (Some(k.as_str().to_owned()), Some(a.to_string()))),
    }
}
