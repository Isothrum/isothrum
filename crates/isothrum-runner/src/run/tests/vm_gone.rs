//! A Vagrant run one of whose VMs is destroyed outside it (`vagrant destroy -f web` in a
//! terminal, or the VM deleted in the hypervisor's app), beside a machine the Vagrantfile never
//! starts (`autostart: false`, the toolbox): `vagrant status` says both `not_created`.

use std::collections::BTreeMap;
use std::path::Path;

use super::vagrant::{a_modelled_vagrant_stack, vagrant_answer};
use super::*;
use crate::process::fake::FakeProcesses;

/// What VirtualBox holds of the stack: each machine made, whether it runs; and the `up`s seen.
#[derive(Debug, Default)]
struct Stack {
    made: BTreeMap<&'static str, bool>,
    ups: usize,
}

/// The dotfile folder `call` is given.
fn dotfile(call: &Call) -> std::path::PathBuf {
    Path::new(call.env("VAGRANT_DOTFILE_PATH").expect("the dotfile folder")).join("machines")
}

/// Vagrant over VirtualBox, for a Vagrantfile of `web` and `toolbox` (never started by an
/// `up`). A destroy forgets the machine's id, as Vagrant does.
fn answer(stack: &Mutex<Stack>, call: &Call) -> Reply {
    vagrant_answer(stack, call, |stack, args| match args {
        ["up", "--provider", "virtualbox", ..] => {
            stack.ups += 1;
            // An `up` of a machine there keeps its VM; one not there is made, a new VM.
            if !stack.made.contains_key("web") {
                let dir = dotfile(call).join("web").join("virtualbox");
                std::fs::create_dir_all(&dir).expect("its folder");
                std::fs::write(dir.join("id"), format!("uuid-web-{}", stack.ups)).expect("its id");
            }
            stack.made.insert("web", true);
            Reply::ok("")
        }
        ["status", "--machine-readable"] => {
            let lines: Vec<String> = ["toolbox", "web"]
                .iter()
                .map(|m| {
                    let state = stack.made.get(m).map_or("not_created", |runs| if *runs { "running" } else { "poweroff" });
                    format!("1794305819,{m},state,{state}")
                })
                .collect();
            Reply::ok(&lines.join("\n"))
        }
        ["port", m, "--machine-readable"] => Reply::ok(&format!("1794305819,{m},forwarded_port,22,2222")),
        ["destroy", "-f"] => {
            stack.made.clear();
            Reply::ok("")
        }
        // What the runner reads of the boxes around an `up`: none held.
        ["box", "list", ..] => Reply::ok(""),
        _ => panic!("a call the model doesn't know: {}", call.line()),
    })
}

/// `web` destroyed outside the run, as `vagrant destroy -f web` leaves it: no VM, no id in the
/// dotfile folder `dot`.
fn destroy_web_outside(stack: &Mutex<Stack>, dot: &Path) {
    stack.lock().unwrap_or_else(PoisonError::into_inner).made.remove("web");
    let id = dot.join("machines/web/virtualbox/id");
    std::fs::remove_file(&id).expect("its id was recorded");
}

/// The modelled VirtualBox, the fake answering from it, and a Vagrant project with a runner on it.
fn virtualbox_stack() -> (Arc<Mutex<Stack>>, Arc<FakeProcesses>, tempfile::TempDir, std::path::PathBuf, Runner) {
    a_modelled_vagrant_stack(answer)
}

#[tokio::test]
async fn a_vm_destroyed_outside_reads_absent_beside_one_never_made_its_resume_is_refused_and_an_up_makes_it_again() {
    let (stack, fake, _root, project, runner) = virtualbox_stack();
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
        (RunState::Ready, vec![absent("toolbox"), running("web")]),
        "the toolbox was never made: not gone"
    );

    let up = fake.made("vagrant up").pop().expect("an up");
    destroy_web_outside(&stack, Path::new(up.env("VAGRANT_DOTFILE_PATH").expect("the dotfile folder")));
    assert_eq!(
        read().await,
        (RunState::Failed, vec![absent("toolbox"), absent("web")]),
        "web was made, then destroyed: gone, and the run failed"
    );

    refused_as_gone(&run.resume(&opts, &Recorder::new()).await.expect_err("web is gone"));
    assert_eq!(stack.lock().unwrap_or_else(PoisonError::into_inner).ups, 1, "nothing was started");

    run.up(&opts, &Recorder::new()).await.expect("up makes it again");
    assert_eq!(read().await, (RunState::Ready, vec![absent("toolbox"), running("web")]));
}

#[tokio::test]
async fn a_vm_made_again_has_its_pinned_host_keys_forgotten_and_one_kept_keeps_them() {
    let (stack, fake, _root, project, runner) = virtualbox_stack();
    let run = runner.open(&project, None).expect("opened");
    let opts = RunOptions::new().with_provider(crate::Provider::Virtualbox);
    run.up(&opts, &Recorder::new()).await.expect("up");
    let up = fake.made("vagrant up").pop().expect("an up");
    let dot = Path::new(up.env("VAGRANT_DOTFILE_PATH").expect("the dotfile folder")).to_path_buf();
    let known = dot.parent().expect("the run's state folder").join(crate::ssh::KNOWN_HOSTS);
    std::fs::write(&known, "10.0.0.5 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOld\n").expect("a console pinned web's keys");

    run.update(&opts, &Recorder::new()).await.expect("an update keeps web's VM");
    assert!(known.exists(), "the same VM: its keys still hold");

    destroy_web_outside(&stack, &dot);
    run.up(&opts, &Recorder::new()).await.expect("up makes web again");
    assert!(!known.exists(), "a new VM, maybe at the old address: its old keys would refuse it");
}
