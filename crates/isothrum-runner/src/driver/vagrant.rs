//! The `vagrant` driver: a Vagrantfile's VMs, on the provider the run picked among the
//! manifest's (`${run.provider}`). Every Vagrant command of the run, its actions' included, runs
//! with `VAGRANT_DOTFILE_PATH` at the manifest's `identity.vagrant.dotfile` (the run's state
//! folder), so a replaced run folder never loses track of its VMs. Its actions run the native
//! commands run-manifest-v0.md §6 fixes; its status is `lifecycle.status` (`vagrant status
//! --machine-readable`) matched to the manifest's machines by their define names. Ported from the
//! launcher's `runtime/vm.rs`, `runtime/vm/{status,recover}.rs` and `platform/exec/vagrant.rs`.
//!
//! A container lab in a VM (`docker-vm`, the manifest's `guest`) gets its ad-hoc attacker box as
//! a container inside that VM, on the lab's Docker network there (`adhoc`). A VM lab gets its
//! attacker box as a VM the runner adds to the running environment, as GNS3 adds a node to a live
//! topology: a definition written under the run's state folder, which the Vagrantfile's
//! `IsoloomAdHoc` hook loads, booted alone with `vagrant up <name> --provider=<p>` and destroyed
//! alone (`adhoc_vm`). A spec's own on-demand access machine keeps its path.

mod actions;
mod adhoc;
mod adhoc_vm;
mod forwarded;
mod orphans;
mod status;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use isoloom_manifest::{Action, Native, SnakeName, Step, Template, VagrantIdentity, VagrantProvider, access};

use super::{Attached, Attacker, Driver, Observed};
use crate::Result;
use crate::error::Error;
use crate::executor::{Cx, Echo, call_ok, step_env};
use crate::process::ProcessSpec;
use crate::scope::provider_id;
use crate::state::Op;
use crate::status::{MachineStatus, PublishedPort, ShellCommand};

/// The variable Vagrant reads its state folder from.
const DOTFILE_VAR: &str = "VAGRANT_DOTFILE_PATH";

/// How long one of an action's quick reads may take (`VBoxManage`, `vagrant status`).
const READ_TIMEOUT: Duration = Duration::from_secs(120);

/// How long hot-adding the access VM may take: booting it, and maybe fetching its box.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(1800);

/// How long hot-removing (destroying) the access VM may take.
const DETACH_TIMEOUT: Duration = Duration::from_secs(300);

/// The id the plugin check reports under.
const PLUGINS: &str = "plugins";

/// The provider that forwards no port to this computer: a VM's published ports answer at its
/// own address.
const ESXI: &str = "vmware_esxi";

/// The QEMU provider (vagrant-qemu), whose VMs need a chosen host port for their SSH forward.
const QEMU: &str = "qemu";

/// Where ports forwarded to this computer answer.
const LOOPBACK: &str = "127.0.0.1";

/// The `vagrant` driver of one manifest.
#[derive(Debug, Clone)]
pub(crate) struct Vagrant {
    identity: VagrantIdentity,
}

impl Vagrant {
    pub(crate) fn new(identity: VagrantIdentity) -> Vagrant {
        Vagrant { identity }
    }

    /// Vagrant's state folder for the run, absolute: the manifest's `dotfile`, else `.vagrant`
    /// next to the Vagrantfile (Vagrant's own default).
    fn dotfile(&self, cx: &Cx<'_>) -> Result<PathBuf> {
        match &self.identity.dotfile {
            Some(t) => Ok(cx.run.dir().join(cx.vars.render(t.template(), "identity.vagrant.dotfile")?)),
            None => Ok(cx.run.dir().join(".vagrant")),
        }
    }

    /// Whether the provider of `scope` takes its management commands elevated (Hyper-V).
    pub(crate) fn elevates(&self, scope: &crate::scope::Scope) -> bool {
        let asked = scope.provider.as_deref();
        self.identity
            .providers
            .iter()
            .any(|p| Some(provider_id(p.id).as_str()) == asked && p.elevation == isoloom_manifest::Elevation::Hyperv)
    }

    /// The run's provider among the manifest's.
    fn provider(&self, cx: &Cx<'_>) -> Result<&VagrantProvider> {
        let asked = cx.scope.provider.as_deref().ok_or_else(|| Error::Provider {
            provider: String::new(),
            reason: "a Vagrant run needs a provider: pass one of the manifest's (`virtualbox`, `libvirt`, …)".into(),
        })?;
        self.identity
            .providers
            .iter()
            .find(|p| provider_id(p.id) == asked)
            .ok_or_else(|| Error::Provider {
                provider: asked.to_owned(),
                reason: "isn't one of this run's providers".into(),
            })
    }

    /// `vagrant <args>` for the step `id`, in the manifest's folder, with the state folder, what
    /// the manifest delivers to that step (the ESXi host, a provider's params) and the relays'
    /// settings.
    fn vagrant(&self, cx: &Cx<'_>, id: &str, args: &[&str]) -> Result<ProcessSpec> {
        let mut env = self.env(cx)?;
        env.extend(step_env(cx, id));
        // An ESXi host behind the macOS relays: Vagrant talks to it through them.
        env.extend(cx.relayed.iter().flat_map(|r| r.env.iter().cloned()));
        Ok(ProcessSpec {
            program: "vagrant".into(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            cwd: Some(cx.run.dir().to_path_buf()),
            env,
            stdin: None,
        })
    }

    /// The address of each running VM that publishes a port, or that Compose runs in, by define
    /// name, as `vagrant ssh-config` gives it (ESXi forwards no port: the player reaches the VM
    /// itself). A VM whose address can't be read is left out, and its ports with it.
    async fn guest_addresses(&self, cx: &Cx<'_>, read: &status::Reading) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let manifest = cx.run.manifest();
        for m in manifest.machines.iter().filter(|m| answers(manifest, m)) {
            let define = define_name(m);
            if read.states.get(define).map(|s| status::machine_state(s)) != Some(crate::status::MachineState::Running) {
                continue;
            }
            let Ok(spec) = self.vagrant(cx, "status", &["ssh-config", define]) else {
                continue;
            };
            if let Ok(text) = call_ok(cx, "status", &spec, READ_TIMEOUT, Echo::Quiet).await
                && let Some(host) = status::host_name(&text)
            {
                out.insert(define.to_owned(), host);
            }
        }
        out
    }

    /// Boots the define `native` alone (`vagrant up <define> --provider=<p>`), on the run's own
    /// provider, the one its other VMs came up on, with the picked box in [`access::BOX_ENV`] (its
    /// `ENV.fetch` default otherwise): an access VM the lab declares, or the runner's ad-hoc VM.
    async fn boot(&self, cx: &Cx<'_>, native: &str, box_name: Option<&str>) -> Result<()> {
        let provider = cx.scope.provider.as_ref().map(|p| format!("--provider={p}"));
        let mut args = vec!["up", native];
        if let Some(flag) = &provider {
            args.push(flag);
        }
        let mut spec = self.vagrant(cx, Op::Attach.as_str(), &args)?;
        if let Some(box_name) = box_name {
            spec.env.push((access::BOX_ENV.to_owned(), box_name.to_owned()));
        }
        call_ok(cx, Op::Attach.as_str(), &spec, ATTACH_TIMEOUT, Echo::Shown).await.map(drop)
    }

    /// The plugins `vagrant plugin list --machine-readable` names.
    async fn plugins(&self, cx: &Cx<'_>) -> Result<Vec<String>> {
        let spec = self.vagrant(cx, PLUGINS, &["plugin", "list", "--machine-readable"])?;
        let out = call_ok(cx, PLUGINS, &spec, READ_TIMEOUT, Echo::Quiet).await?;
        Ok(status::plugins(&out))
    }
}

impl Driver for Vagrant {
    /// The provider must be one of the run's, exist on this host, not be refused by Isoloom for
    /// this run, and have its plugin.
    async fn preflight(&self, cx: &Cx<'_>) -> Result<()> {
        let p = self.provider(cx)?;
        let id = provider_id(p.id);
        let refuse = |reason: String| Error::Provider { provider: id.clone(), reason };
        if !p.hosts.contains(&cx.scope.host) {
            return Err(refuse("doesn't exist on this host".into()));
        }
        if let Some(why) = &p.refusal {
            return Err(refuse(format!("this run can't run there: {why}")));
        }
        if let Some(plugin) = &p.plugin
            && !self.plugins(cx).await?.iter().any(|name| name == plugin.as_str())
        {
            return Err(refuse(format!("needs the Vagrant plugin `{plugin}`: `vagrant plugin install {plugin}`")));
        }
        Ok(())
    }

    fn prepare(&self, cx: &Cx<'_>) -> Result<()> {
        // Its parent is the run's own folder, owner-only; Vagrant makes the folder itself.
        let dotfile = self.dotfile(cx)?;
        match dotfile.parent() {
            Some(parent) if parent.starts_with(cx.run.runner.config.home.as_path()) => crate::store::private_dir(parent).map_err(Error::io(parent, "created")),
            _ => Ok(()),
        }
    }

    fn env(&self, cx: &Cx<'_>) -> Result<Vec<(String, String)>> {
        let mut env = vec![(DOTFILE_VAR.to_owned(), self.dotfile(cx)?.display().to_string())];
        // The host ports a fresh start pinned (kept in the run's record): as a JSON object the
        // Vagrantfile reads for its forwards, defaulting each to the port's own `publish` number.
        let forwards = cx.vars.forwards();
        if !forwards.is_empty() {
            let json = serde_json::to_string(forwards).map_err(|e| Error::Param {
                name: super::HOSTPORTS.to_owned(),
                reason: e.to_string(),
            })?;
            env.push((super::HOSTPORTS.to_owned(), json));
        }
        Ok(env)
    }

    async fn action(&self, cx: &Cx<'_>, step: &Step, action: Action, with: &BTreeMap<SnakeName, Template>) -> Result<()> {
        match action {
            Action::VagrantReconcileProvider => actions::reconcile_provider(self, cx, step).await,
            Action::VagrantDiscardAbortedSaved => actions::discard_aborted_saved(cx, step).await,
            Action::VagrantRemoveOrphans => orphans::remove(cx, step).await,
            Action::VagrantResetState => actions::reset_state(self, cx),
            Action::VagrantWakeController => actions::wake_controller(self, cx, step, with).await,
            Action::VagrantParkController => actions::park_controller(self, cx, step, with).await,
            Action::VagrantCheckForwardedPorts => forwarded::check(cx, step).await,
            other => Err(super::foreign_action("Vagrant", step, other)),
        }
    }

    async fn observe(&self, cx: &Cx<'_>) -> Result<Observed> {
        let out = super::status_report(cx).await?;
        let read = status::parse(&out);
        let guests = if cx.scope.provider.as_deref() == Some(ESXI) {
            Some(self.guest_addresses(cx, &read).await)
        } else {
            None
        };
        let mut observed = machines(cx.run.manifest(), &read, guests.as_ref(), cx.vars.forwards());
        // The ad-hoc attacker box the runner stood up itself, when the lab declares no access
        // machine of its own, so a run that has one shows it like any other machine (its state,
        // its address, the shell into it). A container lab in a VM: a container in there with no
        // Compose service, which the guest's `ps` never lists, probed while the VM runs. A VM lab:
        // the VM the runner added, read from `vagrant status` with the definition it wrote.
        if super::takes_adhoc_box(cx.run.manifest()) {
            match adhoc::guest_vm(cx) {
                Some(vm) => {
                    if read.states.get(&vm).map(|s| status::machine_state(s)) == Some(crate::status::MachineState::Running)
                        && let Some(access) = adhoc::status(self, cx, &vm).await
                    {
                        observed.machines.push(access);
                    }
                }
                None => observed.machines.extend(adhoc_vm::status(self, cx, &read)),
            }
        }
        Ok(observed)
    }

    fn after_down(&self, _cx: &Cx<'_>) -> Result<()> {
        // `vagrant.reset_state`, a step of the manifest's `down`, clears the state folder.
        Ok(())
    }

    /// Hot-adds the attacker box. A manifest access VM, which the initial `up` left defined but
    /// `autostart: false`, boots: `vagrant up <name> --provider=<p>`, with the picked box in
    /// [`access::BOX_ENV`] (its `ENV.fetch` default otherwise); its network is the VM's, fixed by
    /// the manifest, so it is not chosen here. An ad-hoc box with no machine goes inside the VM of
    /// a container lab in a VM, as a container on the lab's Docker network there
    /// ([`adhoc::attach`]), or, on a VM lab, is a VM the runner adds to the running environment
    /// on the chosen network ([`adhoc_vm::attach`]).
    async fn attach(&self, cx: &Cx<'_>, attacker: Attacker<'_>, image: Option<&str>, network: Option<&str>) -> Result<Attached> {
        let native = match target(cx, attacker) {
            Target::AdHocIn(vm) => return adhoc::attach(self, cx, &vm, image, network).await,
            Target::AdHocVm => return adhoc_vm::attach(self, cx, image, network).await,
            Target::Machine(native) => native,
        };
        // A lab's own access VM: the attach's image is the Vagrant box it boots from.
        self.boot(cx, &native, image).await?;
        Ok(Attached::default())
    }

    /// Hot-removes the attacker box, leaving the rest of the run up: a manifest access VM is
    /// destroyed (`vagrant destroy -f <name>`), an ad-hoc container removed from the VM it is in
    /// (best effort, idempotent: a box already gone is no error), an ad-hoc VM destroyed and its
    /// definition removed (none defined: nothing to do).
    async fn detach(&self, cx: &Cx<'_>, attacker: Attacker<'_>) -> Result<()> {
        let native = match target(cx, attacker) {
            Target::AdHocIn(vm) => return adhoc::detach(self, cx, &vm).await,
            Target::AdHocVm => return adhoc_vm::detach(self, cx).await,
            Target::Machine(native) => native,
        };
        let spec = self.vagrant(cx, Op::Detach.as_str(), &["destroy", "-f", &native])?;
        call_ok(cx, Op::Detach.as_str(), &spec, DETACH_TIMEOUT, Echo::Shown).await?;
        Ok(())
    }

    /// The command that opens an interactive shell into the attacker box: `vagrant ssh <define>`
    /// into a manifest access VM or the ad-hoc VM the runner added (then `docker exec` into the
    /// container that VM runs, if any), `vagrant ssh <vm> -c "docker exec …"` into an ad-hoc
    /// container in the VM of a container lab. Either runs in the run's
    /// folder with the run's Vagrant environment (its state folder, its forwarded ports), which
    /// Vagrant needs to find the VM.
    fn shell_command(&self, cx: &Cx<'_>, attacker: Attacker<'_>) -> Result<ShellCommand> {
        let native = match target(cx, attacker) {
            Target::AdHocIn(vm) => return adhoc::shell_command(self, cx, &vm),
            // The runner's own VM: `vagrant ssh` into it, then into the container it runs, if any.
            Target::AdHocVm => return adhoc_vm::shell_command(self, cx),
            Target::Machine(native) => native,
        };
        Ok(ShellCommand {
            program: "vagrant".into(),
            args: vec!["ssh".into(), native],
            cwd: Some(cx.run.dir().to_path_buf()),
            env: self.env(cx)?,
        })
    }
}

/// What a hot-add acts on in a Vagrant run: a manifest access VM by its define name; for a
/// container lab in a VM, the define name of the VM an ad-hoc container goes in; for a VM lab,
/// the ad-hoc VM the runner adds itself.
enum Target {
    Machine(String),
    AdHocIn(String),
    AdHocVm,
}

/// `attacker`'s target on this run.
fn target(cx: &Cx<'_>, attacker: Attacker<'_>) -> Target {
    match attacker {
        Attacker::Machine(native) => Target::Machine(native.to_owned()),
        Attacker::AdHoc => adhoc::guest_vm(cx).map_or(Target::AdHocVm, Target::AdHocIn),
    }
}

/// Whether the ports of `m` answer somewhere: it publishes one, or it is the VM Compose runs in
/// (the containers' ports answer where it does).
fn answers(manifest: &isoloom_manifest::Manifest, m: &isoloom_manifest::Machine) -> bool {
    m.ports.iter().any(|p| p.publish.is_some()) || manifest.guest.as_ref().is_some_and(|g| g.machine == m.name)
}

/// A machine's name in the Vagrantfile.
fn define_name(m: &isoloom_manifest::Machine) -> &str {
    match &m.native {
        Some(Native::Vagrant { vagrant, .. }) => vagrant.as_str(),
        _ => m.name.as_str(),
    }
}

/// The host ports a fresh local Vagrant start pins (`AnyDriver::forward_ports`), by
/// `"<machine>/<port>"`, ready to be kept in the run's record.
pub(super) fn pick_forwards(cx: &Cx<'_>) -> BTreeMap<String, u16> {
    forwarded::pick(cx)
}

/// Every machine of the manifest as Vagrant reports it, by its define name. Locally a published
/// port is forwarded to this computer at the number `chosen` for it (keyed `"<machine>/<port>"`),
/// else its `preferred` fallback. With `guests` (ESXi, which forwards nothing), it answers at its
/// VM's address, on the VM's own port, while that address is known.
fn machines(
    manifest: &isoloom_manifest::Manifest,
    read: &status::Reading,
    guests: Option<&BTreeMap<String, String>>,
    chosen: &BTreeMap<String, u16>,
) -> Observed {
    let mut out = Observed::default();
    for m in &manifest.machines {
        let define = define_name(m);
        let state = read
            .states
            .get(define)
            .map_or(crate::status::MachineState::Absent, |s| status::machine_state(s));
        let host = match guests {
            Some(guests) => guests.get(define).cloned(),
            None => Some(LOOPBACK.to_owned()),
        };
        let mut published = Vec::new();
        let host = host.filter(|_| state != crate::status::MachineState::Absent);
        if let Some(host) = host.as_ref().filter(|_| answers(manifest, m)) {
            out.hosts.insert(m.name.to_string(), host.clone());
        }
        if host.is_some() {
            for p in &m.ports {
                let Some(publish) = p.publish.as_ref() else {
                    continue;
                };
                let host_port = if guests.is_some() {
                    p.port.get()
                } else {
                    // The runner's pick for this port; else its `preferred` number (a fixed port);
                    // else the port itself. Gating on `publish` (not `preferred`) keeps an ephemeral
                    // port with no `publish:` number of its own, whose host port is the pick.
                    chosen
                        .get(&forwarded::key(m, p))
                        .copied()
                        .or_else(|| publish.preferred.map(std::num::NonZeroU16::get))
                        .unwrap_or_else(|| p.port.get())
                };
                published.push(PublishedPort {
                    port: p.port.get(),
                    protocol: p.kind.transport(),
                    host_port,
                });
                out.published.insert((m.name.to_string(), p.name.to_string()), host_port);
            }
        }
        out.machines.push(MachineStatus {
            name: m.name.to_string(),
            role: m.role,
            state,
            addresses: super::static_addresses(m),
            published,
        });
    }
    out
}

#[cfg(test)]
mod tests;
