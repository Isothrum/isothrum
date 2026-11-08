//! Hot-adding the learner's attacker box into a running run, and hot-removing it. A lab no longer
//! declares an access machine, so the box is stood up ad hoc on the running lab's network with no
//! manifest or Compose machine behind it ([`crate::driver::Attacker::AdHoc`]: a container, or a VM
//! the runner adds to a VM lab).
//!
//! For backward compatibility, a lab that still carries an on-demand access machine (a manifest
//! machine marked `on_demand`, which the initial `up` left down — a Compose `access` profile, a
//! Vagrant `autostart: false`) keeps its old path: `attach` brings that machine up on a chosen
//! network with the learner's picked image, `detach` takes it back out.

use isoloom_manifest::{Machine, Native};
use serde::Serialize;
use serde_json::{Value, json};

use super::Running;
use super::operation::Done;
use crate::Result;
use crate::driver::{Attached, Attacker};
use crate::error::Error;
use crate::events::{Emitter, Events};
use crate::options::RunOptions;
use crate::secrets::sensitive_values;
use crate::state::Op;
use crate::status::{ShellCommand, any_target_running};

/// What a hot-add produced: the attacker box brought up, and where to reach it.
#[derive(Debug, Clone, Serialize)]
pub struct AttachReport {
    /// The box's name, as status reports it: the access machine's manifest name, or
    /// `isoloom-access` for an ad-hoc box.
    pub machine: String,
    /// Its native name (the Compose service / Vagrant define, or the ad-hoc container name).
    pub native: String,
    /// The lab network it is on (the chosen one, else its first).
    pub network: Option<String>,
    /// Its address on that network, for the learner to reach it.
    pub address: Option<String>,
    /// The image or box it was brought up with, when the learner picked one.
    pub image: Option<String>,
}

/// Which attacker box a hot-add acts on: the manifest's on-demand access machine when the lab
/// declares one, else an ad-hoc box with no machine (which the driver stands up itself).
enum Target<'a> {
    Machine(&'a Machine),
    AdHoc,
}

impl Target<'_> {
    /// How the driver addresses this box.
    fn as_attacker(&self) -> Attacker<'_> {
        match self {
            Target::Machine(m) => Attacker::Machine(native_name(m)),
            Target::AdHoc => Attacker::AdHoc,
        }
    }

    /// The report for this box: a machine's identity and address come from the manifest, an ad-hoc
    /// box's from what the driver stood up ([`Attached`]). `network` and `image` are the picks.
    fn report(&self, network: Option<&str>, image: Option<&str>, attached: Attached) -> AttachReport {
        let image = image.map(str::to_owned);
        match self {
            Target::Machine(m) => {
                let (network, address) = address_on(m, network);
                AttachReport {
                    machine: m.name.as_str().to_owned(),
                    native: native_name(m).to_owned(),
                    network,
                    address,
                    image,
                }
            }
            Target::AdHoc => {
                AttachReport {
                    // The name status reports the box under, so the two can be matched.
                    machine: crate::driver::ADHOC_NAME.to_owned(),
                    native: attached.name.unwrap_or_default(),
                    network: attached.network,
                    address: attached.address,
                    image,
                }
            }
        }
    }

    /// The `result` event's data for a detach.
    fn detach_data(&self) -> Value {
        match self {
            Target::Machine(m) => json!({ "machine": m.name.as_str(), "native": native_name(m) }),
            Target::AdHoc => json!({ "machine": Value::Null, "native": Value::Null }),
        }
    }
}

impl Running<'_> {
    /// Hot-adds the attacker box into the running run: an ad-hoc box on the lab's network (the
    /// default now that labs declare no access machine), or the manifest's on-demand access machine
    /// when the lab still has one. It is brought up with the learner-picked `image` (its baked or
    /// Kali default otherwise), on `network` when given. `image` is a container image: an ad-hoc
    /// box runs it, beside a container lab's containers or inside the attack VM the runner adds
    /// to a VM lab; a manifest access VM alone takes it as its Vagrant box. `machine` names which on-demand machine,
    /// needed only when the lab has more than one.
    ///
    /// # Errors
    ///
    /// Fails with `machine_unknown` for a named machine the lab doesn't have, `unsupported` when
    /// the lab has several on-demand machines and none is named (or the target can't make an
    /// ad-hoc box: a VM lab on a server or a cloud), `not_running` when the run isn't up,
    /// and as the other operations do for a step or the state.
    pub async fn attach(&self, machine: Option<&str>, network: Option<&str>, image: Option<&str>, events: &dyn Events) -> Result<AttachReport> {
        let opts = self.connected(&RunOptions::default());
        let mask = sensitive_values(self.manifest(), &opts.secrets);
        self.operate(Op::Attach, events, mask, async |e: &Emitter<'_>| {
            let target = self.attacker(machine)?;
            let (_, mut cx) = self.existing(&opts, e).await?;
            if !any_target_running(&self.observe(&mut cx).await?.machines) {
                return Err(Error::NotRunning { run: self.name().to_string() });
            }
            let attached = self.driver.attach(&cx, target.as_attacker(), image, network).await?;
            let report = target.report(network, image, attached);
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
        let opts = self.connected(&RunOptions::default());
        let mask = sensitive_values(self.manifest(), &opts.secrets);
        self.operate(Op::Detach, events, mask, async |e: &Emitter<'_>| {
            let target = self.attacker(machine)?;
            let (_, cx) = self.existing(&opts, e).await?;
            self.driver.detach(&cx, target.as_attacker()).await?;
            Ok(Done {
                value: (),
                ok: true,
                data: target.detach_data(),
                state: None,
            })
        })
        .await
    }

    /// The command that opens an interactive shell into the attacker box: the runner builds it from
    /// the run's runtime (the Compose project, the Vagrant define), so the embedder opens the box
    /// by selection and never reconstructs a container name. A pure query — it runs nothing, emits
    /// nothing, and doesn't check the box is up (the embedder runs what it returns in its own
    /// terminal once the box is attached).
    ///
    /// # Errors
    ///
    /// Fails as [`Running::attach`] does for the box choice, and `unsupported` on a target that
    /// makes no ad-hoc box (a VM lab on a server or a cloud).
    pub fn shell_command(&self, machine: Option<&str>) -> Result<ShellCommand> {
        let target = self.attacker(machine)?;
        let (record, provider, params) = self.query_prelude()?;
        let silent = |_: &crate::events::Event| {};
        let emit = Emitter::new(self.name().as_str(), Op::Check, self.runner.config.clock.as_ref(), &silent);
        let mut cx = self.cx(params, &self.connection, &emit, provider)?;
        // What the run's start kept (its SSH jump, its slot), so the way in is the run's.
        if let Some(r) = &record {
            Self::bind_record(&mut cx, r);
        }
        self.driver.shell_command(&cx, target.as_attacker())
    }

    /// Which box an attach targets: the on-demand machine `machine` names, or the only one, else
    /// an ad-hoc box when the lab declares no on-demand machine at all.
    fn attacker(&self, machine: Option<&str>) -> Result<Target<'_>> {
        let mut on_demand = self.manifest().machines.iter().filter(|m| m.on_demand);
        if let Some(name) = machine {
            return on_demand
                .find(|m| m.name.as_str() == name)
                .map(Target::Machine)
                .ok_or_else(|| Error::MachineUnknown {
                    run: self.name().to_string(),
                    machine: name.to_owned(),
                });
        }
        if crate::driver::takes_adhoc_box(self.manifest()) {
            return Ok(Target::AdHoc);
        }
        match (on_demand.next(), on_demand.next()) {
            (Some(one), None) => Ok(Target::Machine(one)),
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
