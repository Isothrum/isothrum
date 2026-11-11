//! Scripted runs of Isoloom's `proxmox` fixture (solo-web): a fake Terraform that writes and
//! removes the run's state, a fake `ssh-keygen`, and a fake `ssh` answering for the machines'
//! ready files and check runners. No real tool ever runs; under Tokio's paused clock, a wait passes
//! at once (a ready file that never says `ready` fails at its deadline, never hangs).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use isoloom_manifest::HostOs;

use crate::process::fake::{Call, FakeProcesses, Reply};
use crate::secrets::{SecretValue, Secrets};
use crate::testing::{RunFolder, SOLO_WEB_PROXMOX, no_ports, processes, runner_config};
use crate::{ErrorCode, Recorder, RunOptions, RunState, Runner};

/// The router's uplink address the fake apply reports.
const ROUTER: &str = "10.10.10.7";

/// What the fake `ssh` answers with.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Guest {
    /// The ready file says `ready`; the checks pass.
    Ready,
    /// The ready file says a step failed, its log's tail after it.
    Failed,
    /// Nothing answers (ssh exits 255).
    Unreachable,
}

/// A test's runner and its fakes.
struct Server {
    folder: RunFolder,
    manifest: PathBuf,
    fake: Arc<FakeProcesses>,
    guest: Arc<Mutex<Guest>>,
    runner: Runner,
}

fn server(host: HostOs) -> Server {
    server_of(host, SOLO_WEB_PROXMOX)
}

/// A test's runner on `manifest`.
fn server_of(host: HostOs, manifest: &str) -> Server {
    scripted(host, manifest, |_| None)
}

/// A test's runner on `manifest`, `script` answering the calls it knows, [`answer`] the others.
fn scripted(host: HostOs, manifest: &str, script: impl Fn(&Call) -> Option<Reply> + Send + Sync + 'static) -> Server {
    let folder = RunFolder::new("proxmox", manifest);
    std::fs::write(folder.target_dir().join("main.tf"), "").unwrap();
    let guest = Arc::new(Mutex::new(Guest::Ready));
    let fake = Arc::new(FakeProcesses::new({
        let (guest, home) = (Arc::clone(&guest), folder.home.path().to_path_buf());
        move |call| script(call).unwrap_or_else(|| answer(&home, &guest, call))
    }));
    let runner = Runner::new(runner_config(folder.home.path(), processes(&fake), no_ports(), host)).unwrap();
    Server {
        manifest: folder.manifest.clone(),
        folder,
        fake,
        guest,
        runner,
    }
}

/// The run's state folder the fake Terraform writes in: the `-backend-config` path's folder.
fn state_dir(call: &Call) -> Option<PathBuf> {
    call.env("TF_DATA_DIR").map(|d| Path::new(d).parent().unwrap().to_path_buf())
}

fn answer(home: &Path, guest: &Mutex<Guest>, call: &Call) -> Reply {
    let line = call.line();
    let guest = guest.lock().unwrap_or_else(PoisonError::into_inner).clone();
    if call.program == "ssh-keygen" {
        return crate::testing::keygen(home);
    }
    if line.contains(" apply ") || line.ends_with(" apply -auto-approve -input=false -no-color") {
        let state = format!(
            r#"{{"outputs":{{"ip":{{"value":"{ROUTER}"}},"ssh_user":{{"value":"isoloom"}},"ready_file":{{"value":"/var/lib/isoloom/ready"}},"published":{{"value":{{"web/web":"{ROUTER}:8080"}}}}}},"resources":[{{"instances":[{{}}]}}]}}"#
        );
        std::fs::write(state_dir(call).unwrap().join("terraform.tfstate"), state).unwrap();
        return Reply::ok("Apply complete! Resources: 6 added, 0 changed, 0 destroyed.");
    }
    if line.contains(" destroy ") {
        std::fs::write(state_dir(call).unwrap().join("terraform.tfstate"), r#"{"outputs":{},"resources":[]}"#).unwrap();
        return Reply::ok("Destroy complete! Resources: 6 destroyed.");
    }
    if call.program == "ssh" && call.args == ["-V"] {
        return Reply::ok("OpenSSH_9.9p2");
    }
    if call.program == "ssh" {
        return match (guest, call.stdin.is_some()) {
            (Guest::Unreachable, _) => Reply::fail(255, "ssh: connect to host 10.10.10.7 port 22: Operation timed out"),
            (Guest::Failed, false) => Reply::ok("failed: provision/web.sh\n--isoloom-log--\nE: Unable to locate package nginx"),
            (_, false) => Reply::ok("ready\n--isoloom-log--\n"),
            (_, true) => Reply::ok("isoloom-check: PASS checks/web-answers.sh\nisoloom-check: END 1 passed, 0 failed"),
        };
    }
    Reply::ok("")
}

/// A start's options: the server's params and password.
fn opts(node: &str) -> RunOptions {
    let mut secrets = Secrets::new();
    secrets.insert("proxmox_password", SecretValue::new("pw-s3cret"));
    RunOptions::new()
        .with_param("proxmox_endpoint", "https://172.16.1.10:8006/")
        .with_param("node", node)
        .with_param("uplink_bridge", "vmbr1")
        .with_secrets(secrets)
        .with_host_label("pve-run")
}

impl Server {
    fn set(&self, g: Guest) {
        *self.guest.lock().unwrap_or_else(PoisonError::into_inner) = g;
    }
}

#[tokio::test(start_paused = true)]
async fn a_proxmox_run_applies_with_the_runners_state_slot_and_key_then_reads_the_ready_file() {
    let s = server(HostOs::Linux);
    let run = s.runner.open(&s.manifest, None).unwrap();
    let started = run.up(&opts("pve-run"), &Recorder::new()).await.unwrap();
    let state = s.runner.store.run_dir(run.name()).unwrap();
    let init = s.fake.last("terraform init").unwrap();
    assert!(
        init.args.contains(&format!("-backend-config=path={}/terraform.tfstate", state.display())),
        "{init:?}"
    );
    let apply = s.fake.last("terraform apply").unwrap();
    assert_eq!(apply.env("TF_VAR_slot"), Some("1"));
    assert_eq!(apply.env("TF_VAR_ssh_public_key"), Some("ssh-ed25519 AAAAfake isothrum"));
    assert_eq!(apply.env("TF_VAR_node"), Some("pve-run"));
    assert_eq!(apply.env("TF_VAR_proxmox_password"), Some("pw-s3cret"));
    // The manifest writes `${run.state_dir}/.terraform`: on Windows a `/` after the state folder's
    // `\`s, the same folder.
    assert_eq!(apply.env("TF_DATA_DIR").map(Path::new), Some(state.join(".terraform").as_path()));
    assert!(!apply.args.iter().any(|a| a.contains("s3cret")), "never on a command line");
    // The ready file of `web`, read through the router.
    let read = s.fake.last("head -n 1").unwrap();
    assert!(read.args.windows(2).any(|w| w == ["-J", &format!("isoloom@{ROUTER}")]), "{read:?}");
    assert!(read.args.contains(&"isoloom@10.80.0.10".to_owned()), "{read:?}");
    let entry = started.outputs.iter().find(|o| o.name == "web__web").unwrap();
    assert_eq!(entry.value.as_deref(), Some("http://10.10.10.7:8080/"));
    assert_eq!(started.record.slot, Some(1));
    assert_eq!(started.record.state, RunState::Ready);
}

#[tokio::test(start_paused = true)]
async fn a_second_run_on_the_same_server_takes_the_next_slot() {
    // Regression: the modules' `slot` was never set, so a second run collided with the first.
    let s = server(HostOs::Linux);
    let first = s.runner.open(&s.manifest, Some(crate::RunName::new("first").unwrap())).unwrap();
    first.up(&opts("pve-run"), &Recorder::new()).await.unwrap();
    let second = s.runner.open(&s.manifest, Some(crate::RunName::new("second").unwrap())).unwrap();
    let started = second.up(&opts("pve-run"), &Recorder::new()).await.unwrap();
    assert_eq!(started.record.slot, Some(2));
    assert_eq!(s.fake.last("terraform apply").unwrap().env("TF_VAR_slot"), Some("2"));
}

#[tokio::test(start_paused = true)]
async fn a_start_picks_its_slot_only_while_no_other_process_picks_one() {
    // Regression: two starts in one home (two processes) both read the records before either
    // recorded its slot, and took the same one.
    let s = server(HostOs::Linux);
    let other = std::fs::File::create(s.folder.home.path().join("slots.lock")).unwrap();
    other.lock().unwrap();
    let run = s.runner.open(&s.manifest, Some(crate::RunName::new("first").unwrap())).unwrap();
    let err = run.up(&opts("pve-run"), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::Locked, "{err}");
    assert!(s.fake.last("terraform apply").is_none(), "nothing was created");
    drop(other);
    assert_eq!(run.up(&opts("pve-run"), &Recorder::new()).await.unwrap().record.slot, Some(1));
}

#[tokio::test(start_paused = true)]
async fn terraform_uses_the_users_plugin_cache_else_the_runners() {
    let s = server(HostOs::Linux);
    let run = s.runner.open(&s.manifest, None).unwrap();
    run.up(&opts("pve-run"), &Recorder::new()).await.unwrap();
    let ours = s.folder.home.path().join("terraform-plugins");
    assert_eq!(s.fake.last("terraform").unwrap().env("TF_PLUGIN_CACHE_DIR"), Some(ours.to_str().unwrap()));
    assert!(ours.is_dir());

    // Regression: a `TF_PLUGIN_CACHE_DIR` of the user's own was replaced by the runner's.
    let theirs = s.folder.home.path().join("their-cache");
    let mut config = runner_config(s.folder.home.path(), processes(&s.fake), no_ports(), HostOs::Linux);
    config.tf_plugin_cache = Some(theirs.clone());
    std::fs::remove_dir(&ours).unwrap();
    let runner = Runner::new(config).unwrap();
    runner.open(&s.manifest, None).unwrap().down(&opts("pve-run"), &Recorder::new()).await.unwrap();
    assert_eq!(s.fake.last("terraform").unwrap().env("TF_PLUGIN_CACHE_DIR"), Some(theirs.to_str().unwrap()));
    assert!(!ours.exists() && !theirs.exists(), "the user's cache is theirs: used as it is");
}

#[tokio::test(start_paused = true)]
async fn a_machine_that_never_answers_over_ssh_fails_the_start() {
    // A wait that returned OK when SSH never answered within 10 minutes.
    let s = server(HostOs::Linux);
    s.set(Guest::Unreachable);
    let run = s.runner.open(&s.manifest, None).unwrap();
    let events = Recorder::new();
    let err = run.up(&opts("pve-run"), &events).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::NotReady);
    // Why each read failed is said, not swallowed: once as it waits, and in the error.
    let why = "ssh: connect to host 10.10.10.7 port 22: Operation timed out (exited with code 255)";
    assert!(err.to_string().contains(&format!("didn't answer over SSH within 600s: {why}")), "{err}");
    let said: Vec<String> = events
        .events()
        .into_iter()
        .filter_map(|e| match e.kind {
            crate::EventKind::Ready { detail, .. } if detail.starts_with("no answer over SSH yet") => Some(detail),
            _ => None,
        })
        .collect();
    assert_eq!(said, [format!("no answer over SSH yet: {why}")], "once, while the reason stays the same");
    assert_eq!(s.runner.store.record(run.name()).unwrap().unwrap().state, RunState::Failed);
}

#[tokio::test(start_paused = true)]
async fn a_failed_set_up_step_fails_the_start_with_its_log() {
    let s = server(HostOs::Linux);
    s.set(Guest::Failed);
    let run = s.runner.open(&s.manifest, None).unwrap();
    let err = run.up(&opts("pve-run"), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::NotReady);
    let text = err.to_string();
    assert!(
        text.contains("failed: provision/web.sh") && text.contains("Unable to locate package nginx"),
        "{text}"
    );
    // Found on the real server: the VMs exist and run, but the run isn't ready.
    assert_eq!(run.status().await.unwrap().state, RunState::Failed);
    // Starting it again removes what the failed start left, then starts afresh.
    s.set(Guest::Ready);
    run.up(&opts("pve-run"), &Recorder::new()).await.unwrap();
    let lines = s.fake.lines();
    let destroy = lines.iter().position(|l| l.contains("terraform destroy")).expect("the leftovers destroyed");
    assert!(lines.iter().rposition(|l| l.contains("terraform apply")).unwrap() > destroy);
    assert_eq!(run.status().await.unwrap().state, RunState::Ready);
}

#[tokio::test(start_paused = true)]
async fn a_destroy_replays_the_applied_params_and_slot_not_the_current_profile() {
    // A destroy used the profile's current settings, so it went to another node (or region).
    let s = server(HostOs::Linux);
    let run = s.runner.open(&s.manifest, None).unwrap();
    run.up(&opts("pve-run"), &Recorder::new()).await.unwrap();
    run.down(&opts("another-node"), &Recorder::new()).await.unwrap();
    let destroy = s.fake.last("terraform destroy").unwrap();
    assert_eq!(destroy.env("TF_VAR_node"), Some("pve-run"));
    assert_eq!(destroy.env("TF_VAR_slot"), Some("1"));
    assert_eq!(destroy.env("TF_VAR_proxmox_password"), Some("pw-s3cret"), "the connection is the current one");
    let state = s.runner.store.run_dir(run.name()).unwrap();
    assert!(!state.join("terraform.tfstate").exists(), "the state goes with a successful down");
    assert_eq!(run.status().await.unwrap().state, RunState::Absent);
}

#[tokio::test(start_paused = true)]
async fn the_runs_jump_goes_before_the_router() {
    // A run bridge internal to the Proxmox node: SSH goes through the node first.
    let s = server(HostOs::Linux);
    let run = s.runner.open(&s.manifest, None).unwrap();
    run.up(&opts("pve-run").with_ssh_jump("root@172.16.1.10"), &Recorder::new()).await.unwrap();
    let read = s.fake.last("head -n 1").unwrap();
    assert!(
        read.args.windows(2).any(|w| w == ["-J", &format!("root@172.16.1.10,isoloom@{ROUTER}")]),
        "{read:?}"
    );
    // A later operation goes the way the run started, whatever jump its options give.
    let report = run.check(&RunOptions::new().with_ssh_jump("root@elsewhere"), &Recorder::new()).await.unwrap();
    assert!(report.ok, "{report:?}");
    let check = s.fake.last("sh -s").unwrap();
    assert!(check.stdin.as_ref().is_some_and(|p| p.ends_with("checks/isoloom-networks.sh")), "{check:?}");
    assert!(
        check.args.windows(2).any(|w| w[0] == "-J" && w[1].starts_with("root@172.16.1.10,")),
        "recorded at up: {check:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn on_macos_terraform_runs_in_its_pinned_image() {
    let s = server(HostOs::Macos);
    let run = s.runner.open(&s.manifest, None).unwrap();
    run.up(&opts("pve-run"), &Recorder::new()).await.unwrap();
    assert!(s.fake.last("docker info").is_some(), "the engine is probed, not the host binary");
    assert!(s.fake.last("terraform version").is_none());
    let apply = s.fake.last(" apply ").unwrap();
    assert_eq!(apply.program, "docker");
    assert!(apply.args.iter().any(|a| a.starts_with("hashicorp/terraform:1.16.7@sha256:")), "{apply:?}");
    assert!(apply.args.windows(2).any(|w| w == ["-e", "TF_VAR_proxmox_password"]));
    assert!(!apply.args.iter().any(|a| a.contains("s3cret")));
}

/// The fixture, its start copying the project into the router over SSH right after the apply
/// that made it (its address an output of the state): archived by the runner, piped from the
/// run's state folder, a secret carried into the guest.
fn copying() -> String {
    let mut m: serde_json::Value = serde_json::from_str(SOLO_WEB_PROXMOX).unwrap();
    let up = m["lifecycle"]["up"].as_array_mut().unwrap();
    up.push(serde_json::json!({"id": "archive", "action": "project.archive", "with": {"root": "../.."}}));
    up.push(serde_json::json!({
        "id": "copy", "action": "ssh.run", "stdin": "${run.state_dir}/project.tgz",
        "with": {"host": "${outputs.ip}", "user": "isoloom", "command": "sudo tar -xzf - -C /opt/isoloom"}
    }));
    m["secrets"].as_array_mut().unwrap().push(serde_json::json!({
        "name": "DEMO_SECRET", "kind": "input", "required": false, "sensitive": true, "default": "",
        "deliver": [{"via": "env", "var": "DEMO_SECRET", "steps": ["copy"]}]
    }));
    m.to_string()
}

#[tokio::test(start_paused = true)]
async fn a_guest_step_gets_the_projects_archive_on_its_input_and_only_its_own_variables() {
    let s = server_of(HostOs::Linux, &copying());
    std::fs::write(s.manifest.parent().unwrap().join("../../isoloom.yml"), "version: 1\n").unwrap();
    let run = s.runner.open(&s.manifest, None).unwrap();
    let mut o = opts("pve-run");
    o.secrets.insert("DEMO_SECRET", SecretValue::new("token-x"));
    run.up(&o, &Recorder::new()).await.unwrap();
    let state = s.runner.store.run_dir(run.name()).unwrap();
    let copy = s.fake.calls().into_iter().find(|c| c.line().contains("tar -xzf")).unwrap();
    assert_eq!(copy.stdin.as_deref(), Some(state.join("project.tgz").as_path()));
    // The router's address was read from the state the apply wrote, before the step ran.
    assert!(copy.args.contains(&format!("isoloom@{ROUTER}")), "{copy:?}");
    assert!(state.join("project.tgz").is_file(), "the archive is the run's");
    // The secret travels in the SSH session, named once and never on the command line; the server's
    // password, a Terraform variable, stays on this computer.
    assert!(copy.args.windows(2).any(|w| w == ["-o", "SendEnv=DEMO_SECRET"]), "{copy:?}");
    assert_eq!(copy.env("DEMO_SECRET"), Some("token-x"));
    assert!(!copy.line().contains("token-x"));
    assert_eq!(copy.env("TF_VAR_proxmox_password"), None);
    assert!(!copy.args.iter().any(|a| a.contains("TF_VAR")), "{copy:?}");
    run.down(&opts("pve-run"), &Recorder::new()).await.unwrap();
    assert!(!state.join("project.tgz").exists(), "down forgets the archive");
}

#[tokio::test(start_paused = true)]
async fn a_deployment_taken_over_is_destroyed_where_it_was_applied() {
    // An embedder's live deployments at the switch: their state and params become a run's, so `down`
    // (and the reaper) destroy them through the runner, on the node they were applied on.
    let s = server(HostOs::Linux);
    let old = s.folder.home.path().join("deployments").join("solo-web").join("proxmox");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(
        old.join("terraform.tfstate"),
        r#"{"outputs":{"ip":{"value":"10.10.10.7"}},"resources":[{"instances":[{}]}]}"#,
    )
    .unwrap();
    let name = crate::RunName::new("app-solo-web").unwrap();
    let params = [
        ("node", "pve-run"),
        ("uplink_bridge", "vmbr1"),
        ("proxmox_endpoint", "https://172.16.1.10:8006/"),
    ];
    let params = params.map(|(k, v)| (k.to_owned(), v.to_owned()));
    let adoption = crate::Adoption::new(name.clone(), &old)
        .with_params(params)
        .with_host_label("pve-run")
        .with_slot(1);
    s.runner.adopt(&s.manifest, adoption).await.unwrap();
    let run = s.runner.open(&s.manifest, Some(name)).unwrap();
    assert_eq!(run.status().await.unwrap().state, RunState::Ready);
    run.down(&opts("another-node"), &Recorder::new()).await.unwrap();
    let destroy = s.fake.last("terraform destroy").unwrap();
    assert_eq!(destroy.env("TF_VAR_node"), Some("pve-run"), "the applied node, not the profile's current one");
    assert_eq!(destroy.env("TF_VAR_slot"), Some("1"), "the slot it was applied in");
    assert_eq!(destroy.env("TF_VAR_proxmox_password"), Some("pw-s3cret"));
    // Regression: a param the deployment wasn't given took no value, not its default.
    assert_eq!(destroy.env("TF_VAR_datastore"), Some("local-lvm"), "the manifest's default");
    assert_eq!(run.status().await.unwrap().state, RunState::Absent);
}

/// A VM run on a server or a cloud that declares no access machine has none to attach: nothing
/// is run trying.
#[tokio::test(start_paused = true)]
async fn a_vm_lab_on_a_server_gets_no_attack_box() {
    let s = server(HostOs::Linux);
    let run = s.runner.open(&s.manifest, None).unwrap();
    run.up(&opts("pve-run"), &Recorder::new()).await.unwrap();
    crate::testing::refuses_a_box_it_has_none_of(&run, &s.fake).await;
}

/// Retiring a VM the module makes runs nothing: the update's apply removes what the module no
/// longer makes. Its result still names the machine.
#[tokio::test(start_paused = true)]
async fn retiring_a_machine_the_module_makes_is_left_to_the_apply() {
    let s = server(HostOs::Linux);
    let run = s.runner.open(&s.manifest, None).unwrap();
    run.up(&opts("pve-run"), &Recorder::new()).await.unwrap();
    let calls = s.fake.calls().len();
    let events = Recorder::new();
    run.retire("web", &opts("pve-run"), &events).await.unwrap();
    let ran: Vec<String> = s.fake.lines()[calls..]
        .iter()
        .filter(|l| l.contains("apply") || l.contains("destroy"))
        .cloned()
        .collect();
    assert!(ran.is_empty(), "{ran:?}");
    let last = events.events().last().cloned().unwrap();
    assert!(
        matches!(&last.kind, crate::EventKind::Result { ok: true, data } if data["machine"] == "web"),
        "{last:?}"
    );
}

/// A Compose service on a Terraform run with no VM for it to run in has nothing to take out and
/// no way in: refused, rather than reported done.
#[tokio::test(start_paused = true)]
async fn a_compose_service_with_no_vm_is_neither_retired_nor_entered() {
    let mut m: serde_json::Value = serde_json::from_str(SOLO_WEB_PROXMOX).unwrap();
    m["machines"][0]["native"] = serde_json::json!({ "service": "web" });
    let s = server_of(HostOs::Linux, &m.to_string());
    let run = s.runner.open(&s.manifest, None).unwrap();
    run.up(&opts("pve-run"), &Recorder::new()).await.unwrap();
    let err = run.retire("web", &opts("pve-run"), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Unsupported, "{err}");
    assert!(err.to_string().contains("no VM to run in"), "{err}");
    let console = run.console_command("web", None).unwrap_err();
    assert_eq!(console.code(), ErrorCode::Unsupported, "{console}");
}

#[tokio::test(start_paused = true)]
async fn on_macos_terraform_in_its_image_reaches_the_network_as_this_host_does() {
    let mut s = server(HostOs::Macos);
    s.runner.config.host_env = crate::host_env::HostEnv::of([("HTTPS_PROXY", "http://proxy.corp:3128"), ("API_TOKEN", "x")]);
    let run = s.runner.open(&s.manifest, None).unwrap();
    run.up(&opts("pve-run"), &Recorder::new()).await.unwrap();
    let apply = s.fake.last(" apply ").unwrap();
    assert!(
        apply.args.windows(2).any(|w| w == ["-e", "HTTPS_PROXY"]),
        "the runner's proxy, by name: {apply:?}"
    );
    assert!(!apply.args.iter().any(|a| a.contains("API_TOKEN") || a.contains("proxy.corp")), "{apply:?}");
}

#[tokio::test(start_paused = true)]
async fn ssh_runs_in_the_state_folder_so_a_home_with_a_space_reaches_every_jump() {
    // Regression: `-F <home>/runs/<run>/ssh_config` went whole to ssh, but `-J` hands it to each
    // jump's ssh through a shell, unquoted: under macOS's `~/Library/Application Support` every
    // jump failed ("Can't open user config file …/Library/Application"), and a ready file read
    // through the router never answered.
    let s = server(HostOs::Linux);
    let home = s.folder.home.path().join("Application Support").join("isothrum");
    let key = crate::ssh::key_path(&home);
    std::fs::create_dir_all(key.parent().unwrap()).unwrap();
    std::fs::write(&key, "private").unwrap();
    std::fs::write(key.with_extension("pub"), "ssh-ed25519 AAAAfake isothrum\n").unwrap();
    let runner = Runner::new(runner_config(&home, processes(&s.fake), no_ports(), HostOs::Linux)).unwrap();
    let run = runner.open(&s.manifest, None).unwrap();
    run.up(&opts("pve-run"), &Recorder::new()).await.unwrap();
    let state = runner.store.run_dir(run.name()).unwrap();
    let read = s.fake.last("head -n 1").unwrap();
    assert!(read.args.windows(2).any(|w| w == ["-J", &format!("isoloom@{ROUTER}")]), "{read:?}");
    assert_eq!(read.args[..2], ["-F", "ssh_config"]);
    assert_eq!(read.cwd.as_deref(), Some(state.as_path()));
    assert!(state.join("ssh_config").is_file());
    assert!(
        !read.args.iter().any(|a| a.contains("Application Support")),
        "no path with a space on the line: {read:?}"
    );
}

/// A run whose Terraform module has no `.tf` file is the generated files' mistake: refused before
/// anything runs, as `manifest_invalid` saying to generate them again.
#[tokio::test(start_paused = true)]
async fn a_module_with_no_terraform_file_is_manifest_invalid() {
    let s = server(HostOs::Linux);
    std::fs::remove_file(s.folder.target_dir().join("main.tf")).unwrap();
    let run = s.runner.open(&s.manifest, None).unwrap();
    let err = run.up(&opts("pve-run"), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::ManifestInvalid, "{err}");
    assert!(err.to_string().contains("it has no `.tf` file: generate the run's files again"), "{err}");
    assert!(!s.fake.lines().iter().any(|l| l.starts_with("terraform")), "{:?}", s.fake.lines());
}

mod console;
mod gone;
mod params;
