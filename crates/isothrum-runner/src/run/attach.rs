//! Hot-adding a lab's own on-demand access machine into a running run, and hot-removing it: a
//! manifest machine marked `on_demand`, which the initial `up` left down (a Compose `access`
//! profile, a Vagrant `autostart: false`). `attach` brings it up on a chosen network with the
//! image the user picked, `detach` takes it back out. A lab that declares no access machine has
//! nothing to attach: an embedder adds a box of its own as an ordinary machine of the lab
//! ([`Running::update`], [`Running::retire`], [`Running::console_command`]).

use isoloom_manifest::Machine;
use serde::Serialize;
use serde_json::json;

use super::Running;
use super::operation::Done;
use crate::Result;
use crate::driver;
use crate::error::Error;
use crate::events::{Emitter, Events};
use crate::options::RunOptions;
use crate::secrets::sensitive_values;
use crate::state::Op;

/// What a hot-add produced: the access machine brought up, and where to reach it.
#[derive(Debug, Clone, Serialize)]
pub struct AttachReport {
    /// Its name, as status reports it: the access machine's manifest name.
    pub machine: String,
    /// Its native name (the Compose service / Vagrant define).
    pub native: String,
    /// The lab network it is on (the chosen one, else its first).
    pub network: Option<String>,
    /// Its address on that network, where the user reaches it.
    pub address: Option<String>,
    /// The image or box it was brought up with, when the user picked one.
    pub image: Option<String>,
}

/// Which access machine an attach adds, where, from what: as [`Running::attach_with`] takes
/// them.
#[derive(Debug, Clone, Copy, Default)]
pub struct AccessPick<'a> {
    /// The on-demand machine, when the lab has more than one.
    pub machine: Option<&'a str>,
    /// The lab network it joins (default: its first).
    pub network: Option<&'a str>,
    /// What it runs from (default: the lab's).
    pub image: Option<&'a str>,
}

impl Running<'_> {
    /// Hot-adds the lab's on-demand access machine into the running run, with `opts` (the params
    /// and secrets the run's operations take), brought up with the
    /// user-picked `image` (its baked default otherwise), on `network` when given. `image` is a
    /// container image for a Compose access service, the Vagrant box for an access VM. `machine`
    /// names which on-demand machine, needed only when the lab has more than one. `network` is one
    /// of the manifest's networks: a Compose access service's container also joins it, by the
    /// native name the run gives it (as status shows it); a VM's networks are its Vagrantfile's,
    /// so a Vagrant run refuses one.
    ///
    /// # Errors
    ///
    /// Fails with `machine_unknown` for a named machine the lab doesn't have, `unsupported` when
    /// the lab declares no access machine, or several and none is named (or the target brings
    /// none up later: a server or a cloud), and for a `network` the run doesn't have or can't
    /// join (a Vagrant run's), `not_running` when the run isn't up, `tool_failed` when the
    /// container can't join the network (one already on it has), and as the other operations do
    /// for a step or the state.
    pub async fn attach_with(&self, pick: AccessPick<'_>, opts: &RunOptions, events: &dyn Events) -> Result<AttachReport> {
        let AccessPick { machine, network, image } = pick;
        let opts = self.connected(opts);
        let mask = sensitive_values(self.manifest(), &opts.secrets);
        self.operate(Op::Attach, events, mask, async |e: &Emitter<'_>| {
            let target = self.access_machine(machine)?;
            if let Some(n) = network {
                self.network_named(n)?;
            }
            let (_, mut cx) = self.existing(&opts, e).await?;
            let native = network.map(|n| self.native_network(&cx, n)).transpose()?;
            self.observe_running(&mut cx).await?;
            self.driver.attach(&cx, target, image, native.as_deref()).await?;
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

    /// Hot-removes the lab's access machine from the running run, with `opts`, leaving the rest
    /// up: the on-demand machine chosen as [`Running::attach_with`] chooses it, taken out as
    /// [`Running::retire`] takes a machine out (its Compose service removed through the `access`
    /// profile, its VM destroyed).
    ///
    /// # Errors
    ///
    /// Fails as [`Running::attach_with`] does for the machine choice and the target, and as the other
    /// operations do for a step or the state.
    pub async fn detach_with(&self, machine: Option<&str>, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        let opts = self.connected(opts);
        let mask = sensitive_values(self.manifest(), &opts.secrets);
        self.operate(Op::Detach, events, mask, async |e: &Emitter<'_>| {
            let target = self.access_machine(machine)?;
            let (_, cx) = self.existing(&opts, e).await?;
            self.take_out(&cx, target).await?;
            Ok(Done {
                value: (),
                ok: true,
                data: json!({ "machine": target.name.as_str(), "native": driver::native_name(target) }),
                state: None,
            })
        })
        .await
    }

    /// The manifest's network `name`, refused with `unsupported` when the run has none so named.
    fn network_named(&self, name: &str) -> Result<&isoloom_manifest::Network> {
        self.manifest()
            .networks
            .iter()
            .find(|n| n.name.as_str() == name)
            .ok_or_else(|| Error::Unsupported {
                what: format!("joining network `{name}`, which this run doesn't have"),
            })
    }

    /// The native name of the run's network `name` (`flag-portal_lab`), as status renders it.
    fn native_network(&self, cx: &crate::executor::Cx<'_>, name: &str) -> Result<String> {
        self.networks(cx)
            .into_iter()
            .find(|n| n.name == name)
            .and_then(|n| n.native)
            .ok_or_else(|| Error::Unsupported {
                what: format!("joining network `{name}`: the run doesn't say its native name"),
            })
    }

    /// Which access machine an attach targets: the on-demand machine `machine` names, or the
    /// only one.
    fn access_machine(&self, machine: Option<&str>) -> Result<&Machine> {
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
                what: "hot-adding an access machine (this run declares no access machine)".to_owned(),
            }),
            _ => Err(Error::Unsupported {
                what: format!("hot-adding an access machine: this run has several ({}); name which", self.on_demand_names()),
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
        native: driver::native_name(m).to_owned(),
        network,
        address,
        image: image.map(str::to_owned),
    }
}

/// The machine's static address on `network` (its first when none is named), from the manifest.
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
