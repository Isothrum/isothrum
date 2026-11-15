//! A Vagrant run whose controller powers itself off at the end of its own set-up.

use std::collections::BTreeMap;
use std::path::Path;

use super::*;
use crate::process::fake::FakeProcesses;

/// `vagrant --version`, as Vagrant 2.4.1 prints it.
const VERSION: &str = "Vagrant 2.4.1";

/// What VirtualBox holds of the lab: each machine's state label, by name.
#[derive(Debug, Default)]
struct Lab {
    machines: BTreeMap<String, &'static str>,
}

/// The lab's `status --machine-readable`, as Vagrant prints it.
fn status_of(lab: &Lab) -> String {
    ["controller", "web"]
        .iter()
        .flat_map(|m| {
            let state = lab.machines.get(*m).copied().unwrap_or("not_created");
            [
                format!("1794305819,{m},metadata,provider,virtualbox"),
                format!("1794305819,{m},provider-name,virtualbox"),
                format!("1794305819,{m},state,{state}"),
            ]
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Records machine `m` as Vagrant does, in the dotfile folder `call` is given.
fn record(call: &Call, m: &str) {
    let dot = call.env("VAGRANT_DOTFILE_PATH").expect("the dotfile folder");
    let dir = Path::new(dot).join("machines").join(m).join("virtualbox");
    std::fs::create_dir_all(&dir).expect("its folder");
    std::fs::write(dir.join("id"), format!("uuid-{m}")).expect("its id");
}

/// Vagrant over VirtualBox. An `up` boots both machines, and the controller's last provisioner
/// powers it off; a `halt` powers every machine off.
fn answer(lab: &Mutex<Lab>, call: &Call) -> Reply {
    let mut lab = lab.lock().unwrap_or_else(PoisonError::into_inner);
    let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
    match (call.program.as_str(), args.as_slice()) {
        ("vagrant", ["--version"]) => Reply::ok(VERSION),
        ("vagrant", ["up", "--provider", "virtualbox", ..]) => {
            for m in ["controller", "web"] {
                record(call, m);
            }
            lab.machines.insert("web".into(), "running");
            lab.machines.insert("controller".into(), "poweroff");
            Reply::ok("==> controller: Running provisioner: halt (shell)...")
        }
        ("vagrant", ["status", "--machine-readable"]) => Reply::ok(&status_of(&lab)),
        ("vagrant", ["port", m, "--machine-readable"]) => Reply::ok(&format!("1794305819,{m},forwarded_port,22,2222")),
        ("vagrant", ["halt"]) => {
            for state in lab.machines.values_mut() {
                *state = "poweroff";
            }
            Reply::ok("")
        }
        ("VBoxManage", ["showvminfo", _, "--machinereadable"]) => Reply::ok("VMState=\"running\""),
        _ => panic!("a call the model doesn't know: {}", call.line()),
    }
}

#[tokio::test]
async fn a_controller_that_powered_itself_off_leaves_the_run_ready_to_update_and_park() {
    let root = tempfile::tempdir().expect("a folder");
    let project = root.path().join("lab");
    std::fs::create_dir(&project).expect("the project");
    std::fs::write(project.join("Vagrantfile"), "Vagrant.configure('2') do |c|\nend\n").expect("a Vagrantfile");
    let lab = Arc::new(Mutex::new(Lab::default()));
    let fake = Arc::new(FakeProcesses::model({
        let lab = Arc::clone(&lab);
        move |call| answer(&lab, call)
    }));
    let config = runner_config(&root.path().join("home"), processes(&fake), no_ports(), crate::runner::this_host());
    let runner = Runner::new(config).expect("a runner");
    let run = runner.open(&project, None).expect("opened");
    let opts = RunOptions::new().with_provider(crate::Provider::Virtualbox);

    run.up(&opts, &Recorder::new()).await.expect("up");
    let status = run.status_with(&opts).await.expect("status");
    assert_eq!(status.state, RunState::Ready, "{:?}", status.machines);
    let machines: Vec<(&str, &MachineState)> = status.machines.iter().map(|m| (m.name.as_str(), &m.state)).collect();
    assert_eq!(machines, [("controller", &MachineState::Stopped), ("web", &MachineState::Running)]);

    run.update(&opts, &Recorder::new()).await.expect("an update is accepted");
    run.park(crate::Park::Shutdown, &opts, &Recorder::new()).await.expect("a park is accepted");
    assert_eq!(run.status_with(&opts).await.expect("status").state, RunState::Parked);
}
