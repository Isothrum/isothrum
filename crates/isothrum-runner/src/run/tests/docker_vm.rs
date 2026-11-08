//! Scripted runs of Isoloom's `docker-vm` fixtures (flag-portal): the VM made by a fake Vagrant
//! (VirtualBox) or a fake Terraform (Proxmox), the containers by a fake Compose inside it,
//! reached through `vagrant ssh` or `ssh`, and the ad-hoc attacker box by a fake Docker in the
//! VM. No real tool ever runs.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use isoloom_manifest::{HostOs, Protocol, Role};

use crate::net::FakePorts;
use crate::process::fake::{Call, FakeProcesses, Reply};
use crate::secrets::{SecretValue, Secrets};
use crate::testing::{DOCKER_VM, LabFolder, modelled, processes, runner_config, virtualbox_options};
use crate::{ErrorCode, MachineState, Park, Recorder, RunOptions, Runner};

const FLAG: &str = "flag{vm-docker-0001}";
/// The Docker host's address the fake apply reports.
const VM_IP: &str = "10.10.10.9";

/// The VM and the containers in it.
#[derive(Debug, Default)]
struct Engine {
    /// The VM's state label (absent: `not_created`).
    vm: Option<String>,
    /// Whether Compose started the run in it.
    up: bool,
    /// The flag the start's session carried, as the VM got it.
    flag: Option<String>,
    /// The ad-hoc attacker box in the VM: its image, and the Docker networks it is on.
    attacker: Option<(String, Vec<String>)>,
    /// How many reads of the containers still miss (the way into the VM refused).
    ps_misses: u32,
    /// The way into the VM refuses every `docker` command (the VM stopped answering).
    unreachable: bool,
}

/// The address the fake Docker gives the attacker box on every network.
const BOX_IP: &str = "10.30.0.77";

/// `docker compose ps --all --format json` of the started run, one object a line.
const PS: &str = r#"{"Service":"web","State":"running","Health":"healthy","Publishers":[{"PublishedPort":8080,"TargetPort":80,"Protocol":"tcp"}],"Labels":""}
{"Service":"db","State":"running","Health":"healthy","Publishers":null,"Labels":""}
{"Service":"web-init-1","State":"exited","ExitCode":0,"Publishers":[],"Labels":""}"#;

/// What Compose in the VM answers to `command`, `env` the session's.
fn compose(e: &mut Engine, command: &str, call: &Call) -> Reply {
    if e.vm.as_deref() != Some("running") {
        return Reply::fail(255, "ssh: connect to host: Connection refused");
    }
    if command.contains(" up --detach ") {
        e.up = true;
        e.flag = call.env("CTF_FLAG_MAIN").map(str::to_owned);
        return Reply::ok("Container flag-portal-web-1  Healthy");
    }
    if command.contains("ps --all --format json") {
        if e.ps_misses > 0 {
            e.ps_misses -= 1;
            return Reply::fail(255, "ssh: connect to host 127.0.0.1 port 2222: Connection refused");
        }
        return Reply::ok(if e.up { PS } else { "" });
    }
    if let Some(rest) = command.strip_prefix("sudo docker ") {
        return docker(e, rest);
    }
    Reply::ok("")
}

/// The single-quoted words of a `docker` command the runner wrote (`sh_quote`d names).
fn quoted(command: &str) -> Vec<&str> {
    command.split('\'').skip(1).step_by(2).collect()
}

/// What root's Docker in the VM answers to `docker <rest>`: the attacker box's run, removal,
/// network joins and inspects (no other container is ever addressed by name).
fn docker(e: &mut Engine, rest: &str) -> Reply {
    if e.unreachable {
        return Reply::fail(255, "ssh: connect to host 127.0.0.1 port 2222: Connection refused");
    }
    let words = quoted(rest);
    let networks_json = |nets: &[String]| {
        let each: Vec<String> = nets.iter().map(|n| format!(r#""{n}":{{"IPAddress":"{BOX_IP}"}}"#)).collect();
        format!("{{{}}}", each.join(","))
    };
    if rest.starts_with("run -d --name ") {
        // `run -d --name '<name>' --label isoloom.access=true --network '<net>' '<image>' sleep infinity`
        assert!(rest.ends_with(" sleep infinity") && rest.contains("--label isoloom.access=true"), "{rest}");
        let [_, net, image] = words[..] else { panic!("{rest}") };
        e.attacker = Some((image.to_owned(), vec![net.to_owned()]));
        return Reply::ok("0123456789ab");
    }
    if rest.starts_with("rm -f ") {
        return match e.attacker.take() {
            Some(_) => Reply::ok(&words.join("")),
            None => Reply::fail(1, "Error response from daemon: No such container"),
        };
    }
    if rest.starts_with("network connect ") {
        let [net, _] = words[..] else { panic!("{rest}") };
        if let Some((_, nets)) = &mut e.attacker {
            nets.push(net.to_owned());
        }
        return Reply::ok("");
    }
    if rest.starts_with("inspect -f ") {
        let [format, _] = words[..] else { panic!("{rest}") };
        let Some((_, nets)) = &e.attacker else {
            return Reply::fail(1, "Error: No such object");
        };
        return Reply::ok(&match format {
            "{{json .NetworkSettings.Networks}}" => networks_json(nets),
            "{{.State.Running}}|{{json .NetworkSettings.Networks}}" => format!("true|{}", networks_json(nets)),
            other => panic!("an inspect format the fake doesn't model: {other}"),
        });
    }
    panic!("a docker command the fake doesn't model: {rest}")
}

fn vagrant(engine: &Mutex<Engine>, call: &Call) -> Reply {
    let mut e = engine.lock().unwrap_or_else(PoisonError::into_inner);
    let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
    match (call.program.as_str(), args.as_slice()) {
        ("vagrant", ["--version"]) => Reply::ok("Vagrant 2.4.9"),
        ("vagrant", ["plugin", "list", "--machine-readable"]) => Reply::ok("1,,plugin-name,vagrant-qemu\n"),
        ("VBoxManage", ["--version"]) => Reply::ok("7.2.20r175154"),
        ("vagrant", ["status", ..]) => Reply::ok(&format!("1,default,state,{}", e.vm.as_deref().unwrap_or("not_created"))),
        ("vagrant", ["up", ..]) => {
            e.vm = Some("running".into());
            Reply::ok("==> default: Machine booted and ready!")
        }
        ("vagrant", ["suspend"]) => {
            e.vm = Some("saved".into());
            Reply::ok("")
        }
        ("vagrant", ["destroy", ..]) => {
            *e = Engine::default();
            Reply::ok("")
        }
        ("vagrant", ["ssh", "--no-tty", "--command", command, "default"]) => {
            let command = (*command).to_owned();
            compose(&mut e, &command, call)
        }
        _ => Reply::ok(""),
    }
}

struct Running {
    _folder: LabFolder,
    manifest: PathBuf,
    fake: Arc<FakeProcesses>,
    engine: Arc<Mutex<Engine>>,
    ports: Arc<FakePorts>,
    runner: Runner,
}

fn local() -> Running {
    let folder = LabFolder::new("docker-vm", DOCKER_VM);
    let (engine, fake) = modelled(vagrant);
    let ports = Arc::new(FakePorts::new([]));
    let held: Arc<FakePorts> = Arc::clone(&ports);
    let as_port: Arc<dyn crate::HostPorts> = held;
    let runner = Runner::new(runner_config(folder.home.path(), processes(&fake), as_port, HostOs::Macos)).unwrap();
    Running {
        manifest: folder.manifest.clone(),
        _folder: folder,
        fake,
        engine,
        ports,
        runner,
    }
}

fn opts() -> RunOptions {
    virtualbox_options(FLAG)
}

#[tokio::test(start_paused = true)]
async fn on_vagrant_the_vm_comes_up_then_compose_in_it_and_the_ports_answer_on_this_computer() {
    let l = local();
    let run = l.runner.open(&l.manifest, None).unwrap();
    let started = run.up(&opts(), &Recorder::new()).await.unwrap();
    let up = l.fake.last("vagrant up").unwrap();
    assert!(up.line().ends_with("--provision-with docker,accept-env,project"), "{}", up.line());
    // The flag reached Compose in the VM through the session, named by Vagrant's forward_env.
    let start = l.fake.calls().into_iter().find(|c| c.line().contains(" up --detach ")).unwrap();
    assert_eq!(start.env("CTF_FLAG_MAIN"), Some(FLAG));
    assert!(!start.line().contains(FLAG));
    assert_eq!(l.engine.lock().unwrap().flag.as_deref(), Some(FLAG));
    let page = started.outputs.iter().find(|o| o.name == "web_portal").unwrap();
    assert_eq!(page.value.as_deref(), Some("http://127.0.0.1:8080/"));
    let status = run.status().await.unwrap();
    let state = |name: &str| status.machines.iter().find(|m| m.name == name).unwrap().state.clone();
    assert_eq!(
        (state("docker"), state("web"), state("web-init-1")),
        (MachineState::Running, MachineState::Running, MachineState::Exited(0))
    );
}

/// On QEMU, vagrant-qemu forwards every VM's SSH (guest 22) to one fixed host port (50022) with
/// weak auto-correct, so two local docker-vm labs — or a half-created VM left behind — collide on
/// it. The runner gives the helper VM's SSH forward a free host port of its own, carried to the
/// Vagrantfile in `ISOLOOM_HOSTPORTS` under `docker/ssh` (the guest machine's name); the lab's own
/// `web/portal` is fixed, so that is the only entry.
#[tokio::test(start_paused = true)]
async fn on_qemu_the_helper_vms_ssh_forward_takes_a_free_host_port() {
    let folder = LabFolder::new("docker-vm", DOCKER_VM);
    let (_engine, fake) = modelled(vagrant);
    let ports: Arc<dyn crate::HostPorts> = Arc::new(FakePorts::new([50123]));
    let runner = Runner::new(runner_config(folder.home.path(), processes(&fake), ports, HostOs::Macos)).unwrap();
    let run = runner.open(&folder.manifest, None).unwrap();
    let mut secrets = Secrets::new();
    secrets.insert("CTF_FLAG_MAIN", SecretValue::new(FLAG));
    run.up(&RunOptions::new().with_provider("qemu").with_secrets(secrets), &Recorder::new())
        .await
        .unwrap();
    let up = fake.last("vagrant up").unwrap();
    assert_eq!(
        up.env("ISOLOOM_HOSTPORTS"),
        Some(r#"{"docker/ssh":50123}"#),
        "the chosen SSH port reaches the Vagrantfile"
    );
}

#[tokio::test(start_paused = true)]
async fn a_forwarded_port_another_lab_holds_stops_the_start_before_the_vm_is_made() {
    let l = local();
    l.ports.take(8080, Protocol::Tcp);
    let run = l.runner.open(&l.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::PortInUse);
    assert!(l.fake.last("vagrant up").is_none(), "nothing was made");
}

/// The lab up in its VM, then paused.
async fn up_then_paused(l: &Running) -> crate::Running<'_> {
    let run = l.runner.open(&l.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    run.park(Park::Pause, &Recorder::new()).await.unwrap();
    run
}

#[tokio::test(start_paused = true)]
async fn a_parked_vm_holds_its_containers_as_it_does_and_down_removes_the_vm() {
    let l = local();
    let run = up_then_paused(&l).await;
    let status = run.status().await.unwrap();
    let web = status.machines.iter().find(|m| m.name == "web").unwrap();
    assert_eq!(web.state, MachineState::Stopped);
    assert_eq!(web.published, []);
    assert!(run.park(Park::Shutdown, &Recorder::new()).await.is_err(), "a container run in a VM only pauses");
    run.resume(&RunOptions::new(), &Recorder::new()).await.unwrap();
    run.down(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert!(l.engine.lock().unwrap().vm.is_none());
}

/// The ad-hoc attacker box of a container lab in a VM is a container inside that VM, on the lab's
/// Docker network there, through the run's own Vagrant (`vagrant ssh … docker …`, with the run's
/// state folder); status finds it, the shell goes through the VM into it, a detach removes it.
#[tokio::test(start_paused = true)]
async fn the_attack_box_is_a_container_inside_the_vm_on_the_labs_network() {
    let l = local();
    let run = l.runner.open(&l.manifest, None).unwrap();
    let not_up = run.attach(None, None, Some("kalilinux/kali-rolling"), &Recorder::new()).await.unwrap_err();
    assert_eq!(not_up.code(), ErrorCode::NotRunning);
    run.up(&opts(), &Recorder::new()).await.unwrap();
    assert!(run.status().await.unwrap().machines.iter().all(|m| m.role != Role::Access), "no box yet");
    let report = run.attach(None, None, Some("kalilinux/kali-rolling"), &Recorder::new()).await.unwrap();
    let name = format!("{}-isoloom-access", run.name());
    assert_eq!(
        (report.machine.as_str(), report.native.as_str()),
        ("isoloom-access", name.as_str()),
        "named as status names it"
    );
    assert_eq!((report.network.as_deref(), report.address.as_deref()), (Some("lab"), Some(BOX_IP)));
    let started = l.fake.calls().into_iter().find(|c| c.line().contains("docker run -d")).unwrap();
    assert_eq!(started.program, "vagrant");
    assert_eq!(started.args[..3], ["ssh", "--no-tty", "--command"]);
    assert_eq!(
        started.args[3],
        format!(
            "sudo docker run -d --name '{name}' --hostname attackbox --label isoloom.access=true --network 'flag-portal_lab' 'kalilinux/kali-rolling' sleep infinity"
        )
    );
    assert_eq!(started.args[4], "default", "into the lab's VM");
    assert!(started.env("VAGRANT_DOTFILE_PATH").is_some(), "with the run's state folder");
    assert_eq!(
        l.engine.lock().unwrap().attacker,
        Some(("kalilinux/kali-rolling".into(), vec!["flag-portal_lab".into()]))
    );

    let access = running_box(&run).await;
    assert_eq!(access.published, [], "nothing of it is forwarded to this computer");

    let shell = run.shell_command(None).unwrap();
    assert_eq!(shell.program, "vagrant");
    assert_eq!(shell.args[..3], ["ssh", "default", "-c"]);
    assert!(
        shell.args[3].starts_with(&format!("sudo docker exec -it '{name}' sh -c '")),
        "{}",
        shell.args[3]
    );
    assert_eq!(shell.cwd.as_deref(), l.manifest.parent());
    let dotfile = l.runner.store.run_dir(run.name()).unwrap().join("vagrant");
    assert!(
        shell.env.contains(&("VAGRANT_DOTFILE_PATH".to_owned(), dotfile.display().to_string())),
        "{:?}",
        shell.env
    );

    run.detach(None, &Recorder::new()).await.unwrap();
    assert_eq!(l.fake.last("docker rm -f").unwrap().args[3], format!("sudo docker rm -f '{name}'"));
    assert!(l.engine.lock().unwrap().attacker.is_none());
    assert!(run.status().await.unwrap().machines.iter().all(|m| m.role != Role::Access), "the box is gone");
    run.detach(None, &Recorder::new()).await.expect("a box already gone is no error");
}

/// A detach that can't reach the VM fails and says why: the box may still be running there.
#[tokio::test(start_paused = true)]
async fn a_detach_that_cannot_reach_the_vm_fails() {
    let l = local();
    let run = l.runner.open(&l.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    run.attach(None, None, Some("kalilinux/kali-rolling"), &Recorder::new()).await.unwrap();
    l.engine.lock().unwrap().unreachable = true;
    let err = run.detach(None, &Recorder::new()).await.unwrap_err();
    assert!(err.to_string().contains("Connection refused"), "{err}");
    assert!(l.engine.lock().unwrap().attacker.is_some(), "the box is still there");
}

/// A VM that is paused holds its containers as it does: no box goes into it.
#[tokio::test(start_paused = true)]
async fn no_attack_box_goes_into_a_parked_vm() {
    let l = local();
    let run = up_then_paused(&l).await;
    let err = run.attach(None, None, None, &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::NotRunning);
    assert!(l.fake.last("docker run").is_none(), "nothing went into the VM");
}

/// The way into the VM can miss once (an SSH port read from the hypervisor that answers late): a
/// running lab still reads as running, and only a read that misses again counts as unreadable.
#[tokio::test(start_paused = true)]
async fn a_container_read_that_misses_once_is_read_again() {
    let l = local();
    let run = l.runner.open(&l.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let reads = || l.fake.calls().iter().filter(|c| c.line().contains("ps --all --format json")).count();
    let before = reads();
    l.engine.lock().unwrap().ps_misses = 1;
    let status = run.status().await.unwrap();
    assert_eq!(status.machines.iter().find(|m| m.name == "web").unwrap().state, MachineState::Running);
    assert_eq!(reads() - before, 2, "read again after the miss");
    l.engine.lock().unwrap().ps_misses = 2;
    let web = run.status().await.unwrap().machines.iter().find(|m| m.name == "web").unwrap().state.clone();
    assert!(matches!(&web, MachineState::Unknown(why) if why.starts_with("unreadable: ")), "{web:?}");
}

/// The run's ad-hoc attacker box as status reports it: running, at [`BOX_IP`] on the lab network.
async fn running_box(run: &crate::Running<'_>) -> crate::MachineStatus {
    let status = run.status().await.unwrap();
    let access = status.machines.into_iter().find(|m| m.role == Role::Access).unwrap();
    assert_eq!((access.name.as_str(), &access.state), ("isoloom-access", &MachineState::Running));
    assert_eq!(access.addresses.get("lab").map(String::as_str), Some(BOX_IP));
    access
}

mod proxmox;
