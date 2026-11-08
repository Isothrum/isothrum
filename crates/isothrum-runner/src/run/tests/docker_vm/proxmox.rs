//! The `docker-vm` fixture on a Proxmox server: the VM made by a fake Terraform, everything in
//! it reached over `ssh` to its address, the ad-hoc attacker box included.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use isoloom_manifest::{HostOs, Role};

use super::{BOX_IP, Engine, FLAG, VM_IP, compose, running_box};
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
    engine: Arc<Mutex<Engine>>,
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
    Server {
        folder,
        fake,
        engine,
        runner,
        options,
    }
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

/// On a VM Terraform made (a server, a cloud), the ad-hoc attacker box is a container inside that
/// VM too, reached as the guest's own steps reach it: `ssh` to its address as the guest's user,
/// with the run's SSH configuration. Status finds it, the shell is `ssh -t` then `docker exec`, a
/// detach removes it.
#[tokio::test(start_paused = true)]
async fn on_proxmox_the_attack_box_is_a_container_in_the_vm_reached_over_ssh() {
    let s = proxmox();
    let run = s.runner.open(&s.folder.manifest, None).unwrap();
    run.up(&s.options, &Recorder::new()).await.unwrap();
    assert!(run.status().await.unwrap().machines.iter().all(|m| m.role != Role::Access), "no box yet");
    let report = run.attach(None, None, Some("cyberctf/attack-box"), &Recorder::new()).await.unwrap();
    let name = format!("{}-isoloom-access", run.name());
    assert_eq!(report.machine, name);
    assert_eq!((report.network.as_deref(), report.address.as_deref()), (Some("lab"), Some(BOX_IP)));
    let started = s.fake.calls().into_iter().find(|c| c.line().contains("docker run -d")).unwrap();
    assert_eq!(started.program, "ssh");
    let config = s.runner.store.run_dir(run.name()).unwrap().join("ssh_config");
    assert_eq!(
        started.args,
        [
            "-F".to_owned(),
            config.display().to_string(),
            format!("isoloom@{VM_IP}"),
            format!("sudo docker run -d --name '{name}' --label isoloom.access=true --network 'flag-portal_lab' 'cyberctf/attack-box' sleep infinity"),
        ]
    );
    assert_eq!(
        s.engine.lock().unwrap().attacker,
        Some(("cyberctf/attack-box".into(), vec!["flag-portal_lab".into()]))
    );

    running_box(&run).await;

    let shell = run.shell_command(None).unwrap();
    assert_eq!(shell.program, "ssh");
    assert_eq!(
        shell.args[..4],
        ["-t".to_owned(), "-F".to_owned(), config.display().to_string(), format!("isoloom@{VM_IP}")]
    );
    assert!(
        shell.args[4].starts_with(&format!("sudo docker exec -it '{name}' sh -c '")),
        "{}",
        shell.args[4]
    );
    assert_eq!(shell.env, Vec::<(String, String)>::new());

    run.detach(None, &Recorder::new()).await.unwrap();
    let rm = s.fake.last("docker rm -f").unwrap();
    assert_eq!((rm.program.as_str(), rm.args.last().unwrap()), ("ssh", &format!("sudo docker rm -f '{name}'")));
    assert!(s.engine.lock().unwrap().attacker.is_none());
    assert!(run.status().await.unwrap().machines.iter().all(|m| m.role != Role::Access), "the box is gone");
    run.down(&s.options, &Recorder::new()).await.unwrap();
}

/// The run's SSH jump (a node whose run bridge this host can't route to) carries the box's calls
/// and its shell as it carries the guest's own.
#[tokio::test(start_paused = true)]
async fn the_attack_box_goes_through_the_runs_ssh_jump() {
    let s = proxmox();
    let run = s.runner.open(&s.folder.manifest, None).unwrap();
    run.up(&s.options.clone().with_ssh_jump("root@pve.lan"), &Recorder::new()).await.unwrap();
    run.attach(None, None, None, &Recorder::new()).await.unwrap();
    let started = s.fake.calls().into_iter().find(|c| c.line().contains("docker run -d")).unwrap();
    assert!(started.args.windows(2).any(|w| w == ["-J", "root@pve.lan"]), "{:?}", started.args);
    let shell = run.shell_command(None).unwrap();
    assert!(shell.args.windows(2).any(|w| w == ["-J", "root@pve.lan"]), "{:?}", shell.args);
}
