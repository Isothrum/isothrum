//! The vagrant driver through a fake Vagrant: every `vagrant` and `VBoxManage` call answered
//! from a small model of a Vagrantfile's VMs (the `flag-vm` fixture's `web` and controller),
//! which keeps Vagrant's record where the call's `VAGRANT_DOTFILE_PATH` says, as Vagrant does.
//! The launcher's `runtime/vm` tests, rewritten against the runner's fake processes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use isoloom_manifest::HostOs;

use crate::net::FakePorts;
use crate::process::fake::{Call, FakeProcesses, Reply};
use crate::secrets::{SecretValue, Secrets};
use crate::testing::{FLAG_VM, LabFolder, modelled, processes, runner_config, virtualbox_options};
use crate::{ErrorCode, Park, Recorder, RunOptions, RunState, Runner};

const CONTROLLER: &str = "isoloom-controller";
const FLAG: &str = "flag{vm-0001}";

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
    /// How many times the controller still says it halts.
    halting: u32,
    /// What `vagrant --version` says (default: 2.4.9).
    version: Option<String>,
    /// What `vagrant halt <controller>` does: `None` halts it; `Some(state)` fails as libvirt's
    /// does once SSH is closed, the controller left in `state`.
    halt_fails: Option<&'static str>,
    /// The controller powers itself off before `vagrant halt` (ESXi refuses that halt).
    esxi_off_before_halt: bool,
}

struct Fixture {
    _folder: LabFolder,
    manifest: PathBuf,
    fake: Arc<FakeProcesses>,
    engine: Arc<Mutex<Engine>>,
    runner: Runner,
}

fn fixture(host: HostOs) -> Fixture {
    fixture_from(host, FLAG_VM)
}

fn fixture_from(host: HostOs, manifest: &str) -> Fixture {
    let folder = LabFolder::new("vagrant", manifest);
    let (engine, fake) = modelled(answer);
    let ports: Arc<dyn crate::HostPorts> = Arc::new(FakePorts::new([41000]));
    let runner = Runner::new(runner_config(folder.home.path(), processes(&fake), ports, host)).unwrap();
    Fixture {
        manifest: folder.manifest.clone(),
        _folder: folder,
        fake,
        engine,
        runner,
    }
}

/// The fixture's manifest with its web port made ephemeral and the port-mode choice declared, as
/// Isoloom now generates a VM's published ports: the runner picks a free host port for each,
/// unless `ISOLOOM_PUBLISH_FIXED` asks for its own number.
fn ephemeral_manifest() -> String {
    let mut v: serde_json::Value = serde_json::from_str(FLAG_VM).expect("the fixture parses");
    for machine in v["machines"].as_array_mut().into_iter().flatten() {
        for port in machine.get_mut("ports").and_then(serde_json::Value::as_array_mut).into_iter().flatten() {
            if let Some(publish) = port.get_mut("publish").and_then(serde_json::Value::as_object_mut) {
                publish.insert("mode".to_owned(), serde_json::Value::from("ephemeral"));
            }
        }
    }
    let fixed = serde_json::json!({
        "name": "ISOLOOM_PUBLISH_FIXED",
        "description": "Set: each published port takes its `publish:` number instead of an ephemeral one.",
        "values": ["1"],
        "set_by": "embedder",
        "deliver": [{"via": "env", "var": "ISOLOOM_PUBLISH_FIXED", "steps": ["*"]}]
    });
    if let Some(params) = v["params"].as_array_mut() {
        params.push(fixed);
    }
    serde_json::to_string(&v).expect("the manifest serializes")
}

/// `ephemeral_manifest`, with each published port's `preferred` number dropped — an ephemeral port
/// Isoloom emits with no `publish:` number of its own, so its host port can only come from the
/// runner's pick.
fn ephemeral_no_preferred_manifest() -> String {
    let mut v: serde_json::Value = serde_json::from_str(&ephemeral_manifest()).expect("the fixture parses");
    for machine in v["machines"].as_array_mut().into_iter().flatten() {
        for port in machine.get_mut("ports").and_then(serde_json::Value::as_array_mut).into_iter().flatten() {
            if let Some(publish) = port.get_mut("publish").and_then(serde_json::Value::as_object_mut) {
                publish.remove("preferred");
            }
        }
    }
    serde_json::to_string(&v).expect("the manifest serializes")
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

/// The options the tests start with: VirtualBox, the flag given.
fn opts() -> RunOptions {
    virtualbox_options(FLAG)
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
    match (call.program.as_str(), args.as_slice()) {
        ("vagrant", ["--version"]) => Reply::ok(&format!("Vagrant {}", e.version.as_deref().unwrap_or("2.4.9"))),
        ("VBoxManage", ["--version"]) => Reply::ok("7.2.20r175154"),
        ("vagrant", ["plugin", "list", "--machine-readable"]) => {
            Reply::ok(&e.plugins.iter().map(|p| format!("1,,plugin-name,{p}")).collect::<Vec<_>>().join("\n"))
        }
        ("vagrant", ["status", rest @ ..]) => {
            let only = rest.first().filter(|r| !r.starts_with("--"));
            let lines: Vec<String> = VMS
                .iter()
                .filter(|vm| only.is_none_or(|o| o == *vm))
                .map(|vm| format!("1,{vm},state,{}", e.states.get(*vm).map_or("not_created", String::as_str)))
                .collect();
            Reply::ok(&lines.join("\n"))
        }
        ("vagrant", ["ssh", CONTROLLER, "-c", _]) if e.halting > 0 => {
            e.halting -= 1;
            if e.halting == 0 {
                e.states.insert(CONTROLLER.into(), "poweroff".into());
            }
            Reply::ok("")
        }
        ("vagrant", ["ssh", ..]) => Reply::fail(1, ""),
        // ESXi's guests answer at their own address: what their VMware Tools report.
        ("vagrant", ["ssh-config", vm]) if e.states.get(*vm).is_some_and(|s| s == "running") => {
            Reply::ok(&format!("Host {vm}\n  HostName 172.16.1.5{}\n  User vagrant\n  Port 22\n", vm.len() % 10))
        }
        ("vagrant", ["ssh-config", ..]) => Reply::fail(
            1,
            "The provider for this Vagrant-managed machine is reporting that it is not yet ready for SSH.",
        ),
        ("vagrant", rest) => vagrant(&mut e, call, rest),
        ("VBoxManage", rest) => vboxmanage(&mut e, rest),
        _ => Reply::ok(""),
    }
}

fn vagrant(e: &mut Engine, call: &Call, args: &[&str]) -> Reply {
    let dotfile = PathBuf::from(call.env("VAGRANT_DOTFILE_PATH").unwrap_or_default());
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
            for vm in VMS {
                e.states.insert(vm.into(), "running".into());
                record(&dotfile, vm, provider);
            }
            e.provider = Some((*provider).to_owned());
            Reply::ok(&format!("{out}\nseeding {}", call.env("CTF_FLAG_MAIN").unwrap_or("no flag")))
        }
        ["up", "--no-provision", "--provider", _, vms @ ..] | ["up", vms @ .., "--no-provision"] => {
            for vm in vms {
                e.states.insert((*vm).to_owned(), "running".into());
            }
            Reply::ok("")
        }
        ["halt", vm] => {
            if e.esxi_off_before_halt && *vm == CONTROLLER {
                e.states.insert(CONTROLLER.into(), "powered_off".into());
                return Reply::fail(1, ESXI_ALREADY_OFF);
            }
            if let Some(state) = e.halt_fails.filter(|_| *vm == CONTROLLER) {
                e.states.insert(CONTROLLER.into(), state.into());
                return Reply::fail(1, "SSH connection was refused! This usually happens if the VM failed to\nboot properly.");
            }
            e.states.insert((*vm).to_owned(), "poweroff".into());
            Reply::ok(&format!("==> {vm}: Attempting graceful shutdown of VM..."))
        }
        [verb @ ("halt" | "suspend")] => {
            let label = if *verb == "halt" { "poweroff" } else { "saved" };
            for state in e.states.values_mut().filter(|s| *s == "running") {
                *state = label.into();
            }
            Reply::ok("")
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

fn vboxmanage(e: &mut Engine, args: &[&str]) -> Reply {
    match args {
        ["showvminfo", name, "--machinereadable"] => match e.vbox.get(*name) {
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
        ["unregistervm", uuid, "--delete"] => {
            let name = uuid.trim_start_matches("uuid-").to_owned();
            e.vbox.remove(&name);
            Reply::ok("")
        }
        _ => Reply::ok(""),
    }
}

#[tokio::test]
async fn a_lab_comes_up_is_checked_parked_resumed_and_goes() {
    let f = fixture(crate::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let started = run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(started.record.state, RunState::Ready);
    assert_eq!(started.record.provider.as_deref(), Some("virtualbox"));
    assert_eq!(f.state(CONTROLLER), "poweroff", "the controller is halted once the run is built");
    assert_eq!(started.outputs[0].value.as_deref(), Some("http://127.0.0.1:8086/"));
    assert_eq!(started.message.as_deref(), Some("The page answers at http://localhost:8086/."));

    let report = run.check(&opts(), &Recorder::new()).await.unwrap();
    assert!(report.ok, "{report:?}");
    assert_eq!(f.state(CONTROLLER), "poweroff", "booted for the checks, halted after");
    assert!(
        f.vagrant().contains(&"vagrant up isoloom-controller --no-provision".to_owned()),
        "{:?}",
        f.vagrant()
    );

    run.park(Park::Pause, &Recorder::new()).await.unwrap();
    assert_eq!(f.state("web"), "saved");
    assert_eq!(run.status().await.unwrap().state, RunState::Parked);
    run.resume(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert_eq!(
        (f.state("web").as_str(), f.state(CONTROLLER).as_str()),
        ("running", "poweroff"),
        "the controller stays off"
    );
    run.park(Park::Shutdown, &Recorder::new()).await.unwrap();
    assert_eq!(f.state("web"), "poweroff");
    run.resume(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert_eq!(f.state("web"), "running");

    run.down(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert_eq!(f.state("web"), "not_created");
    assert_eq!(run.status().await.unwrap().state, RunState::Absent);
}

#[tokio::test]
async fn vagrants_record_lives_in_the_runs_state_folder() {
    let f = fixture(crate::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let dotfile = f.runner.store.run_dir(run.name()).unwrap().join("vagrant");
    for call in f.fake.calls().iter().filter(|c| c.program == "vagrant") {
        assert_eq!(call.env("VAGRANT_DOTFILE_PATH").map(PathBuf::from), Some(dotfile.clone()), "{}", call.line());
    }
    assert!(dotfile.join("machines/web/virtualbox/id").is_file());
    assert!(!f.manifest.parent().unwrap().join(".vagrant").exists(), "nothing next to the Vagrantfile");
    run.down(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert!(!dotfile.exists(), "reset once the VMs are gone");
}

#[tokio::test]
async fn the_flag_reaches_the_start_and_never_an_event_and_an_unset_one_is_empty() {
    let f = fixture(crate::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    let up = f.fake.last("vagrant up --provider").unwrap();
    assert_eq!(up.env("CTF_FLAG_MAIN"), Some(FLAG));
    assert_eq!(
        f.fake.last("vagrant halt").unwrap().env("CTF_FLAG_MAIN"),
        None,
        "only the steps that run provisioners"
    );
    let text = serde_json::to_string(&events.events()).unwrap();
    assert!(!text.contains(FLAG) && text.contains("seeding ***"), "{text}");
    // ESXi's connection never reaches a VirtualBox run.
    assert!(
        f.fake
            .calls()
            .iter()
            .all(|c| c.env("ESXI_PASSWORD").is_none() && c.env("ESXI_HOSTNAME").is_none())
    );
    run.down(&RunOptions::new(), &Recorder::new()).await.unwrap();
    run.up(&RunOptions::new().with_provider("virtualbox"), &Recorder::new()).await.unwrap();
    assert_eq!(f.fake.last("vagrant up --provider").unwrap().env("CTF_FLAG_MAIN"), Some(""), "its default");
}

#[tokio::test]
async fn a_provider_that_cant_run_the_lab_is_refused_before_anything_runs() {
    let f = fixture(HostOs::Macos);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let code = |r: crate::Result<crate::Started>| r.map(drop).unwrap_err();
    let none = code(run.up(&RunOptions::new(), &Recorder::new()).await);
    assert_eq!(none.code(), ErrorCode::ProviderRefused);
    assert!(none.to_string().contains("needs a provider"), "{none}");
    // UTM gives a VM no private network: Isoloom's refusal, in its words.
    let utm = code(run.up(&RunOptions::new().with_provider("utm"), &Recorder::new()).await);
    assert!(utm.to_string().contains("vagrant_utm gives a VM no private network"), "{utm}");
    let libvirt = code(run.up(&RunOptions::new().with_provider("libvirt"), &Recorder::new()).await);
    assert!(libvirt.to_string().contains("doesn't exist on this host"), "{libvirt}");
    assert!(f.vagrant().is_empty(), "{:?}", f.vagrant());
}

#[tokio::test]
async fn a_vagrant_older_than_2_4_is_refused_before_anything_runs() {
    // Debian's packaged 2.3.8 ignores a box's architecture: it booted a ppc64le Debian on amd64.
    let f = fixture(crate::this_host());
    f.engine().version = Some("2.3.8.dev".into());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::ToolTooOld);
    assert_eq!(err.to_string(), "`vagrant` is version 2.3.8; this run needs 2.4.0 or later");
    assert!(f.vagrant().is_empty(), "{:?}", f.vagrant());
}

#[tokio::test]
async fn a_providers_missing_plugin_is_named_with_its_install_command() {
    let f = fixture(HostOs::Linux);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&RunOptions::new().with_provider("libvirt"), &Recorder::new()).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "provider `libvirt`: needs the Vagrant plugin `vagrant-libvirt`: `vagrant plugin install vagrant-libvirt`"
    );
    f.engine().plugins.push("vagrant-libvirt".into());
    run.up(&RunOptions::new().with_provider("libvirt"), &Recorder::new()).await.unwrap();
    assert_eq!(f.engine().provider.as_deref(), Some("libvirt"));
}

#[tokio::test]
async fn a_start_that_tripped_over_leftovers_clears_them_and_starts_again() {
    // R8: VirtualBox prints VERR_ALREADY_EXISTS in capitals; the launcher's markers are lower case.
    let f = fixture(crate::this_host());
    f.engine()
        .ups
        .push(("VBoxManage: error: Details: code VERR_ALREADY_EXISTS (0x80004005)".into(), true));
    f.engine().vbox.insert("flag-vm · web".into(), "poweroff".into());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    let calls = f.fake.lines();
    let at = |text: &str| calls.iter().position(|l| l.contains(text)).unwrap_or_else(|| panic!("{text}: {calls:#?}"));
    assert!(at("vagrant destroy --force") < at("VBoxManage unregistervm uuid-flag-vm · web --delete"));
    assert_eq!(calls.iter().filter(|l| l.starts_with("vagrant up --provider")).count(), 2);
    assert!(f.engine().vbox.is_empty(), "the orphan is gone");
}

#[tokio::test]
async fn a_genuine_failure_is_never_retried_and_leads_with_ansibles_verdict() {
    let f = fixture(crate::this_host());
    let play = "    isoloom-controller: fatal: [web]: UNREACHABLE! => {\"msg\": \"timed out\"}\nThe SSH command responded with a non-zero exit status.";
    f.engine().ups.push((play.into(), true));
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.to_string(), "step `up` failed: fatal: [web]: UNREACHABLE! => {\"msg\": \"timed out\"}");
    assert_eq!(f.fake.lines().iter().filter(|l| l.starts_with("vagrant up --provider")).count(), 1);
}

#[tokio::test]
async fn a_vagrant_crash_leads_with_its_error_not_its_backtrace() {
    // Seen on libvirt: Vagrant prints the error, then dozens of Ruby frames.
    let f = fixture(crate::this_host());
    let error = "/gems/fog-core/wait_for.rb:9:in `block in wait_for': The specified wait_for timeout (2 seconds) was exceeded (Fog::Errors::TimeoutError)";
    let frames = (0..30).map(|n| format!("\tfrom /gems/vagrant/action/builder.rb:{n}:in `call'"));
    let crash = std::iter::once(error.to_owned()).chain(frames).collect::<Vec<_>>().join("\n");
    f.engine().ups.push((crash, true));
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err().to_string();
    assert!(
        err.contains("wait_for timeout (2 seconds) was exceeded") && !err.contains("builder.rb"),
        "{err}"
    );
}

#[tokio::test]
async fn a_record_on_another_provider_is_cleared_before_the_start() {
    let f = fixture(crate::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let dotfile = f.runner.store.run_dir(run.name()).unwrap().join("vagrant");
    record(&dotfile, "web", "vmware_desktop");
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    let calls = f.vagrant();
    assert_eq!(calls.first().map(String::as_str), Some("vagrant destroy --force"), "{calls:?}");
    assert!(!dotfile.join("machines/web/vmware_desktop").exists());
    // The clean-up is shown as it goes; a read of the driver's own (the plugins) is not.
    let logged: Vec<String> = events
        .events()
        .into_iter()
        .filter_map(|e| match e.kind {
            crate::EventKind::Log { line, .. } => Some(line),
            _ => None,
        })
        .collect();
    assert!(logged.iter().any(|l| l.contains("Destroying VM")), "{logged:?}");
    assert!(!logged.iter().any(|l| l.contains("plugin-name")), "{logged:?}");
    // On its own provider, nothing is cleared.
    let again = fixture(crate::this_host());
    let same = again.runner.open(&again.manifest, None).unwrap();
    record(&again.runner.store.run_dir(same.name()).unwrap().join("vagrant"), "web", "virtualbox");
    same.up(&opts().with_occupied(crate::Occupied::Replace), &Recorder::new()).await.unwrap();
    assert!(!again.vagrant().iter().take(1).any(|l| l.contains("destroy")), "{:?}", again.vagrant());
}

#[tokio::test]
async fn a_virtualbox_vm_whose_saved_state_was_cut_off_boots_fresh() {
    let f = fixture(crate::this_host());
    f.engine().vbox.insert("flag-vm · web".into(), "aborted-saved".into());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let calls = f.fake.lines();
    let discard = calls.iter().position(|l| l == "VBoxManage discardstate flag-vm · web").expect("discarded");
    assert!(discard < calls.iter().position(|l| l.starts_with("vagrant up --provider")).unwrap());
}

#[tokio::test]
async fn a_controller_powering_itself_off_after_up_is_waited_for_not_warned_about() {
    // Seen on libvirt: the halt after `up` races the controller's own poweroff and fails over SSH.
    let f = fixture(crate::this_host());
    f.engine().halt_fails = Some("shutoff");
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    let said = serde_json::to_string(&events.events()).unwrap();
    assert!(!said.contains("step_tolerated"), "{said}");
    assert_eq!(f.state(CONTROLLER), "shutoff");
}

/// What vagrant-vmware-esxi prints when it is asked to halt a VM that is already off.
const ESXI_ALREADY_OFF: &str = "There was an error talking to ESXi.
  Unable to shutdown the VM:
    (vim.fault.InvalidPowerState) {
   faultCause = (vmodl.MethodFault) null,
   faultMessage = <unset>,
   requestedState = \"poweredOn\",
   existingState = \"poweredOff\"
   msg = \"Received SOAP response fault from [<TCP '127.0.0.1 : 8307'>, /sdk>]: shutdownGuest
The attempted operation cannot be performed in the current state (Powered off).\"
}";

#[tokio::test]
async fn on_esxi_a_controller_already_off_is_halted_without_an_error_in_the_log() {
    // Seen on esxi-run: the controller powers itself off at the end of its play, and ESXi
    // refuses the guest shutdown `vagrant halt` asks for.
    let f = fixture(HostOs::Linux);
    {
        let mut e = f.engine();
        e.plugins.push("vagrant-vmware-esxi".into());
        e.esxi_off_before_halt = true;
    }
    let mut connection = Secrets::new();
    connection.insert("ESXI_PASSWORD", SecretValue::new("pw"));
    let run = f.runner.open(&f.manifest, None).unwrap().with_connection(connection);
    let events = Recorder::new();
    run.up(&opts().with_provider("vmware_esxi").with_param("ESXI_HOSTNAME", "172.16.1.20"), &events)
        .await
        .unwrap();
    let said = serde_json::to_string(&events.events()).unwrap();
    assert!(!said.contains("InvalidPowerState") && !said.contains("step_tolerated"), "{said}");
    assert!(said.contains("isoloom-controller was already off"), "{said}");
    assert!(
        !f.fake.lines().iter().any(|l| l.starts_with("vagrant status isoloom-controller")),
        "no wait for a controller ESXi says is off: {:?}",
        f.fake.lines()
    );
}

#[tokio::test]
async fn a_controller_halts_with_its_lines_in_the_log() {
    let f = fixture(crate::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    assert_eq!(f.state(CONTROLLER), "poweroff");
    let halted = events
        .events()
        .iter()
        .any(|e| matches!(&e.kind, crate::EventKind::Log { line, .. } if line == "==> isoloom-controller: Attempting graceful shutdown of VM..."));
    assert!(halted, "{:?}", events.events());
}

#[tokio::test(start_paused = true)]
async fn a_controller_no_halt_stops_is_a_warning_after_up() {
    let f = fixture(crate::this_host());
    f.engine().halt_fails = Some("running");
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    let said = serde_json::to_string(&events.events()).unwrap();
    assert!(
        said.contains("step_tolerated") && said.contains("`vagrant halt isoloom-controller` failed: SSH connection was refused!"),
        "{said}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_controller_still_halting_is_waited_for_before_it_boots() {
    let f = fixture(crate::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    {
        let mut e = f.engine();
        e.states.insert(CONTROLLER.into(), "running".into());
        e.halting = 3;
    }
    let report = run.check(&opts(), &Recorder::new()).await.unwrap();
    assert!(report.ok);
    let calls = f.fake.lines();
    let probes = calls.iter().filter(|l| l.contains("test -e /run/isoloom-halting")).count();
    assert_eq!(probes, 3, "{calls:#?}");
    let boot = calls.iter().rposition(|l| l == "vagrant up isoloom-controller --no-provision").unwrap();
    let last_probe = calls.iter().rposition(|l| l.contains("isoloom-halting")).unwrap();
    assert!(last_probe < boot);
}

#[tokio::test]
async fn provisioning_again_runs_on_the_running_machines_with_the_values_given_again() {
    let f = fixture(crate::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let not_up = run.provision(None, &opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(not_up.code(), ErrorCode::NotRunning);
    run.up(&opts(), &Recorder::new()).await.unwrap();
    run.provision(Some("web"), &opts(), &Recorder::new()).await.unwrap();
    let one = f.fake.last("vagrant provision web").unwrap();
    assert_eq!(one.env("CTF_FLAG_MAIN"), Some(FLAG), "the value given again");
    run.provision(None, &RunOptions::new(), &Recorder::new()).await.unwrap();
    assert_eq!(
        f.fake.last("vagrant provision").unwrap().env("CTF_FLAG_MAIN"),
        Some(""),
        "none given: its default"
    );
    let unknown = run.provision(Some("isoloom-controller"), &opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(unknown.code(), ErrorCode::MachineUnknown, "only the run's own machines");
}

/// ESXi forwards no port to this computer: a published port is shown at its VM's own address
/// (what `vagrant ssh-config` reports), with the VM's own port; locally, at the loopback with the
/// forwarded one.
#[tokio::test]
async fn on_esxi_a_published_port_is_shown_at_its_vms_address() {
    let f = fixture(HostOs::Linux);
    f.engine().plugins.push("vagrant-vmware-esxi".into());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let mut secrets = Secrets::new();
    secrets.insert("CTF_FLAG_MAIN", SecretValue::new(FLAG));
    secrets.insert("ESXI_PASSWORD", SecretValue::new("pw"));
    let opts = RunOptions::new()
        .with_provider("vmware_esxi")
        .with_param("ESXI_HOSTNAME", "172.16.1.20")
        .with_secrets(secrets);
    let started = run.up(&opts, &Recorder::new()).await.unwrap();
    assert_eq!(started.outputs[0].value.as_deref(), Some("http://172.16.1.53:80/"));
    let status = run.status().await.unwrap();
    assert_eq!(status.outputs[0].value.as_deref(), Some("http://172.16.1.53:80/"));
    let web = status.machines.iter().find(|m| m.name == "web").unwrap();
    assert_eq!(web.published.iter().map(|p| (p.port, p.host_port)).collect::<Vec<_>>(), [(80, 80)]);
    assert!(f.fake.lines().contains(&"vagrant ssh-config web".to_owned()), "{:?}", f.fake.lines());
    assert!(
        !f.fake.lines().iter().any(|l| l == "vagrant ssh-config isoloom-controller"),
        "the controller publishes nothing"
    );
}

/// A fresh local start pins a free host port for an ephemeral published port, forwards the VM to
/// it (the Vagrantfile reads `ISOLOOM_HOSTPORTS`), and the lab answers on it — not on its own 80.
#[tokio::test]
async fn a_fresh_local_start_pins_a_free_host_port_and_answers_on_it() {
    let f = fixture_from(HostOs::Linux, &ephemeral_manifest());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let started = run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(
        f.fake.last("vagrant up").unwrap().env("ISOLOOM_HOSTPORTS"),
        Some(r#"{"web/web":41000}"#),
        "the pinned port reaches the Vagrantfile"
    );
    assert_eq!(started.outputs[0].value.as_deref(), Some("http://127.0.0.1:41000/"));
    let status = run.status().await.unwrap();
    let web = status.machines.iter().find(|m| m.name == "web").unwrap();
    assert_eq!(web.published.iter().map(|p| (p.port, p.host_port)).collect::<Vec<_>>(), [(80, 41000)]);
}

/// R2: a published port with no `preferred` number (an ephemeral port Isoloom emits without a
/// `publish:` number) was dropped from `status` entirely, so `${published…}`/`${host…}` could not
/// resolve. Its host port is the runner's pick, so it is reported like any other published port.
#[tokio::test]
async fn a_published_port_with_no_preferred_number_is_still_reported() {
    let f = fixture_from(HostOs::Linux, &ephemeral_no_preferred_manifest());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let status = run.status().await.unwrap();
    let web = status.machines.iter().find(|m| m.name == "web").unwrap();
    assert_eq!(web.published.iter().map(|p| (p.port, p.host_port)).collect::<Vec<_>>(), [(80, 41000)]);
}

/// A resume forwards the same pinned port (kept in the record), so the VM's URL doesn't change —
/// no port is picked again (the fake has none left to hand out).
#[tokio::test]
async fn a_resume_forwards_the_same_pinned_port() {
    let f = fixture_from(HostOs::Linux, &ephemeral_manifest());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    run.park(Park::Shutdown, &Recorder::new()).await.unwrap();
    run.resume(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert_eq!(f.fake.last("vagrant up").unwrap().env("ISOLOOM_HOSTPORTS"), Some(r#"{"web/web":41000}"#));
    let status = run.status().await.unwrap();
    let web = status.machines.iter().find(|m| m.name == "web").unwrap();
    assert_eq!(web.published.iter().find(|p| p.port == 80).unwrap().host_port, 41000);
}

/// With `ISOLOOM_PUBLISH_FIXED`, a VM forwards the lab's own port number instead of a free one.
#[tokio::test]
async fn in_fixed_mode_a_vm_forwards_the_labs_own_port() {
    let f = fixture_from(HostOs::Linux, &ephemeral_manifest());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let started = run.up(&opts().with_param("ISOLOOM_PUBLISH_FIXED", "1"), &Recorder::new()).await.unwrap();
    assert_eq!(f.fake.last("vagrant up").unwrap().env("ISOLOOM_HOSTPORTS"), Some(r#"{"web/web":8086}"#));
    assert_eq!(started.outputs[0].value.as_deref(), Some("http://127.0.0.1:8086/"));
}

/// A VM with only fixed ports (Isoloom before this change, or a lab that pins a port) picks
/// nothing and sets no `ISOLOOM_HOSTPORTS`; it forwards each port at its own number.
#[tokio::test]
async fn a_vm_with_only_fixed_ports_sets_no_host_ports_env() {
    let f = fixture(HostOs::Linux);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let started = run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(f.fake.last("vagrant up").unwrap().env("ISOLOOM_HOSTPORTS"), None);
    assert_eq!(started.outputs[0].value.as_deref(), Some("http://127.0.0.1:8086/"));
}

/// An ESXi host's password is a connection slot every Vagrant command needs, reads included: a
/// run given it once ([`crate::Running::with_connection`]) delivers it to every operation, status and
/// park too, which take no options; a value given to an operation wins.
#[tokio::test]
async fn a_connection_secret_given_to_the_lab_reaches_every_operation() {
    let f = fixture(HostOs::Linux);
    f.engine().plugins.push("vagrant-vmware-esxi".into());
    let mut connection = Secrets::new();
    connection.insert("ESXI_PASSWORD", SecretValue::new("pw"));
    let run = f.runner.open(&f.manifest, None).unwrap().with_connection(connection);
    let mut flag = Secrets::new();
    flag.insert("CTF_FLAG_MAIN", SecretValue::new(FLAG));
    let opts = RunOptions::new()
        .with_provider("vmware_esxi")
        .with_param("ESXI_HOSTNAME", "172.16.1.20")
        .with_secrets(flag);
    run.up(&opts, &Recorder::new()).await.unwrap();
    let after_up = f.fake.calls().len();
    run.status().await.unwrap();
    run.park(Park::Shutdown, &Recorder::new()).await.unwrap();
    run.resume(&RunOptions::new(), &Recorder::new()).await.unwrap();
    let vagrant: Vec<Call> = f
        .fake
        .calls()
        .into_iter()
        .filter(|c| c.program == "vagrant" && c.args != ["--version"])
        .collect();
    let later: Vec<String> = f.fake.calls()[after_up..].iter().map(Call::line).collect();
    assert!(later.iter().any(|l| l.starts_with("vagrant status")), "{later:?}");
    assert!(later.iter().any(|l| l == "vagrant halt"), "{later:?}");
    for call in &vagrant {
        assert_eq!(call.env("ESXI_PASSWORD"), Some("pw"), "{}", call.line());
    }
    // Given to an operation, its own value is the one delivered.
    let mut other = Secrets::new();
    other.insert("ESXI_PASSWORD", SecretValue::new("other"));
    run.down(&RunOptions::new().with_secrets(other), &Recorder::new()).await.unwrap();
    let destroy = f.fake.last("vagrant destroy").unwrap();
    assert_eq!(destroy.env("ESXI_PASSWORD"), Some("other"));
}
