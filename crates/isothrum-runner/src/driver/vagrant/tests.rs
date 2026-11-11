//! The vagrant driver through a fake Vagrant: every `vagrant` and `VBoxManage` call answered
//! from a small model of a Vagrantfile's VMs (the `web-vm` fixture's `web` and controller),
//! which keeps Vagrant's record where the call's `VAGRANT_DOTFILE_PATH` says, as Vagrant does.

mod controller;
mod disruptions;
mod failures;
mod lifecycle;
mod locks;
mod outputs;
mod ports;
mod setups;
mod updates;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use isoloom_manifest::HostOs;

use crate::net::FakePorts;
use crate::process::fake::{Call, FakeProcesses, Reply};
use crate::secrets::{SecretValue, Secrets};
use crate::testing::{RunFolder, WEB_VM, modelled, processes, runner_config, virtualbox_options, with_boot_digest};
use crate::{ErrorCode, Park, Recorder, RunOptions, RunState, Runner};

use self::outputs::{ESXI_ALREADY_OFF, FOG_WARNING, VBOX_LOCKED, VBOX_WEDGED, index_locked_status, locked_halt, locked_status};
use self::setups::{each_publish, ephemeral_no_preferred_manifest, esxi_secrets, fixed_manifest, on_esxi, with_esxi_plugin, with_vm_added};

const CONTROLLER: &str = "isoloom-controller";
const SECRET: &str = "token-vm-0001";

/// A small model of the Vagrantfile's VMs and of what the host has installed.
#[derive(Debug, Default)]
struct Engine {
    /// Each VM's state label (absent: `not_created`).
    states: BTreeMap<String, String>,
    /// The provider the VMs were created with.
    provider: Option<String>,
    /// Vagrant's plugins.
    plugins: Vec<String>,
    /// What `vagrant up --provider` prints, then whether it fails, for each start in turn.
    ups: Vec<(String, bool)>,
    /// VirtualBox's VMs, by name, with their state.
    vbox: BTreeMap<String, String>,
    /// Parallels' VMs, by name; `None`: `prlctl list` fails.
    parallels: Option<std::collections::BTreeSet<String>>,
    /// How many times the controller still says it halts.
    halting: u32,
    /// How many times SSH to the controller still fails on its way down (then it is off).
    going_down: u32,
    /// How long the controller takes to answer the halting probe.
    probe_delay: std::time::Duration,
    /// What `vagrant --version` says (default: 2.4.9).
    version: Option<String>,
    /// What `vagrant halt <controller>` does: `None` halts it; `Some(state)` fails as libvirt's
    /// does once SSH is closed, the controller left in `state`.
    halt_fails: Option<&'static str>,
    /// The controller powers itself off before `vagrant halt` (ESXi refuses that halt).
    esxi_off_before_halt: bool,
    /// How long `vagrant halt <controller>` takes.
    halt_delay: std::time::Duration,
    /// `vagrant ssh-config` fails whatever the VM's state.
    ssh_config_fails: bool,
    /// The VMs a regenerated Vagrantfile defines beyond the fixture's, which `vagrant up` brings up.
    added: Vec<String>,
    /// How many `vagrant status` calls another Vagrant process's machine lock still refuses.
    locked_reads: u32,
    /// How many `vagrant halt <controller>` calls another Vagrant process's machine lock still
    /// refuses.
    locked_halts: u32,
    /// How long `vagrant up --provider` takes (booting and provisioning the VMs), holding their
    /// locks all along, as Vagrant does: a `vagrant status` meanwhile is refused.
    up_takes: std::time::Duration,
    /// Until when the `vagrant up` under way holds the VMs' locks.
    up_holds_until: Option<tokio::time::Instant>,
    /// How many `vagrant status` calls after the controller's halt Vagrant's machine index lock
    /// refuses, as on libvirt ([`index_locked_status`]).
    index_locks_after_halt: u32,
    /// How many `vagrant status` calls Vagrant's machine index lock still refuses.
    index_locked_reads: u32,
    /// The VM another Vagrant process acts on (its `vagrant up`, not this runner's), if any: every
    /// `vagrant status` meanwhile is refused with Vagrant's action lock on it.
    acting_on: Option<&'static str>,
    /// The Vagrant commands, by their verb, another Vagrant process's machine lock still refuses.
    refusals: BTreeMap<&'static str, Refusal>,
    /// VirtualBox answers no `showvminfo` (its service wedged).
    vbox_wedged: bool,
    /// What fails that otherwise works.
    fails: std::collections::BTreeSet<Fails>,
    /// VirtualBox's default machine folder (`VBoxManage list systemproperties`), if it has one.
    machine_folder: Option<PathBuf>,
}

/// A call of a clean-up that fails in a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Fails {
    /// The hypervisor deleting a VM (`VBoxManage unregistervm`, `prlctl delete`).
    Delete,
    /// `vagrant destroy --force`.
    Destroy,
}

/// A Vagrant command refused for another process's machine lock, `times` more times.
#[derive(Debug)]
struct Refusal {
    times: u32,
    /// What it printed before the refusal (what it did first).
    before: &'static str,
    /// What the refused try did to the VMs first.
    did: fn(&mut Engine),
    /// How long it ran before the refusal.
    took: std::time::Duration,
    /// Vagrant's refusal, on standard error.
    said: String,
}

impl Refusal {
    /// Refused `times` times by `vm`'s machine index lock (`MachineLocked`, libvirt) at once.
    fn index(times: u32, vm: &str) -> Refusal {
        Refusal {
            times,
            before: "",
            did: |_| {},
            took: std::time::Duration::ZERO,
            said: format!(
                "{FOG_WARNING}\nVagrant can't use the requested machine because it is locked! This\nmeans that another Vagrant \
                 process is currently reading or modifying\nthe machine. Please wait for that Vagrant process to end and try\n\
                 again. Details about the machine are shown below:\n\nName: {vm}\nProvider: libvirt"
            ),
        }
    }

    /// Refused `times` times by `vm`'s action lock (`MachineActionLockedError`) for `action`,
    /// before it acted.
    fn action(times: u32, action: &str, vm: &str) -> Refusal {
        Refusal {
            said: format!(
                "An action '{action}' was attempted on the machine '{vm}',\nbut another process is already executing an action on \
                 the machine.\nVagrant locks each machine for access by only one process at a time."
            ),
            ..Refusal::index(times, vm)
        }
    }

    /// Refused after printing `before` and doing `did`.
    fn after(self, before: &'static str, did: fn(&mut Engine)) -> Refusal {
        Refusal { before, did, ..self }
    }
}

struct Fixture {
    folder: RunFolder,
    manifest: PathBuf,
    fake: Arc<FakeProcesses>,
    engine: Arc<Mutex<Engine>>,
    runner: Runner,
}

fn fixture(host: HostOs) -> Fixture {
    fixture_from(host, WEB_VM)
}

fn fixture_from(host: HostOs, manifest: &str) -> Fixture {
    let folder = RunFolder::new("vagrant", manifest);
    let (engine, fake) = modelled(answer);
    let ports: Arc<dyn crate::HostPorts> = Arc::new(FakePorts::new([41000, 41001]));
    let runner = Runner::new(runner_config(folder.home.path(), processes(&fake), ports, host)).unwrap();
    Fixture {
        manifest: folder.manifest.clone(),
        folder,
        fake,
        engine,
        runner,
    }
}

impl Fixture {
    fn engine(&self) -> std::sync::MutexGuard<'_, Engine> {
        self.engine.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn state(&self, vm: &str) -> String {
        self.engine().states.get(vm).cloned().unwrap_or_else(|| "not_created".into())
    }

    /// The `vagrant` calls, as lines.
    fn vagrant(&self) -> Vec<String> {
        self.fake
            .lines()
            .into_iter()
            .filter(|l| l.starts_with("vagrant ") && !l.contains("--machine-readable") && !l.contains("--version"))
            .collect()
    }
}

/// The options the tests start with: VirtualBox, the secret given.
fn opts() -> RunOptions {
    virtualbox_options(SECRET)
}

/// The VMs of the fixture's Vagrantfile.
const VMS: [&str; 2] = ["web", CONTROLLER];

/// Vagrant's record of `vm` on `provider` in `dotfile`, as `vagrant up` writes it.
fn record(dotfile: &Path, vm: &str, provider: &str) {
    let dir = dotfile.join("machines").join(vm).join(provider);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("id"), "0000").unwrap();
}

/// The fake Vagrant's answer to `call`.
fn answer(engine: &Mutex<Engine>, call: &Call) -> Reply {
    let mut e = engine.lock().unwrap_or_else(PoisonError::into_inner);
    let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
    if call.program == "vagrant"
        && let Some(refused) = locked_out(&mut e, &args)
    {
        return refused;
    }
    match (call.program.as_str(), args.as_slice()) {
        ("vagrant", ["--version"]) => Reply::ok(&format!("Vagrant {}", e.version.as_deref().unwrap_or("2.4.9"))),
        ("VBoxManage", ["--version"]) => Reply::ok("7.2.20r175154"),
        ("vagrant", ["plugin", "list", "--machine-readable"]) => {
            Reply::ok(&e.plugins.iter().map(|p| format!("1,,plugin-name,{p}")).collect::<Vec<_>>().join("\n"))
        }
        ("vagrant", ["status", rest @ ..]) => {
            let only = rest.first().filter(|r| !r.starts_with("--"));
            // The Vagrantfile's VMs, then any other VM the run defined.
            let extra = e.states.keys().filter(|vm| !VMS.contains(&vm.as_str())).map(String::as_str);
            let lines: Vec<String> = VMS
                .iter()
                .copied()
                .chain(extra)
                .filter(|vm| only.is_none_or(|o| o == vm))
                .map(|vm| format!("1,{vm},state,{}", e.states.get(vm).map_or("not_created", String::as_str)))
                .collect();
            Reply::ok(&lines.join("\n"))
        }
        ("vagrant", ["ssh", CONTROLLER, "-c", probe]) if probe.contains("isoloom-halting") => {
            if e.halting > 0 {
                e.halting -= 1;
                if e.halting == 0 {
                    e.states.insert(CONTROLLER.into(), "poweroff".into());
                }
                Reply::ok("halting")
            } else if e.going_down > 0 {
                e.going_down -= 1;
                if e.going_down == 0 {
                    e.states.insert(CONTROLLER.into(), "poweroff".into());
                }
                Reply::fail(255, "ssh: connect to host 127.0.0.1 port 2200: Connection refused").after(e.probe_delay)
            } else {
                Reply::ok("up")
            }
        }
        ("vagrant", ["ssh", ..]) => Reply::fail(1, ""),
        // ESXi's guests answer at their own address: what their VMware Tools report.
        ("vagrant", ["ssh-config", vm]) if !e.ssh_config_fails && e.states.get(*vm).is_some_and(|s| s == "running") => {
            Reply::ok(&format!("Host {vm}\n  HostName 172.16.1.5{}\n  User vagrant\n  Port 22\n", vm.len() % 10))
        }
        ("vagrant", ["ssh-config", ..]) => Reply::fail(
            1,
            "The provider for this Vagrant-managed machine is reporting that it is not yet ready for SSH.",
        ),
        ("vagrant", rest) => vagrant(&mut e, call, rest),
        ("VBoxManage", rest) => vboxmanage(&mut e, rest),
        ("prlctl", rest) => prlctl(&mut e, rest),
        _ => Reply::ok(""),
    }
}

/// Vagrant's refusal of `args` while another process holds a machine's lock, as long as the
/// engine says it still does.
fn locked_out(e: &mut Engine, args: &[&str]) -> Option<Reply> {
    let up_holds = e.up_holds_until.is_some_and(|until| tokio::time::Instant::now() < until);
    match args {
        // The run's own `vagrant up` provisions `web`: Vagrant refuses to read its state.
        ["status", ..] if up_holds => Some(Reply {
            stdout: locked_status("web"),
            code: 1,
            ..Reply::default()
        }),
        ["status", ..] if e.acting_on.is_some() => Some(Reply {
            stdout: locked_status(e.acting_on.unwrap_or_default()),
            code: 1,
            ..Reply::default()
        }),
        ["status", ..] if e.locked_reads > 0 => {
            e.locked_reads -= 1;
            Some(Reply {
                stdout: locked_status("web"),
                code: 1,
                ..Reply::default()
            })
        }
        ["status", ..] if e.index_locked_reads > 0 => {
            e.index_locked_reads -= 1;
            Some(Reply {
                stdout: index_locked_status(CONTROLLER),
                stderr: vec![FOG_WARNING.to_owned(); VMS.len()],
                code: 1,
                ..Reply::default()
            })
        }
        ["halt", CONTROLLER] if e.locked_halts > 0 => {
            e.locked_halts -= 1;
            Some(Reply::fail(1, &locked_halt(CONTROLLER)))
        }
        [verb, ..] if e.refusals.get(*verb).is_some_and(|r| r.times > 0) => {
            let refusal = e.refusals.get_mut(*verb)?;
            refusal.times -= 1;
            let (did, mut reply) = (refusal.did, Reply::fail(1, &refusal.said));
            reply.stdout = refusal.before.lines().map(str::to_owned).collect();
            reply.delay = refusal.took;
            did(e);
            Some(reply)
        }
        _ => None,
    }
}

/// One VM booted alone on the run's provider (`vagrant up <vm> --provider=<p>`) and destroyed
/// alone (`vagrant destroy -f <vm>`): a spec's access machine, or a retired one.
fn one_vm(e: &mut Engine, dotfile: &Path, args: &[&str]) -> Option<Reply> {
    match args {
        ["up", vm, provider] if provider.starts_with("--provider=") => {
            e.states.insert((*vm).to_owned(), "running".into());
            record(dotfile, vm, provider.trim_start_matches("--provider="));
            Some(Reply::ok(&format!("==> {vm}: Machine booted and ready!")))
        }
        ["destroy", "-f", vm] => {
            e.states.remove(*vm);
            std::fs::remove_dir_all(dotfile.join("machines").join(vm)).ok();
            Some(Reply::ok(&format!("==> {vm}: Destroying VM and associated drives...")))
        }
        _ => None,
    }
}

fn vagrant(e: &mut Engine, call: &Call, args: &[&str]) -> Reply {
    let dotfile = PathBuf::from(call.env("VAGRANT_DOTFILE_PATH").unwrap_or_default());
    if let Some(reply) = one_vm(e, &dotfile, args) {
        return reply;
    }
    match args {
        ["up", "--provider", provider] => {
            let (out, failed) = if e.ups.is_empty() {
                ("Machine booted and ready!".to_owned(), false)
            } else {
                e.ups.remove(0)
            };
            if failed {
                return Reply::fail(1, &out);
            }
            let added = e.added.clone();
            for vm in VMS.iter().copied().chain(added.iter().map(String::as_str)) {
                e.states.insert(vm.into(), "running".into());
                record(&dotfile, vm, provider);
            }
            e.provider = Some((*provider).to_owned());
            e.up_holds_until = Some(tokio::time::Instant::now() + e.up_takes);
            Reply::ok(&format!("{out}\nseeding {}", call.env("DEMO_SECRET").unwrap_or("no secret"))).after(e.up_takes)
        }
        ["up", "--no-provision", "--provider", _, vms @ ..] | ["up", vms @ .., "--no-provision"] => {
            for vm in vms {
                e.states.insert((*vm).to_owned(), "running".into());
            }
            Reply::ok("")
        }
        ["halt", vm] => halt(e, vm),
        // Every VM, or those named (Isoloom suspends the environment's own, not the controller).
        [verb @ ("halt" | "suspend"), vms @ ..] => {
            let label = if *verb == "halt" { "poweroff" } else { "saved" };
            let named = |vm: &String| vms.is_empty() || vms.contains(&vm.as_str());
            for (_, state) in e.states.iter_mut().filter(|(vm, s)| *s == "running" && named(vm)) {
                *state = label.into();
            }
            Reply::ok("")
        }
        ["destroy", "--force"] if e.fails.contains(&Fails::Destroy) => {
            Reply::fail(1, "The provider for this Vagrant-managed machine is reporting that it is not yet ready")
        }
        ["destroy", "--force"] => {
            e.states.clear();
            // As Vagrant does: the machines' records go with them (none to remove is fine).
            std::fs::remove_dir_all(dotfile.join("machines")).ok();
            Reply::ok("==> web: Destroying VM and associated drives...")
        }
        ["provision", CONTROLLER, "--provision-with", "checks"] => {
            Reply::ok("    isoloom-controller: isoloom-check: PASS http://web/\n    isoloom-controller: isoloom-check: END 1 passed, 0 failed")
        }
        _ => Reply::ok(""),
    }
}

fn prlctl(e: &mut Engine, args: &[&str]) -> Reply {
    match args {
        ["list", "-a", "--no-header", "-o", "name"] => match &e.parallels {
            Some(vms) => Reply::ok(&vms.iter().cloned().collect::<Vec<_>>().join("\n")),
            None => Reply::fail(1, "Failed to get the list of VMs: the Parallels service isn't running"),
        },
        ["delete", _] if e.fails.contains(&Fails::Delete) => Reply::fail(1, "Failed to delete the VM: the virtual machine is in use"),
        ["delete", name] => {
            e.parallels.as_mut().map(|vms| vms.remove(*name));
            Reply::ok("")
        }
        _ => Reply::ok(""),
    }
}

/// `vagrant halt <vm>`, one VM.
fn halt(e: &mut Engine, vm: &str) -> Reply {
    if vm == CONTROLLER {
        e.index_locked_reads = std::mem::take(&mut e.index_locks_after_halt);
    }
    if e.esxi_off_before_halt && vm == CONTROLLER {
        e.states.insert(CONTROLLER.into(), "powered_off".into());
        return Reply::fail(1, ESXI_ALREADY_OFF);
    }
    if let Some(state) = e.halt_fails.filter(|_| vm == CONTROLLER) {
        e.states.insert(CONTROLLER.into(), state.into());
        return Reply::fail(1, "SSH connection was refused! This usually happens if the VM failed to\nboot properly.");
    }
    e.states.insert(vm.to_owned(), "poweroff".into());
    Reply::ok(&format!("==> {vm}: Attempting graceful shutdown of VM...")).after(e.halt_delay)
}

fn vboxmanage(e: &mut Engine, args: &[&str]) -> Reply {
    match args {
        ["showvminfo", name, "--machinereadable"] => match e.vbox.get(*name) {
            _ if e.vbox_wedged => Reply::fail(1, VBOX_WEDGED),
            Some(state) => Reply::ok(&format!("name=\"{name}\"\nVMState=\"{state}\"")),
            None => Reply::fail(1, "Could not find a registered machine"),
        },
        ["discardstate", vm] => {
            if let Some(state) = e.vbox.get_mut(vm.trim_start_matches("uuid-")) {
                *state = "poweroff".into();
            }
            Reply::ok("")
        }
        ["list", "vms"] => Reply::ok(&e.vbox.keys().map(|n| format!("\"{n}\" {{uuid-{n}}}")).collect::<Vec<_>>().join("\n")),
        ["unregistervm", _, "--delete"] if e.fails.contains(&Fails::Delete) => Reply::fail(1, VBOX_LOCKED),
        ["list", "systemproperties"] => match &e.machine_folder {
            Some(folder) => Reply::ok(&format!("Default machine folder:          {}", folder.display())),
            None => Reply::ok(""),
        },
        ["unregistervm", uuid, "--delete"] => {
            let name = uuid.trim_start_matches("uuid-").to_owned();
            e.vbox.remove(&name);
            Reply::ok("")
        }
        _ => Reply::ok(""),
    }
}

/// A start once `set` made the engine so: it succeeds, with no step tolerated.
async fn starts_without_a_tolerated_step(set: impl FnOnce(&mut Engine)) -> Fixture {
    let f = fixture(crate::runner::this_host());
    set(&mut f.engine());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    let said = serde_json::to_string(&events.events()).unwrap();
    assert!(!said.contains("step_tolerated"), "{said}");
    f
}
