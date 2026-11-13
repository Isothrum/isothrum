//! The `vagrant` driver: a Vagrantfile's VMs, on the provider the run picked among the
//! manifest's (`${run.provider}`). Every Vagrant command of the run, its actions' included, runs
//! with `VAGRANT_DOTFILE_PATH` at the manifest's `identity.vagrant.dotfile` (the run's state
//! folder), so a replaced run folder never loses track of its VMs. Its actions run the native
//! commands each one names; its status is `lifecycle.status` (`vagrant status --machine-readable`)
//! matched to the manifest's machines by their define names, read again while another Vagrant
//! process holding a machine's lock refuses it ([`lock`]).

mod actions;
mod boxes;
pub(super) mod forwarded;
mod lock;
mod orphans;
mod status;

pub(in crate::driver) use boxes::before_step as boxes_before_step;
pub(super) use lock::run_step;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use isoloom_manifest::{Action, Machine, Native, Provider, Step, VagrantIdentity, VagrantProvider, contract};

use super::{Driver, GuestUse, Observed, native_name};
use crate::Result;
use crate::console::ShellCommand;
use crate::encoding::wire_name;
use crate::error::Error;
use crate::executor::{Cx, Echo, call_ok, with_delivered};
use crate::process::ProcessSpec;
use crate::state::Op;
use crate::status::{MachineStatus, PublishedPort};

/// The variable Vagrant reads its state folder from.
const DOTFILE_VAR: &str = "VAGRANT_DOTFILE_PATH";

/// How long one of an action's quick reads (`VBoxManage`, `vagrant status`) may go without
/// progress (as every tool call's limit counts).
const READ_LIMIT: Duration = Duration::from_secs(120);

/// How long hot-adding the access machine may go without progress: booting its VM, and maybe
/// fetching its box.
const ATTACH_LIMIT: Duration = Duration::from_secs(1800);

/// How long restarting a running VM may go without progress: shutting it down, then booting it.
const RELOAD_LIMIT: Duration = Duration::from_secs(1800);

/// The id the plugin check reports under.
const PLUGINS: &str = "plugins";

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
        self.identity
            .providers
            .iter()
            .any(|p| scope.runs_on(p.id) && p.elevation == isoloom_manifest::Elevation::Hyperv)
    }

    /// The run's provider among the manifest's.
    fn provider(&self, cx: &Cx<'_>) -> Result<&VagrantProvider> {
        let theirs = || {
            self.identity
                .providers
                .iter()
                .map(|p| format!("`{}`", wire_name(&p.id)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let Some(asked) = cx.scope.provider else {
            return Err(Error::NoProvider { providers: theirs() });
        };
        self.identity.providers.iter().find(|p| p.id == asked).ok_or_else(|| Error::Provider {
            provider: wire_name(&asked),
            reason: format!("isn't one of this run's providers ({})", theirs()),
        })
    }

    /// `vagrant <args>` for the step `id`, in the manifest's folder, with the state folder, what
    /// the manifest delivers to that step (the ESXi host, a provider's params) and the relays'
    /// settings.
    fn vagrant(&self, cx: &Cx<'_>, id: &str, args: &[&str]) -> Result<ProcessSpec> {
        // An ESXi host behind the macOS relays: Vagrant talks to it through them.
        let env = with_delivered(cx, id, self.env(cx)?);
        Ok(ProcessSpec {
            program: "vagrant".into(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            cwd: Some(cx.run.dir().to_path_buf()),
            env,
            stdin: None,
            tools: None,
        })
    }

    /// `vagrant <args>` for a console in the embedder's terminal: with the run's own variables
    /// ([`Driver::env`]) but the consoles the runner chose, and nothing the manifest delivers to a
    /// step, whose values (and passwords) would show in the printed command.
    fn console(&self, cx: &Cx<'_>, args: &[&str]) -> Result<ProcessSpec> {
        let mut spec = ProcessSpec::new("vagrant", args.iter().copied());
        spec.env = self.env(cx)?;
        spec.env.retain(|(k, _)| k != contract::CONSOLES);
        Ok(spec)
    }

    /// The address of each running VM that publishes a port, or that Compose runs in, by define
    /// name, as `vagrant ssh-config` gives it (ESXi forwards no port: the user reaches the VM
    /// itself). A VM whose address can't be read is left out, and its ports with it; why is
    /// traced.
    async fn guest_addresses(&self, cx: &Cx<'_>, read: &status::Reading) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let manifest = cx.manifest();
        for m in manifest.machines.iter().filter(|m| answers(manifest, m)) {
            let define = native_name(m);
            if read.states.get(define).map(|s| status::machine_state(s)) != Some(crate::status::MachineState::Running) {
                continue;
            }
            let asked = match self.vagrant(cx, "status", &["ssh-config", define]) {
                Ok(spec) => call_ok(cx, "status", &spec, READ_LIMIT, Echo::Quiet).await,
                Err(e) => Err(e),
            };
            match asked.map(|text| status::host_name(&text)) {
                Ok(Some(host)) => drop(out.insert(define.to_owned(), host)),
                Ok(None) => tracing::warn!(define, "`vagrant ssh-config` named no host: the VM's address and ports are left out"),
                Err(e) => tracing::warn!(define, error = %e, "the VM's address couldn't be read: it and its ports are left out"),
            }
        }
        out
    }

    /// The plugins `vagrant plugin list --machine-readable` names.
    async fn plugins(&self, cx: &Cx<'_>) -> Result<Vec<String>> {
        let spec = self.vagrant(cx, PLUGINS, &["plugin", "list", "--machine-readable"])?;
        let out = call_ok(cx, PLUGINS, &spec, READ_LIMIT, Echo::Quiet).await?;
        Ok(crate::vagrant_plugins(&out).into_iter().map(|(name, _)| name).collect())
    }
}

impl Driver for Vagrant {
    /// The provider must be one of the run's, exist on this host, not be refused by Isoloom for
    /// this run, and have its plugin.
    async fn preflight(&self, cx: &Cx<'_>) -> Result<()> {
        let p = self.provider(cx)?;
        let id = wire_name(&p.id);
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
            Some(parent) if parent.starts_with(cx.home()) => crate::store::create_dir(parent)?,
            _ => {}
        }
        Ok(())
    }

    fn env(&self, cx: &Cx<'_>) -> Result<Vec<(String, String)>> {
        let mut env = vec![(DOTFILE_VAR.to_owned(), self.dotfile(cx)?.display().to_string())];
        // The host ports a fresh start pinned (kept in the run's record): as a JSON object the
        // Vagrantfile reads for its forwards, defaulting each to the port's own `publish` number.
        let forwards = cx.vars.vm_pins();
        if !forwards.is_empty() {
            let json: serde_json::Map<String, serde_json::Value> = forwards.iter().map(|(k, port)| (k.clone(), (*port).into())).collect();
            env.push((contract::HOSTPORTS.to_owned(), serde_json::Value::Object(json).to_string()));
        }
        // The consoles the runner chose, passwords and all: the run's own Vagrant processes only
        // (a console's command, which an embedder is handed, leaves them out: `console`).
        env.extend(crate::graphical::chosen::var(cx)?);
        Ok(env)
    }

    async fn action(&self, cx: &Cx<'_>, list: &str, step: &Step, action: Action) -> Result<()> {
        match action {
            Action::VagrantReconcileProvider => actions::reconcile_provider(self, cx, step).await,
            Action::VagrantDiscardAbortedSaved => actions::discard_aborted_saved(cx, step).await,
            Action::VagrantRemoveOrphans => orphans::remove(cx, step).await,
            Action::VagrantResetState => actions::reset_state(self, cx),
            Action::VagrantWakeController => actions::wake_controller(self, cx, list, step).await,
            Action::VagrantParkController => actions::park_controller(self, cx, list, step).await,
            Action::VagrantCheckForwardedPorts => forwarded::check(cx, step).await,
            other => Err(super::foreign_action(isoloom_manifest::Driver::Vagrant, step, other)),
        }
    }

    async fn observe(&self, cx: &Cx<'_>) -> Result<Observed> {
        let out = match lock::read_status(cx).await? {
            lock::StatusRead::Read(out) => out,
            lock::StatusRead::Busy(defines) => return Ok(busy(cx.manifest(), &defines)),
        };
        let read = status::parse(&out);
        let at_vm = cx.scope.runs_on(Provider::VmwareEsxi);
        let hosts = if at_vm { self.guest_addresses(cx, &read).await } else { local_hosts(cx)? };
        Ok(machines(cx.manifest(), &read, &hosts, at_vm, cx.vars.vm_pins()))
    }

    fn after_down(&self, cx: &Cx<'_>) -> impl Future<Output = Result<()>> {
        // `vagrant.reset_state`, a step of the manifest's `down`, clears the state folder; the
        // consoles the runner chose go with the run.
        std::future::ready(crate::graphical::chosen::forget(cx))
    }

    /// `vagrant reload <define> --no-provision`: the VM boots with what the Vagrantfile now says,
    /// its synced folders, networks, forwarded ports, size and provider settings, which Vagrant
    /// applies only at boot. Its provisioners are left to the steps that follow.
    async fn restart(&self, cx: &Cx<'_>, machine: &Machine, id: &str) -> Result<()> {
        let spec = self.vagrant(cx, id, &["reload", define_of(machine, "restarting")?, "--no-provision"])?;
        lock::call_ok_retrying(cx, id, &spec, RELOAD_LIMIT, Echo::Shown).await.map(drop)
    }

    /// Hot-adds the access machine, a VM the initial `up` left defined but `autostart: false`:
    /// its define booted alone (`vagrant up <define> --provider=<p>`) on the run's own provider,
    /// the one its other VMs came up on, with the picked box (the attach's image) in
    /// [`contract::ACCESS_BOX`] (its `ENV.fetch` default otherwise). A VM's networks are the
    /// manifest's, fixed in the Vagrantfile: a chosen one is refused.
    async fn attach(&self, cx: &Cx<'_>, machine: &Machine, image: Option<&str>, network: Option<&str>) -> Result<()> {
        let define = define_of(machine, "hot-adding")?;
        if let Some(network) = network {
            return Err(Error::Unsupported {
                what: format!(
                    "joining `{}` to network `{network}` on a Vagrant run (a VM's networks are the Vagrantfile's)",
                    machine.name
                ),
            });
        }
        let provider = cx.scope.provider.map(|p| format!("--provider={}", wire_name(&p)));
        let mut args = vec!["up", define];
        if let Some(flag) = &provider {
            args.push(flag);
        }
        let mut spec = self.vagrant(cx, Op::Attach.as_str(), &args)?;
        if let Some(box_name) = image {
            spec.env.push((contract::ACCESS_BOX.to_owned(), box_name.to_owned()));
        }
        lock::call_ok_retrying(cx, Op::Attach.as_str(), &spec, ATTACH_LIMIT, Echo::Shown)
            .await
            .map(drop)
    }

    /// A VM of the run destroyed alone (`vagrant destroy -f <define>`): the access machine's at a
    /// detach, a machine's at its retire, before its define leaves the Vagrantfile (once it has,
    /// Vagrant no longer knows the VM).
    async fn retire(&self, cx: &Cx<'_>, machine: &Machine) -> Result<()> {
        let op = cx.events.call_id();
        let spec = self.vagrant(cx, op, &["destroy", "-f", define_of(machine, "taking out")?])?;
        lock::call_ok_retrying(cx, op, &spec, super::REMOVE_LIMIT, Echo::Shown).await.map(drop)
    }

    /// `vagrant ssh` into the machine's VM; as `user`, `vagrant ssh <define> -c` a `sudo -i`
    /// login as it ([`super::shells::login_shell_as`]: Vagrant gives `-c` a terminal).
    fn console_command(&self, cx: &Cx<'_>, machine: &Machine, user: Option<&str>) -> Result<ShellCommand> {
        let define = define_of(machine, "a console into")?;
        let spec = match user {
            None => self.console(cx, &["ssh", define])?,
            Some(user) => self.console(cx, &["ssh", define, "-c", &super::shells::login_shell_as(user)])?,
        };
        Ok(super::console_in_run(cx, spec))
    }

    /// `vagrant ssh <define> -c <script>` (Vagrant gives `-c` a terminal by default): a console
    /// gets the run's own variables only ([`Driver::env`]), never what the manifest delivers to a
    /// step, since the embedder is handed its command; a step of the operation gets what the
    /// manifest delivers to it ([`GuestUse`]).
    fn in_guest(&self, cx: &Cx<'_>, vm: &Machine, script: &str, use_: GuestUse) -> Result<ProcessSpec> {
        let args = ["ssh", define_of(vm, "running a script in")?, "-c", script];
        match use_ {
            GuestUse::Console => self.console(cx, &args),
            GuestUse::Step => self.vagrant(cx, cx.events.call_id(), &args),
        }
    }
}

/// The Vagrant define `machine` is, for `what`: refused for a Compose service with no VM to run
/// in, and for a machine whose manifest doesn't say how it runs.
fn define_of<'m>(machine: &'m Machine, what: &str) -> Result<&'m str> {
    match &machine.native {
        Some(Native::Vagrant { vagrant, .. }) => Ok(vagrant.as_str()),
        Some(Native::Compose { .. }) => Err(super::no_vm_for(what, machine)),
        None => Err(Error::Unsupported {
            what: format!("{what} `{}`: the run doesn't say how it runs", machine.name),
        }),
    }
}

/// Whether the ports of `m` answer somewhere: it publishes one, or it is the VM Compose runs in
/// (the containers' ports answer where it does).
fn answers(manifest: &isoloom_manifest::Manifest, m: &Machine) -> bool {
    m.ports.iter().any(|p| p.publish.is_some()) || manifest.guest.as_ref().is_some_and(|g| g.machine == m.name)
}

/// Where each VM forwarding to this computer answers, by define name: the address the manifest
/// publishes its ports on ([`super::publish_host`]).
fn local_hosts(cx: &Cx<'_>) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for m in &cx.manifest().machines {
        out.insert(native_name(m).to_owned(), super::publish_host(cx, m)?);
    }
    Ok(out)
}

/// Every machine of the manifest as Vagrant reports it, by its define name, each answering at its
/// address among `hosts` while that address is known. Locally a published port is forwarded to
/// this computer at the number `chosen` for it (keyed `"<machine>/<port>"`), else its `preferred`
/// fallback. `at_vm` (ESXi, which forwards nothing), it answers on the VM's own port.
fn machines(
    manifest: &isoloom_manifest::Manifest,
    read: &status::Reading,
    hosts: &BTreeMap<String, String>,
    at_vm: bool,
    chosen: &BTreeMap<String, u16>,
) -> Observed {
    let mut out = Observed::default();
    for m in &manifest.machines {
        let define = native_name(m);
        let state = read
            .states
            .get(define)
            .map_or(crate::status::MachineState::Absent, |s| status::machine_state(s));
        let host = hosts.get(define).filter(|_| state != crate::status::MachineState::Absent);
        if let Some(host) = host.filter(|_| answers(manifest, m)) {
            out.hosts.insert(m.name.to_string(), host.clone());
        }
        let mut published = Vec::new();
        if host.is_some() {
            for (name, port) in published_ports(m, at_vm, chosen) {
                out.published.insert(
                    crate::vars::PortKey {
                        machine: m.name.to_string(),
                        port: name,
                    },
                    port.host_port,
                );
                published.push(port);
            }
        }
        out.machines.push(MachineStatus {
            name: m.name.to_string(),
            role: m.role,
            state,
            addresses: super::static_addresses(m),
            published,
            console: None,
        });
    }
    out
}

/// A query's read another process's lock refused: the manifest's machines Vagrant named
/// (`defines`, by define name) busy, by their names; nothing else is read.
fn busy(manifest: &isoloom_manifest::Manifest, defines: &[String]) -> Observed {
    let named = manifest
        .machines
        .iter()
        .filter(|m| defines.iter().any(|d| d == native_name(m)))
        .map(|m| m.name.to_string())
        .collect();
    Observed {
        busy: Some(named),
        ..Observed::default()
    }
}

/// The ports `m` publishes, by name, each with its host port: on a VM that answers at its own address
/// (`at_vm`, ESXi), the port itself; locally, the one it is forwarded from, the runner's pick for
/// it among `chosen` first ([`forwarded::host_port`]). An ephemeral port with no `publish:`
/// number of its own is kept: its host port is the pick.
fn published_ports(m: &Machine, at_vm: bool, chosen: &BTreeMap<String, u16>) -> Vec<(String, PublishedPort)> {
    m.ports
        .iter()
        .filter_map(|p| {
            let host_port = if at_vm {
                p.publish.as_ref().map(|_| p.port.get())?
            } else {
                forwarded::host_port(m, p, chosen)?
            };
            let port = PublishedPort {
                port: p.port.get(),
                protocol: p.kind.transport(),
                host_port,
            };
            Some((p.name.to_string(), port))
        })
        .collect()
}

#[cfg(test)]
mod tests;
