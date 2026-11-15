//! A Vagrant run one of whose VMs is destroyed outside it (`vagrant destroy -f web` in a
//! terminal, or the VM deleted in the hypervisor's app), beside a machine the Vagrantfile never
//! starts (`autostart: false`, the attack box): `vagrant status` says both `not_created`.

use std::collections::BTreeMap;
use std::path::Path;

use super::vagrant::{VERSION, a_vagrant_lab_on};
use super::*;
use crate::process::fake::FakeProcesses;

/// What VirtualBox holds of the lab: each machine made, whether it runs; and the `up`s seen.
#[derive(Debug, Default)]
struct Lab {
    made: BTreeMap<&'static str, bool>,
    ups: usize,
}

/// The dotfile folder `call` is given.
fn dotfile(call: &Call) -> std::path::PathBuf {
    Path::new(call.env("VAGRANT_DOTFILE_PATH").expect("the dotfile folder")).join("machines")
}

/// Vagrant over VirtualBox, for a Vagrantfile of `web` and `attackbox` (never started by an
/// `up`). A destroy forgets the machine's id, as Vagrant does.
fn answer(lab: &Mutex<Lab>, call: &Call) -> Reply {
    if call.program == "VBoxManage" {
        // The preamble's look at a saved state: none was cut off.
        return Reply::ok("VMState=\"running\"");
    }
    let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
    let mut lab = lab.lock().unwrap_or_else(PoisonError::into_inner);
    match args.as_slice() {
        ["--version"] => Reply::ok(VERSION),
        ["up", "--provider", "virtualbox", ..] => {
            lab.ups += 1;
            let dir = dotfile(call).join("web").join("virtualbox");
            std::fs::create_dir_all(&dir).expect("its folder");
            std::fs::write(dir.join("id"), format!("uuid-web-{}", lab.ups)).expect("its id");
            lab.made.insert("web", true);
            Reply::ok("")
        }
        ["status", "--machine-readable"] => {
            let lines: Vec<String> = ["attackbox", "web"]
                .iter()
                .map(|m| {
                    let state = lab.made.get(m).map_or("not_created", |runs| if *runs { "running" } else { "poweroff" });
                    format!("1794305819,{m},state,{state}")
                })
                .collect();
            Reply::ok(&lines.join("\n"))
        }
        ["port", m, "--machine-readable"] => Reply::ok(&format!("1794305819,{m},forwarded_port,22,2222")),
        ["destroy", "-f"] => {
            lab.made.clear();
            Reply::ok("")
        }
        _ => panic!("a call the model doesn't know: {}", call.line()),
    }
}

/// `web` destroyed outside the run, as `vagrant destroy -f web` leaves it: no VM, no id in the
/// dotfile folder `dot`.
fn destroy_web_outside(lab: &Mutex<Lab>, dot: &Path) {
    lab.lock().unwrap_or_else(PoisonError::into_inner).made.remove("web");
    let id = dot.join("machines/web/virtualbox/id");
    std::fs::remove_file(&id).expect("its id was recorded");
}

#[tokio::test]
async fn a_vm_destroyed_outside_reads_absent_beside_one_never_made_its_resume_is_refused_and_an_up_makes_it_again() {
    let lab = Arc::new(Mutex::new(Lab::default()));
    let fake = Arc::new(FakeProcesses::model({
        let lab = Arc::clone(&lab);
        move |call| answer(&lab, call)
    }));
    let (_root, project, runner) = a_vagrant_lab_on(&fake);
    let run = runner.open(&project, None).expect("opened");
    let opts = RunOptions::new().with_provider(crate::Provider::Virtualbox);
    // The run's state and each machine's, as a status reads them now.
    let read = async || {
        let s = run.status_with(&opts).await.expect("status");
        (s.state, s.machines.into_iter().map(|m| (m.name, m.state)).collect::<Vec<_>>())
    };
    let absent = |m: &str| (m.to_owned(), MachineState::Absent);
    let running = |m: &str| (m.to_owned(), MachineState::Running);

    run.up(&opts, &Recorder::new()).await.expect("up");
    assert_eq!(
        read().await,
        (RunState::Ready, vec![absent("attackbox"), running("web")]),
        "the attack box was never made: not gone"
    );

    let up = fake.made("vagrant up").pop().expect("an up");
    destroy_web_outside(&lab, Path::new(up.env("VAGRANT_DOTFILE_PATH").expect("the dotfile folder")));
    assert_eq!(
        read().await,
        (RunState::Failed, vec![absent("attackbox"), absent("web")]),
        "web was made, then destroyed: gone, and the run failed"
    );

    refused_as_gone(&run.resume(&opts, &Recorder::new()).await.expect_err("web is gone"));
    assert_eq!(lab.lock().unwrap_or_else(PoisonError::into_inner).ups, 1, "nothing was started");

    run.up(&opts, &Recorder::new()).await.expect("up makes it again");
    assert_eq!(read().await, (RunState::Ready, vec![absent("attackbox"), running("web")]));
}
