//! Scripted runs of Isoloom's `docker-vm` fixtures (flag-portal): the VM made by a fake Vagrant
//! (VirtualBox) or a fake Terraform (Proxmox), the containers by a fake Compose inside it,
//! reached through `vagrant ssh` or `ssh`. No real tool ever runs.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use isoloom_manifest::{HostOs, Protocol};

use crate::net::FakePorts;
use crate::process::fake::{Call, FakeProcesses, Reply};
use crate::secrets::{SecretValue, Secrets};
use crate::testing::{DOCKER_VM, DOCKER_VM_PROXMOX, LabFolder, modelled, processes, runner_config, virtualbox_options};
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
}

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
        return Reply::ok(if e.up { PS } else { "" });
    }
    Reply::ok("")
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
    run.up(&RunOptions::new().with_provider("qemu").with_secrets(secrets), &Recorder::new()).await.unwrap();
    let up = fake.last("vagrant up").unwrap();
    assert_eq!(up.env("ISOLOOM_HOSTPORTS"), Some(r#"{"docker/ssh":50123}"#), "the chosen SSH port reaches the Vagrantfile");
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

#[tokio::test(start_paused = true)]
async fn a_parked_vm_holds_its_containers_as_it_does_and_down_removes_the_vm() {
    let l = local();
    let run = l.runner.open(&l.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    run.park(Park::Pause, &Recorder::new()).await.unwrap();
    let status = run.status().await.unwrap();
    let web = status.machines.iter().find(|m| m.name == "web").unwrap();
    assert_eq!(web.state, MachineState::Stopped);
    assert_eq!(web.published, []);
    assert!(run.park(Park::Shutdown, &Recorder::new()).await.is_err(), "a container run in a VM only pauses");
    run.resume(&RunOptions::new(), &Recorder::new()).await.unwrap();
    run.down(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert!(l.engine.lock().unwrap().vm.is_none());
}

/// The fake tools of a Proxmox run: Terraform writing the state, `ssh` to the Docker host.
fn server_tools(home: &Path, engine: &Mutex<Engine>, call: &Call) -> Reply {
    let line = call.line();
    if call.program == "ssh-keygen" {
        let key = crate::ssh::key_path(home);
        std::fs::write(&key, "private").unwrap();
        std::fs::write(key.with_extension("pub"), "ssh-ed25519 AAAAfake isothrum\n").unwrap();
        return Reply::ok("");
    }
    let state_dir = || Path::new(call.env("TF_DATA_DIR").unwrap()).parent().unwrap().to_path_buf();
    if line.contains("terraform apply") {
        let state = format!(r#"{{"outputs":{{"ip":{{"value":"{VM_IP}"}},"ssh_user":{{"value":"isoloom"}}}},"resources":[{{"instances":[{{}}]}}]}}"#);
        std::fs::write(state_dir().join("terraform.tfstate"), state).unwrap();
        engine.lock().unwrap().vm = Some("running".into());
        return Reply::ok("Apply complete!");
    }
    if line.contains("terraform destroy") {
        std::fs::write(state_dir().join("terraform.tfstate"), r#"{"outputs":{},"resources":[]}"#).unwrap();
        *engine.lock().unwrap() = Engine::default();
        return Reply::ok("Destroy complete!");
    }
    if call.program == "ssh" && call.args != ["-V"] {
        let command = call.args.last().cloned().unwrap_or_default();
        let mut e = engine.lock().unwrap_or_else(PoisonError::into_inner);
        return compose(&mut e, &command, call);
    }
    Reply::ok("")
}

#[tokio::test(start_paused = true)]
async fn on_proxmox_terraform_makes_the_vm_and_everything_in_it_goes_over_ssh_to_its_address() {
    let folder = LabFolder::new("docker-vm/proxmox", DOCKER_VM_PROXMOX);
    std::fs::write(folder.target_dir().join("main.tf"), "").unwrap();
    std::fs::write(folder.target_dir().join("setup.sh"), "set -e\n").unwrap();
    let engine = Arc::new(Mutex::new(Engine::default()));
    let fake = Arc::new(FakeProcesses::new({
        let (engine, home) = (Arc::clone(&engine), folder.home.path().to_path_buf());
        move |call| server_tools(&home, &engine, call)
    }));
    let runner = Runner::new(runner_config(folder.home.path(), processes(&fake), crate::testing::no_ports(), HostOs::Linux)).unwrap();
    let run = runner.open(&folder.manifest, None).unwrap();
    let mut secrets = Secrets::new();
    secrets.insert("CTF_FLAG_MAIN", SecretValue::new(FLAG));
    secrets.insert("proxmox_password", SecretValue::new("pw"));
    let server = RunOptions::new()
        .with_param("proxmox_endpoint", "https://172.16.1.10:8006/")
        .with_param("node", "pve")
        .with_secrets(secrets)
        .with_host_label("pve-run");
    let started = run.up(&server, &Recorder::new()).await.unwrap();
    let ssh: Vec<Call> = fake.calls().into_iter().filter(|c| c.program == "ssh" && c.args != ["-V"]).collect();
    assert!(ssh.iter().all(|c| c.args.contains(&format!("isoloom@{VM_IP}"))), "{ssh:?}");
    let state = runner.store.run_dir(run.name()).unwrap();
    let piped: Vec<Option<PathBuf>> = ssh.iter().map(|c| c.stdin.clone()).collect();
    assert!(piped.contains(&Some(folder.target_dir().join("setup.sh"))), "{piped:?}");
    assert!(piped.contains(&Some(state.join("project.tgz"))), "{piped:?}");
    let start = ssh.iter().find(|c| c.line().contains(" up --detach ")).unwrap();
    assert!(start.args.windows(2).any(|w| w == ["-o", "SendEnv=CTF_FLAG_MAIN"]));
    assert_eq!(start.env("TF_VAR_proxmox_password"), None);
    let page = started.outputs.iter().find(|o| o.name == "web_portal").unwrap();
    assert_eq!(page.value.as_deref(), Some(format!("http://{VM_IP}:8080/").as_str()));
    assert!(run.park(Park::Pause, &Recorder::new()).await.is_err(), "a server run isn't parked");
    run.down(&server, &Recorder::new()).await.unwrap();
}
