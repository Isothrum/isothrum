//! The `docker-vm` fixture on a Proxmox server: the VM made by a fake Terraform, everything in
//! it reached over `ssh` to its address.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use isoloom_manifest::HostOs;

use super::{Engine, FLAG, VM_IP, compose};
use crate::process::fake::{Call, FakeProcesses, Reply};
use crate::secrets::{SecretValue, Secrets};
use crate::testing::{DOCKER_VM_PROXMOX, LabFolder, processes, runner_config};
use crate::{Park, Recorder, RunOptions, Runner};

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

/// A Proxmox server's run of the lab: its folder, fakes, runner and options.
struct Server {
    folder: LabFolder,
    fake: Arc<FakeProcesses>,
    runner: Runner,
    options: RunOptions,
}

fn proxmox() -> Server {
    let folder = LabFolder::new("docker-vm/proxmox", DOCKER_VM_PROXMOX);
    std::fs::write(folder.target_dir().join("main.tf"), "").unwrap();
    std::fs::write(folder.target_dir().join("setup.sh"), "set -e\n").unwrap();
    let engine = Arc::new(Mutex::new(Engine::default()));
    let fake = Arc::new(FakeProcesses::new({
        let (engine, home) = (Arc::clone(&engine), folder.home.path().to_path_buf());
        move |call| server_tools(&home, &engine, call)
    }));
    let runner = Runner::new(runner_config(folder.home.path(), processes(&fake), crate::testing::no_ports(), HostOs::Linux)).unwrap();
    let mut secrets = Secrets::new();
    secrets.insert("CTF_FLAG_MAIN", SecretValue::new(FLAG));
    secrets.insert("proxmox_password", SecretValue::new("pw"));
    let options = RunOptions::new()
        .with_param("proxmox_endpoint", "https://172.16.1.10:8006/")
        .with_param("node", "pve")
        .with_secrets(secrets)
        .with_host_label("pve-run");
    Server { folder, fake, runner, options }
}

#[tokio::test(start_paused = true)]
async fn on_proxmox_terraform_makes_the_vm_and_everything_in_it_goes_over_ssh_to_its_address() {
    let Server {
        folder,
        fake,
        runner,
        options: server,
        ..
    } = proxmox();
    let run = runner.open(&folder.manifest, None).unwrap();
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

/// The run's SSH jump (a node whose run bridge this host can't route to) carries a console as it
/// carries the guest's own steps.
#[tokio::test(start_paused = true)]
async fn a_console_goes_through_the_runs_ssh_jump() {
    let s = proxmox();
    let run = s.runner.open(&s.folder.manifest, None).unwrap();
    run.up(&s.options.clone().with_ssh_jump("root@pve.lan"), &Recorder::new()).await.unwrap();
    let console = run.console_command("web").unwrap();
    assert!(console.args.windows(2).any(|w| w == ["-J", "root@pve.lan"]), "{:?}", console.args);
}

/// A console into a service of the lab in the server's VM: `ssh -t` to the VM as the guest's
/// user, then into the service's container.
#[tokio::test(start_paused = true)]
async fn on_proxmox_a_console_into_a_service_goes_over_ssh_into_its_container() {
    let s = proxmox();
    let run = s.runner.open(&s.folder.manifest, None).unwrap();
    run.up(&s.options, &Recorder::new()).await.unwrap();
    let console = run.console_command("web").unwrap();
    assert_eq!(console.program, "ssh");
    assert_eq!(console.args[0], "-t");
    assert!(console.args.contains(&format!("isoloom@{VM_IP}")), "{:?}", console.args);
    assert!(
        console.args.last().unwrap().contains("label=com.docker.compose.service=web"),
        "{:?}",
        console.args
    );
}

/// On a server, a Compose service in the run's VM is removed there over SSH, as the guest's
/// own steps reach it, with the containers in its namespace; the module's apply isn't run.
#[tokio::test(start_paused = true)]
async fn on_proxmox_retiring_a_service_removes_it_in_the_vm_over_ssh() {
    let s = proxmox();
    let run = s.runner.open(&s.folder.manifest, None).unwrap();
    run.up(&s.options, &Recorder::new()).await.unwrap();
    let applies = s.fake.calls().iter().filter(|c| c.line().contains("apply")).count();
    run.retire("web", &s.options, &Recorder::new()).await.unwrap();
    let rm = s.fake.last("label=com.docker.compose.service=web").expect("the service removed in the VM");
    assert_eq!(rm.program, "ssh");
    assert!(rm.args.iter().any(|a| a == &format!("isoloom@{VM_IP}")), "{:?}", rm.args);
    assert!(rm.args.last().unwrap().contains("sudo docker rm -f"), "{:?}", rm.args);
    assert_eq!(s.fake.calls().iter().filter(|c| c.line().contains("apply")).count(), applies, "no apply");
}
