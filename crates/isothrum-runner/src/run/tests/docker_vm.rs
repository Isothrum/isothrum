//! Scripted runs of Isoloom's `docker-vm` fixtures (web-portal): the VM made by a fake Vagrant
//! (VirtualBox) or a fake Terraform (Proxmox), the containers by a fake Compose inside it,
//! reached through `vagrant ssh` or `ssh`. No real tool ever runs.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use isoloom_manifest::{HostOs, Protocol, Role};

use crate::net::FakePorts;
use crate::process::fake::{Call, FakeProcesses, Reply};
use crate::secrets::{SecretValue, Secrets};
use crate::testing::{DOCKER_VM, RunFolder, modelled, processes, runner_config, virtualbox_options, with_boot_digest};
use crate::{ErrorCode, EventKind, MachineState, Park, Recorder, RunOptions, Runner, StepState};

/// The VM the containers run in, as the run manifest names it.
const HOST: &str = "isoloom-docker-host";
const SECRET: &str = "token-vm-docker-0001";
/// The Docker host's address the fake apply reports.
const VM_IP: &str = "10.10.10.9";

/// The VM and the containers in it.
#[derive(Debug, Default)]
struct Engine {
    /// The VM's state label (absent: `not_created`).
    vm: Option<String>,
    /// Whether Compose started the run in it.
    up: bool,
    /// The secret the start's session carried, as the VM got it.
    secret: Option<String>,
    /// How many reads of the containers still miss (the way into the VM refused).
    ps_misses: u32,
    /// Whether a read of the containers prints something that isn't Compose's JSON.
    ps_garbled: bool,
    /// How many times the VM was restarted (`vagrant reload`).
    reloads: u32,
    /// Whether a restart fails (the VM left as it was).
    reload_fails: bool,
    /// What pulling the images in the VM does.
    pull: downloads::Pull,
}

/// `docker compose ps --all --format json` of the started run, one object a line.
const PS: &str = r#"{"Service":"web","State":"running","Health":"healthy","Publishers":[{"PublishedPort":8080,"TargetPort":80,"Protocol":"tcp"}],"Labels":""}
{"Service":"db","State":"running","Health":"healthy","Publishers":null,"Labels":""}
{"Service":"isoloom-init-web-1","State":"exited","ExitCode":0,"Publishers":[],"Labels":""}"#;

/// What Compose in the VM answers to `command`, `env` the session's.
fn compose(e: &mut Engine, command: &str, call: &Call) -> Reply {
    if e.vm.as_deref() != Some("running") {
        return Reply::fail(255, "ssh: connect to host: Connection refused");
    }
    if command.contains(" pull --policy ") {
        return downloads::pulled(&mut e.pull);
    }
    if command.contains(" up --detach ") {
        e.up = true;
        e.secret = call.env("DEMO_SECRET").map(str::to_owned);
        return Reply::ok("Container web-portal-web-1  Healthy");
    }
    if command.contains("ps --all --format json") {
        if e.ps_misses > 0 {
            e.ps_misses -= 1;
            return Reply::fail(255, "ssh: connect to host 127.0.0.1 port 2222: Connection refused");
        }
        if e.ps_garbled {
            return Reply::ok("Error response from daemon: not json");
        }
        return Reply::ok(if e.up { PS } else { "" });
    }
    assert!(!command.starts_with("sudo docker "), "no container of the runner's own: {command}");
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
        // A restart stops the containers; Docker comes back empty until Compose starts them.
        ("vagrant", ["reload", "default", "--no-provision"]) if e.reload_fails => Reply::fail(1, "VBoxManage: error: the machine is locked"),
        ("vagrant", ["reload", "default", "--no-provision"]) if e.vm.as_deref() == Some("running") => {
            e.reloads += 1;
            e.up = false;
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
    _folder: RunFolder,
    manifest: PathBuf,
    fake: Arc<FakeProcesses>,
    engine: Arc<Mutex<Engine>>,
    ports: Arc<FakePorts>,
    runner: Runner,
}

fn local() -> Running {
    local_with(DOCKER_VM)
}

/// [`local`] of `manifest`.
fn local_with(manifest: &str) -> Running {
    let folder = RunFolder::new("docker-vm", manifest);
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
    virtualbox_options(SECRET)
}

#[tokio::test(start_paused = true)]
async fn on_vagrant_the_vm_comes_up_then_compose_in_it_and_the_ports_answer_on_this_computer() {
    let l = local();
    let run = l.runner.open(&l.manifest, None).unwrap();
    let started = run.up(&opts(), &Recorder::new()).await.unwrap();
    let up = l.fake.last("vagrant up").unwrap();
    assert!(up.line().ends_with("--provision-with nat-route,project,docker,accept-env"), "{}", up.line());
    // The secret reached Compose in the VM through the session, named by Vagrant's forward_env.
    let start = l.fake.calls().into_iter().find(|c| c.line().contains(" up --detach ")).unwrap();
    assert_eq!(start.env("DEMO_SECRET"), Some(SECRET));
    assert!(!start.line().contains(SECRET));
    assert_eq!(l.engine.lock().unwrap().secret.as_deref(), Some(SECRET));
    let page = started.outputs.iter().find(|o| o.name == "web__portal").unwrap();
    assert_eq!(page.value.as_deref(), Some("http://127.0.0.1:8080/"));
    let status = run.status().await.unwrap();
    let state = |name: &str| status.machines.iter().find(|m| m.name == name).unwrap().state.clone();
    assert_eq!(
        (state(HOST), state("web"), state("isoloom-init-web-1")),
        (MachineState::Running, MachineState::Running, MachineState::Exited(0))
    );
}

/// On QEMU, vagrant-qemu forwards every VM's SSH (guest 22) to one fixed host port (50022) with
/// weak auto-correct, so two local docker-vm runs — or a half-created VM left behind — collide on
/// it. The runner gives the helper VM's SSH forward a free host port of its own, carried to the
/// Vagrantfile in `ISOLOOM_HOSTPORTS` under `<host>/ssh` ([`HOST`], the guest machine's name); the run's own
/// `web/portal` is fixed, so that is the only entry.
#[tokio::test(start_paused = true)]
async fn on_qemu_the_helper_vms_ssh_forward_takes_a_free_host_port() {
    let folder = RunFolder::new("docker-vm", DOCKER_VM);
    let (_engine, fake) = modelled(vagrant);
    let ports: Arc<dyn crate::HostPorts> = Arc::new(FakePorts::new([50123]));
    let runner = Runner::new(runner_config(folder.home.path(), processes(&fake), ports, HostOs::Macos)).unwrap();
    let run = runner.open(&folder.manifest, None).unwrap();
    let mut secrets = Secrets::new();
    secrets.insert("DEMO_SECRET", SecretValue::new(SECRET));
    run.up(
        &RunOptions::new().with_provider(isoloom_manifest::Provider::Qemu).with_secrets(secrets),
        &Recorder::new(),
    )
    .await
    .unwrap();
    let up = fake.last("vagrant up").unwrap();
    assert_eq!(
        up.env("ISOLOOM_HOSTPORTS"),
        Some(format!(r#"{{"{HOST}/ssh":50123}}"#).as_str()),
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

/// An ephemeral port no free host port is left for fails the start, naming the port, as Compose's
/// pick does: it was left out in silence, and the VM forwarded it at its `preferred` number, which
/// a resume needn't get again.
#[tokio::test(start_paused = true)]
async fn an_ephemeral_port_with_no_free_host_port_left_fails_the_start() {
    let ephemeral = DOCKER_VM.replace(r#""mode": "fixed""#, r#""mode": "ephemeral""#);
    assert_ne!(ephemeral, DOCKER_VM, "the fixture's port is fixed");
    let l = local_with(&ephemeral);
    let run = l.runner.open(&l.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert!(
        matches!(&err, crate::Error::ToolFailed { step, reason } if step == "host-ports"
            && reason.starts_with("no free host port over tcp that another run starting hasn't reserved could be found to pin machine `")),
        "{err}"
    );
    assert!(l.fake.last("vagrant up").is_none(), "nothing was made");
}

/// The run up in its VM, then paused.
async fn up_then_paused(l: &Running) -> crate::Running<'_> {
    let run = l.runner.open(&l.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    run.park(Park::Pause, &RunOptions::new(), &Recorder::new()).await.unwrap();
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
    assert!(
        run.park(Park::Shutdown, &RunOptions::new(), &Recorder::new()).await.is_err(),
        "a container run in a VM only pauses"
    );
    run.resume(&RunOptions::new(), &Recorder::new()).await.unwrap();
    run.down(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert!(l.engine.lock().unwrap().vm.is_none());
}

/// A container run in a VM declares no access machine: the runner puts no box of its own in the
/// VM, and an attach says so.
#[tokio::test(start_paused = true)]
async fn a_container_lab_in_a_vm_gets_no_attack_box_of_the_runners() {
    let l = local();
    let run = l.runner.open(&l.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    crate::testing::refuses_a_box_it_has_none_of(&run, &l.fake).await;
    assert!(run.status().await.unwrap().machines.iter().all(|m| m.role != Role::Access));
}

/// The way into the VM can miss once (an SSH port read from the hypervisor that answers late): a
/// running run still reads as running, and only a read that misses again counts as unreadable.
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

/// A read that prints no container the runner can read is no read: the containers of a running
/// VM read as unreadable, not absent.
#[tokio::test(start_paused = true)]
async fn container_output_that_cant_be_read_is_unreadable_not_absent() {
    let l = local();
    let run = l.runner.open(&l.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    l.engine.lock().unwrap().ps_garbled = true;
    let status = run.status().await.unwrap();
    let web = &status.machines.iter().find(|m| m.name == "web").unwrap().state;
    assert!(
        matches!(web, MachineState::Unknown(why) if why.starts_with("unreadable: ") && why.contains("printed no container")),
        "{web:?}"
    );
}

mod proxmox;

/// A console into a service of the run in the VM: `vagrant ssh` into the VM, then into the
/// service's container, found by its Compose label.
#[tokio::test(start_paused = true)]
async fn a_console_into_a_service_goes_through_the_vm_into_its_container() {
    let l = local();
    let run = l.runner.open(&l.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let console = run.console_command("web", None).unwrap();
    assert_eq!(console.program, "vagrant");
    assert_eq!(console.args[..3], ["ssh", "default", "-c"]);
    assert!(console.args[3].contains("label=com.docker.compose.service=web"), "{}", console.args[3]);
    let vm = run.console_command(HOST, None).unwrap();
    assert_eq!(vm.args, ["ssh", "default"], "the VM itself: its define");
}

/// As an account: into the service's container with `--user`, into the VM itself with a `sudo -i`
/// login.
#[tokio::test(start_paused = true)]
async fn a_console_into_the_vm_or_a_service_opens_as_the_account_asked() {
    let l = local();
    let run = l.runner.open(&l.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let console = run.console_command("web", Some("user")).unwrap();
    assert_eq!(console.args[..3], ["ssh", "default", "-c"]);
    assert!(console.args[3].starts_with("sudo docker exec -it --user user "), "{}", console.args[3]);
    let vm = run.console_command(HOST, Some("user")).unwrap();
    assert_eq!(vm.args, ["ssh", "default", "-c", &crate::driver::shells::login_shell_as("user")]);
}

/// A console is handed to the embedder, which prints and runs it: it carries nothing the manifest
/// delivers to a step, while a step into the VM (a retire's) gets every delivery naming it.
#[tokio::test(start_paused = true)]
async fn a_console_into_the_vm_carries_no_step_delivery_and_a_retire_does() {
    const TOKEN: &str = "token-0001-secret";
    const REGION: &str = "region-0001";
    let mut m: serde_json::Value = serde_json::from_str(DOCKER_VM).unwrap();
    let every_step = |var: &str| serde_json::json!([{ "via": "env", "var": var, "steps": ["*"] }]);
    m["secrets"].as_array_mut().unwrap().push(serde_json::json!({
        "name": "LAB_TOKEN", "kind": "input", "required": false, "sensitive": true, "deliver": every_step("LAB_TOKEN"),
    }));
    m["params"].as_array_mut().unwrap().push(serde_json::json!({
        "name": "RUN_REGION", "description": "Where the run goes.", "set_by": "embedder", "deliver": every_step("RUN_REGION"),
    }));
    let l = local_with(&m.to_string());
    let run = l.runner.open(&l.manifest, None).unwrap();
    let mut opts = opts();
    opts.secrets.insert("LAB_TOKEN", SecretValue::new(TOKEN));
    opts.params.insert("RUN_REGION".into(), REGION.into());
    run.up(&opts, &Recorder::new()).await.unwrap();
    let console = run.console_command("web", None).unwrap();
    let words = format!("{:?} {:?}", console.args, console.env);
    assert!(!words.contains(TOKEN) && !words.contains(REGION), "{words}");
    run.retire("web", &opts, &Recorder::new()).await.unwrap();
    let rm = l.fake.last("label=com.docker.compose.service=web").unwrap();
    assert_eq!(rm.args[..3], ["ssh", "default", "-c"]);
    assert_eq!((rm.env("LAB_TOKEN"), rm.env("RUN_REGION")), (Some(TOKEN), Some(REGION)));
}

/// Retiring the VM the run's containers run in is refused: destroying it would take every
/// container with it. Nothing is destroyed.
#[tokio::test(start_paused = true)]
async fn the_vm_the_containers_run_in_is_not_retired() {
    let l = local();
    let run = l.runner.open(&l.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let err = run.retire(HOST, &RunOptions::new(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Unsupported, "{err}");
    assert!(l.fake.last("destroy").is_none(), "nothing destroyed");
    assert_eq!(l.engine.lock().unwrap().vm.as_deref(), Some("running"));
}

/// A Compose service in the VM is removed there, with the containers in its namespace, through
/// `vagrant ssh` into the VM; nothing of the VM goes.
#[tokio::test(start_paused = true)]
async fn retiring_a_service_in_the_vm_removes_it_there() {
    let l = local();
    let run = l.runner.open(&l.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    run.retire("web", &RunOptions::new(), &Recorder::new()).await.unwrap();
    let rm = l.fake.last("label=com.docker.compose.service=web").expect("the service removed in the VM");
    assert_eq!(
        (rm.program.as_str(), &rm.args[..3]),
        ("vagrant", &["ssh".to_owned(), "default".to_owned(), "-c".to_owned()][..])
    );
    assert!(rm.args[3].contains("sudo docker rm -f"), "{}", rm.args[3]);
    assert!(l.fake.last("destroy").is_none(), "nothing of the VM goes");
}

/// The VM's boot digest in the fixture.
const BOOTED: &str = "sha256:5287ee9cf9a9dbf65c880529e7d490c7e0fb5036ee8fd50eb57f16502ef18e44";

/// The steps `events` show, with their states.
fn steps(events: &Recorder) -> Vec<(String, StepState)> {
    events
        .events()
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::Step { step, state, .. } => Some((step, state)),
            _ => None,
        })
        .collect()
}

/// A container given a host folder after the start: the regenerated Vagrantfile shares it into
/// the VM, which only a boot applies. The update restarts the running VM first, then its start
/// steps provision it and start the containers again, which then see the folder.
#[tokio::test(start_paused = true)]
async fn an_update_restarts_the_vm_whose_boot_settings_changed_then_starts_its_containers_again() {
    let l = local();
    let run = l.runner.open(&l.manifest, None).unwrap();
    let started = run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(started.record.vm_boot_digests.get(HOST).map(String::as_str), Some(BOOTED));
    std::fs::write(&l.manifest, with_boot_digest(DOCKER_VM, HOST, Some("sha256:shared-folder"))).unwrap();
    let before = l.fake.calls().len();
    let events = Recorder::new();
    let updated = l.runner.open(&l.manifest, None).unwrap().update(&opts(), &events).await.unwrap();
    let during: Vec<String> = l.fake.calls()[before..].iter().map(Call::line).collect();
    let reloads: Vec<usize> = (0..during.len()).filter(|i| during[*i].starts_with("vagrant reload")).collect();
    assert_eq!(reloads.len(), 1, "{during:?}");
    assert_eq!(during[reloads[0]], "vagrant reload default --no-provision");
    let up = during.iter().position(|c| c.starts_with("vagrant up")).unwrap();
    let start = during.iter().position(|c| c.contains(" up --detach ")).unwrap();
    assert!(reloads[0] < up && up < start, "restarted, then provisioned, then the containers: {during:?}");
    assert!(l.engine.lock().unwrap().up, "the containers run again");
    let shown = steps(&events);
    let reload = |state| shown.contains(&(format!("reload-{HOST}"), state));
    assert!(reload(StepState::Running) && reload(StepState::Done), "{shown:?}");
    assert_eq!(
        updated.record.vm_boot_digests.get(HOST).map(String::as_str),
        Some("sha256:shared-folder"),
        "the new digest is recorded"
    );
    // Updated again with the same manifest: nothing to restart.
    l.runner.open(&l.manifest, None).unwrap().update(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(l.engine.lock().unwrap().reloads, 1);
}

/// A restart that fails fails its step and the update, before `up`'s steps run, and leaves the
/// digest the VM booted with recorded: the next update sees the change again and restarts it.
#[tokio::test(start_paused = true)]
async fn a_failed_restart_fails_the_update_and_the_next_update_restarts_again() {
    let l = local();
    let run = l.runner.open(&l.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    std::fs::write(&l.manifest, with_boot_digest(DOCKER_VM, HOST, Some("sha256:shared-folder"))).unwrap();
    l.engine.lock().unwrap().reload_fails = true;
    let before = l.fake.calls().len();
    let events = Recorder::new();
    let err = l.runner.open(&l.manifest, None).unwrap().update(&opts(), &events).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::ToolFailed, "{err}");
    let shown = steps(&events);
    let reload = |state| shown.contains(&(format!("reload-{HOST}"), state));
    assert!(reload(StepState::Running) && reload(StepState::Failed), "{shown:?}");
    let during: Vec<String> = l.fake.calls()[before..].iter().map(Call::line).collect();
    assert!(during.iter().all(|c| !c.starts_with("vagrant up")), "no start step ran: {during:?}");
    let record = l.runner.store.record(run.name()).unwrap().unwrap();
    assert_eq!(record.vm_boot_digests.get(HOST).map(String::as_str), Some(BOOTED), "what the VM booted with");

    l.engine.lock().unwrap().reload_fails = false;
    let updated = l.runner.open(&l.manifest, None).unwrap().update(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(l.engine.lock().unwrap().reloads, 1, "restarted at the next update");
    assert_eq!(updated.record.vm_boot_digests.get(HOST).map(String::as_str), Some("sha256:shared-folder"));
}

#[tokio::test(start_paused = true)]
async fn an_update_with_the_same_boot_settings_restarts_nothing() {
    let l = local();
    let run = l.runner.open(&l.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let events = Recorder::new();
    run.update(&opts(), &events).await.unwrap();
    assert_eq!(l.engine.lock().unwrap().reloads, 0);
    assert!(steps(&events).iter().all(|(s, _)| !s.starts_with("reload-")));
}

/// A run started from a manifest without boot digests (or by a runner that kept none) has no
/// recorded digest: its first update can't tell what the VM booted with, so it restarts nothing
/// and records the manifest's; a later change is then restarted for.
#[tokio::test(start_paused = true)]
async fn an_unknown_boot_digest_restarts_nothing_and_is_recorded_for_the_next_update() {
    let l = local();
    std::fs::write(&l.manifest, with_boot_digest(DOCKER_VM, HOST, None)).unwrap();
    let started = l.runner.open(&l.manifest, None).unwrap().up(&opts(), &Recorder::new()).await.unwrap();
    assert!(started.record.vm_boot_digests.is_empty());
    std::fs::write(&l.manifest, DOCKER_VM).unwrap();
    let updated = l.runner.open(&l.manifest, None).unwrap().update(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(l.engine.lock().unwrap().reloads, 0, "an unknown digest never restarts");
    assert_eq!(updated.record.vm_boot_digests.get(HOST).map(String::as_str), Some(BOOTED));
    std::fs::write(&l.manifest, with_boot_digest(DOCKER_VM, HOST, Some("sha256:other"))).unwrap();
    l.runner.open(&l.manifest, None).unwrap().update(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(l.engine.lock().unwrap().reloads, 1);
}

mod downloads;
