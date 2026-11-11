//! The `docker-vm` fixture on a Proxmox server: the VM made by a fake Terraform, everything in
//! it reached over `ssh` to its address.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use isoloom_manifest::HostOs;

use super::{Engine, SECRET, VM_IP, compose};
use crate::process::fake::{Call, FakeProcesses, Reply};
use crate::secrets::{SecretValue, Secrets};
use crate::testing::{DOCKER_VM_PROXMOX, RunFolder, processes, runner_config};
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

/// A run on a Proxmox server: its folder, fakes, runner and options.
struct Server {
    folder: RunFolder,
    fake: Arc<FakeProcesses>,
    runner: Runner,
    options: RunOptions,
}

fn proxmox() -> Server {
    proxmox_with(DOCKER_VM_PROXMOX)
}

/// [`proxmox`], `manifest` its `run.json`.
fn proxmox_with(manifest: &str) -> Server {
    let folder = RunFolder::new("docker-vm/proxmox", manifest);
    std::fs::write(folder.target_dir().join("main.tf"), "").unwrap();
    std::fs::write(folder.target_dir().join("setup.sh"), "set -e\n").unwrap();
    let engine = Arc::new(Mutex::new(Engine::default()));
    let fake = Arc::new(FakeProcesses::new({
        let (engine, home) = (Arc::clone(&engine), folder.home.path().to_path_buf());
        move |call| server_tools(&home, &engine, call)
    }));
    let runner = Runner::new(runner_config(folder.home.path(), processes(&fake), crate::testing::no_ports(), HostOs::Linux)).unwrap();
    let mut secrets = Secrets::new();
    secrets.insert("DEMO_SECRET", SecretValue::new(SECRET));
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
    assert!(start.args.windows(2).any(|w| w == ["-o", "SendEnv=DEMO_SECRET"]));
    assert_eq!(start.env("TF_VAR_proxmox_password"), None);
    let page = started.outputs.iter().find(|o| o.name == "web__portal").unwrap();
    assert_eq!(page.value.as_deref(), Some(format!("http://{VM_IP}:8080/").as_str()));
    assert!(
        run.park(Park::Pause, &RunOptions::new(), &Recorder::new()).await.is_err(),
        "a server run isn't parked"
    );
    run.down(&server, &Recorder::new()).await.unwrap();
}

/// The project reaches the VM as the generated files copy it: the outputs the guest reads (the
/// archive step's `with.outputs`: Compose's files and the configuration), none of Isoloom's
/// others, and no Terraform state.
#[tokio::test(start_paused = true)]
async fn the_vm_gets_the_outputs_it_reads_and_no_terraform_state() {
    let Server {
        folder,
        runner,
        options: server,
        ..
    } = proxmox();
    let project = folder.target_dir().ancestors().nth(3).unwrap();
    for (rel, text) in [
        ("isoloom.yml", "version: 1\n"),
        (".isoloom/docker/compose.yml", "name: web-portal\n"),
        (".isoloom/configure/site.yml", "- hosts: all\n"),
        (".isoloom/vagrant/Vagrantfile", "Vagrant.configure(2)\n"),
        ("infra/terraform.tfstate", "{\"secret\": \"pw\"}\n"),
    ] {
        std::fs::create_dir_all(project.join(rel).parent().unwrap()).unwrap();
        std::fs::write(project.join(rel), text).unwrap();
    }
    let run = runner.open(&folder.manifest, None).unwrap();
    run.up(&server, &Recorder::new()).await.unwrap();
    let archive = std::fs::File::open(runner.store.run_dir(run.name()).unwrap().join("project.tgz")).unwrap();
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let names: Vec<String> = tar
        .entries()
        .unwrap()
        .map(|e| e.unwrap().path().unwrap().to_string_lossy().trim_end_matches('/').to_owned())
        .collect();
    for kept in ["isoloom.yml", ".isoloom/docker/compose.yml", ".isoloom/configure/site.yml", "infra"] {
        assert!(names.iter().any(|n| n == kept), "{kept} goes in: {names:?}");
    }
    for left in [".isoloom/docker-vm/proxmox/main.tf", ".isoloom/vagrant/Vagrantfile", "infra/terraform.tfstate"] {
        assert!(!names.iter().any(|n| n == left), "{left} stays out: {names:?}");
    }
    run.down(&server, &Recorder::new()).await.unwrap();
}

/// The run's SSH jump (a node whose run bridge this host can't route to) carries a console as it
/// carries the guest's own steps.
#[tokio::test(start_paused = true)]
async fn a_console_goes_through_the_runs_ssh_jump() {
    let s = proxmox();
    let run = s.runner.open(&s.folder.manifest, None).unwrap();
    run.up(&s.options.clone().with_ssh_jump("root@pve.lan"), &Recorder::new()).await.unwrap();
    let console = run.console_command("web", None).unwrap();
    assert!(console.args.windows(2).any(|w| w == ["-J", "root@pve.lan"]), "{:?}", console.args);
}

/// A console into a service of the run in the server's VM: `ssh -t` to the VM as the guest's
/// user, then into the service's container.
#[tokio::test(start_paused = true)]
async fn on_proxmox_a_console_into_a_service_goes_over_ssh_into_its_container() {
    let s = proxmox();
    let run = s.runner.open(&s.folder.manifest, None).unwrap();
    run.up(&s.options, &Recorder::new()).await.unwrap();
    let console = run.console_command("web", None).unwrap();
    assert_eq!(console.program, "ssh");
    assert_eq!(console.args[0], "-t");
    assert!(console.args.contains(&format!("isoloom@{VM_IP}")), "{:?}", console.args);
    assert!(
        console.args.last().unwrap().contains("label=com.docker.compose.service=web"),
        "{:?}",
        console.args
    );
    let as_user = run.console_command("web", Some("user")).unwrap();
    assert!(as_user.args.last().unwrap().contains("docker exec -it --user user "), "{:?}", as_user.args);
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

/// On a server too, the VM the run's containers run in is never retired: refused from the
/// manifest's `guest`, before anything runs.
#[tokio::test(start_paused = true)]
async fn on_proxmox_the_vm_the_containers_run_in_is_not_retired() {
    let s = proxmox();
    let run = s.runner.open(&s.folder.manifest, None).unwrap();
    run.up(&s.options, &Recorder::new()).await.unwrap();
    let calls = s.fake.calls().len();
    let err = run.retire(super::HOST, &s.options, &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::Unsupported, "{err}");
    assert!(err.to_string().contains("the VM the run's containers run in"), "{err}");
    assert_eq!(s.fake.calls().len(), calls, "nothing ran: {:?}", &s.fake.lines()[calls..]);
}

#[tokio::test(start_paused = true)]
async fn an_archive_step_without_its_folder_is_a_malformed_manifest() {
    let mut m: serde_json::Value = serde_json::from_str(DOCKER_VM_PROXMOX).unwrap();
    let archive = m["lifecycle"]["up"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|s| s["action"] == "project.archive")
        .unwrap();
    archive["with"].as_object_mut().unwrap().remove("root");
    let Server { folder, runner, options, .. } = proxmox_with(&m.to_string());
    let run = runner.open(&folder.manifest, None).unwrap();
    let err = run.up(&options, &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::ManifestInvalid, "{err}");
    assert_eq!(
        err.to_string(),
        "lifecycle.up.archive.with.root: a `project.archive` needs the folder to archive"
    );
}

/// Regression: a guest host that couldn't be filled (the VM's address output gone) left
/// `${host.<machine>}` unset with no word, and the later reads failed elsewhere.
#[tokio::test(start_paused = true)]
async fn a_guest_host_that_cant_be_filled_is_traced() {
    let Server {
        folder,
        runner,
        options: server,
        ..
    } = proxmox();
    let run = runner.open(&folder.manifest, None).unwrap();
    run.up(&server, &Recorder::new()).await.unwrap();
    let state = runner.store.run_dir(run.name()).unwrap().join("terraform.tfstate");
    std::fs::write(&state, r#"{"outputs":{},"resources":[{"instances":[{}]}]}"#).unwrap();
    let (_status, traced) = crate::testing::traced(run.status()).await;
    assert!(traced.contains("WARN") && traced.contains("the guest's host couldn't be filled"), "{traced}");
    assert!(traced.contains("outputs.ip"), "with the render error: {traced}");
}
