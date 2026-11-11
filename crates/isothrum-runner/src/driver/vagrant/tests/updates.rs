//! Operations on a running VM run: provisioning again, the connection secret at every operation,
//! an update, the console, the relay's hosts.

use super::*;

#[tokio::test]
async fn provisioning_again_runs_on_the_running_machines_with_the_values_given_again() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let not_up = run.provision(None, &opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(not_up.code(), ErrorCode::NotRunning);
    run.up(&opts(), &Recorder::new()).await.unwrap();
    run.provision(Some("web"), &opts(), &Recorder::new()).await.unwrap();
    let one = f.fake.last("vagrant provision web").unwrap();
    assert_eq!(one.env("DEMO_SECRET"), Some(SECRET), "the value given again");
    run.provision(None, &RunOptions::new(), &Recorder::new()).await.unwrap();
    assert_eq!(
        f.fake.last("vagrant provision").unwrap().env("DEMO_SECRET"),
        Some(""),
        "none given: its default"
    );
    let unknown = run.provision(Some("isoloom-controller"), &opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(unknown.code(), ErrorCode::MachineUnknown, "only the run's own machines");
}

/// An ESXi host's password is a connection slot every Vagrant command needs, reads included: a
/// run given it once ([`crate::Running::with_connection`]) delivers it to every operation, status
/// (which takes no options) and an operation given none too; a value given to an operation wins.
#[tokio::test]
async fn a_connection_secret_given_to_the_run_reaches_every_operation() {
    let f = fixture(HostOs::Linux);
    f.engine().plugins.push("vagrant-vmware-esxi".into());
    let mut connection = Secrets::new();
    connection.insert("ESXI_PASSWORD", SecretValue::new("pw"));
    let run = f.runner.open(&f.manifest, None).unwrap().with_connection(connection);
    let mut secrets = Secrets::new();
    secrets.insert("DEMO_SECRET", SecretValue::new(SECRET));
    let opts = on_esxi(secrets);
    run.up(&opts, &Recorder::new()).await.unwrap();
    let after_up = f.fake.calls().len();
    run.status().await.unwrap();
    run.park(Park::Shutdown, &RunOptions::new(), &Recorder::new()).await.unwrap();
    run.resume(&RunOptions::new(), &Recorder::new()).await.unwrap();
    let vagrant: Vec<Call> = f
        .fake
        .calls()
        .into_iter()
        .filter(|c| c.program == "vagrant" && c.args != ["--version"])
        .collect();
    let later: Vec<String> = f.fake.calls()[after_up..].iter().map(Call::line).collect();
    assert!(later.iter().any(|l| l.starts_with("vagrant status")), "{later:?}");
    assert!(later.iter().any(|l| l == "vagrant halt"), "{later:?}");
    for call in &vagrant {
        assert_eq!(call.env("ESXI_PASSWORD"), Some("pw"), "{}", call.line());
    }
    // Given to an operation, its own value is the one delivered.
    let mut other = Secrets::new();
    other.insert("ESXI_PASSWORD", SecretValue::new("other"));
    run.down(&RunOptions::new().with_secrets(other), &Recorder::new()).await.unwrap();
    let destroy = f.fake.last("vagrant destroy").unwrap();
    assert_eq!(destroy.env("ESXI_PASSWORD"), Some("other"));
}

/// Regression: an update given no provider (a run keeps its own) checked the secrets on no
/// provider, so a run recorded on ESXi updated without the host's password ran its steps and
/// failed in the tool, not with `secret_missing`.
#[tokio::test]
async fn an_update_of_an_esxi_run_needs_the_hosts_password_with_no_provider_given() {
    let f = with_esxi_plugin();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&on_esxi(esxi_secrets()), &Recorder::new()).await.unwrap();
    let before = f.vagrant().len();
    let mut secrets = Secrets::new();
    secrets.insert("DEMO_SECRET", SecretValue::new(SECRET));
    let err = run.update(&RunOptions::new().with_secrets(secrets), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::SecretMissing, "{err}");
    assert!(err.to_string().contains("ESXI_PASSWORD"), "{err}");
    assert_eq!(f.vagrant().len(), before, "no step ran: {:?}", &f.vagrant()[before..]);
}

/// An update of a running VM run with a VM added runs its start again (`vagrant up`), which
/// brings the new VM up beside the running ones; the run stays ready.
#[tokio::test]
async fn an_update_brings_a_vm_added_to_the_run_up() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    std::fs::write(&f.manifest, with_vm_added("extra")).unwrap();
    f.engine().added.push("extra".into());
    let regenerated = f.runner.open(&f.manifest, None).unwrap();
    let ups = || f.vagrant().iter().filter(|l| l.starts_with("vagrant up --provider")).count();
    let before = ups();
    let updated = regenerated.update(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(updated.record.state, RunState::Ready);
    assert_eq!(ups(), before + 1, "{:?}", f.vagrant());
    let status = regenerated.status().await.unwrap();
    let extra = status.machines.iter().find(|m| m.name == "extra").unwrap();
    assert_eq!(extra.state, crate::MachineState::Running);
}

/// A console as an account into a Windows VM is refused: its way in has no `sudo` to switch with.
/// Its console as Vagrant logs in stays.
#[tokio::test]
async fn a_console_as_an_account_into_a_windows_machine_is_refused() {
    let mut m: serde_json::Value = serde_json::from_str(WEB_VM).unwrap();
    m["machines"][0]["windows"] = serde_json::json!(true);
    let f = fixture_from(crate::runner::this_host(), &m.to_string());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.console_command("web", Some("user")).unwrap_err();
    assert_eq!(err.code(), ErrorCode::Unsupported, "{err}");
    assert!(err.to_string().contains("which runs Windows"), "{err}");
    assert_eq!(run.console_command("web", None).unwrap().args, ["ssh", "web"]);
}

/// A machine whose manifest doesn't say how it runs (no native name) has no console and isn't
/// retired: refused, rather than a guess at its define.
#[tokio::test]
async fn a_machine_with_no_native_name_has_no_console() {
    let mut m: serde_json::Value = serde_json::from_str(WEB_VM).unwrap();
    m["machines"][0].as_object_mut().unwrap().remove("native");
    let f = fixture_from(crate::runner::this_host(), &m.to_string());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.console_command("web", None).unwrap_err();
    assert_eq!(err.code(), ErrorCode::Unsupported, "{err}");
    assert!(err.to_string().contains("doesn't say how it runs"), "{err}");
    // Nor a retire: nothing would be destroyed, so it isn't reported done.
    let retired = run.retire("web", &opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(retired.code(), ErrorCode::Unsupported, "{retired}");
    assert!(f.vagrant().iter().all(|l| !l.contains("destroy")), "{:?}", f.vagrant());
}

/// Both VMs' boot settings changed (a host folder each, say): the update restarts the one that
/// runs, the web VM, and leaves the controller, off once the run is built, to boot with them
/// when it next boots.
#[tokio::test]
async fn an_update_restarts_only_the_running_vm_whose_boot_settings_changed() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!((f.state("web").as_str(), f.state(CONTROLLER).as_str()), ("running", "poweroff"));
    let changed = with_boot_digest(&with_boot_digest(WEB_VM, "web", Some("sha256:web-2")), CONTROLLER, Some("sha256:controller-2"));
    std::fs::write(&f.manifest, changed).unwrap();
    let updated = f.runner.open(&f.manifest, None).unwrap().update(&opts(), &Recorder::new()).await.unwrap();
    let reloads: Vec<String> = f.vagrant().into_iter().filter(|l| l.starts_with("vagrant reload")).collect();
    assert_eq!(reloads, ["vagrant reload web --no-provision"]);
    let reload = f.fake.last("vagrant reload").unwrap();
    assert!(reload.env("VAGRANT_DOTFILE_PATH").is_some_and(|p| !p.is_empty()), "in the run's Vagrant state");
    assert_eq!(updated.record.vm_boot_digests.get("web").map(String::as_str), Some("sha256:web-2"));
    assert_eq!(updated.record.vm_boot_digests.get(CONTROLLER).map(String::as_str), Some("sha256:controller-2"));
}

/// Vagrant on an ESXi host from macOS, relayed: the hosts this computer sends around its proxy
/// (as the runner read them) are kept beside the relays' loopback.
#[tokio::test]
async fn a_relayed_run_keeps_the_hosts_this_computer_sends_around_its_proxy() {
    let mut f = fixture(HostOs::Macos);
    f.runner.config.host_env = crate::host_env::HostEnv::of([("no_proxy", ".corp.example")]);
    f.engine().plugins.push("vagrant-vmware-esxi".into());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let opts = on_esxi(esxi_secrets());
    run.up(&opts, &Recorder::new()).await.unwrap();
    let up = f.fake.last("vagrant up").unwrap();
    assert_eq!(up.env("no_proxy"), Some(".corp.example,localhost,127.0.0.1"));
}
