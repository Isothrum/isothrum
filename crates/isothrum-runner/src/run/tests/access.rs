//! Hot-adding a lab's own access machine into a running run, and taking it back out, through the
//! fake engines: the Compose `attacker` service in its `access` profile, and the Vagrant
//! `attacker` define. A lab that declares none has nothing to attach. No real tool ever runs.

use super::*;
use crate::ErrorCode;
use crate::testing::{FLAG_PORTAL_ACCESS, FLAG_VM_ACCESS};

#[tokio::test]
async fn attach_brings_the_access_service_up_in_its_profile_with_the_picked_image() {
    let f = fixture_with(FLAG_PORTAL_ACCESS);
    f.set(Containers::Up); // the lab is already up; the access machine was left out of it
    let run = f.runner.open(&f.manifest, None).unwrap();

    let events = Recorder::new();
    let report = run.attach(None, Some("lab"), Some("myimage"), &events).await.unwrap();

    assert_eq!((report.machine.as_str(), report.native.as_str()), ("attacker", "attacker"));
    assert_eq!(report.network.as_deref(), Some("lab"));
    assert_eq!(report.address.as_deref(), Some("10.30.0.99"), "its static address, for the launcher to show");
    assert_eq!(report.image.as_deref(), Some("myimage"));

    let up = f.fake.last("--profile access up").expect("the access profile is brought up");
    assert!(up.line().contains("--no-deps attacker"), "{}", up.line());
    assert_eq!(up.env(isoloom_manifest::access::IMAGE_ENV), Some("myimage"), "the picked image is delivered");
    assert!(matches!(kinds(&events).last(), Some(EventKind::Result { ok: true, .. })));
}

/// The lab's Docker, its config putting a routes service in the access service's namespace.
fn routed_answer(engine: &Mutex<Engine>, call: &Call) -> Reply {
    if call.line().contains("--profile access config --format json") {
        return Reply::ok(r#"{"services":{"attacker":{},"attacker-routes":{"network_mode":"service:attacker"},"web":{}}}"#);
    }
    answer(engine, call)
}

/// A service in the access machine's namespace (its routes through the lab's router) is part of
/// it: up with it, waited for, and removed with it.
#[tokio::test]
async fn the_access_machines_routes_come_and_go_with_it() {
    let f = fixture_answering(FLAG_PORTAL_ACCESS, routed_answer);
    let up = attach_plain(&f).await;
    assert!(up.line().ends_with("up -d --wait --no-deps attacker attacker-routes"), "{}", up.line());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.detach(None, &Recorder::new()).await.unwrap();
    let rm = f.fake.last("--profile access rm").unwrap();
    assert!(rm.line().ends_with("rm -sf attacker attacker-routes"), "{}", rm.line());
}

/// The lab up without its access machine, then the machine attached with no image: its `up`
/// call.
async fn attach_plain(f: &Fixture) -> Call {
    f.set(Containers::Up);
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.attach(None, None, None, &Recorder::new()).await.unwrap();
    f.fake.last("--profile access up").unwrap()
}

#[tokio::test]
async fn attach_without_an_image_lets_the_services_default_stand() {
    let f = fixture_with(FLAG_PORTAL_ACCESS);
    let up = attach_plain(&f).await;
    assert_eq!(
        up.env(isoloom_manifest::access::IMAGE_ENV),
        None,
        "no override: the service's baked default stands"
    );
}

#[tokio::test]
async fn detach_removes_just_the_access_service() {
    let f = fixture_with(FLAG_PORTAL_ACCESS);
    f.set(Containers::Up);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.detach(None, &events).await.unwrap();
    let rm = f.fake.last("--profile access rm").expect("the access service is removed");
    assert!(rm.line().contains("rm -sf attacker"), "{}", rm.line());
    assert!(matches!(kinds(&events).last(), Some(EventKind::Result { ok: true, .. })));
}

#[tokio::test]
async fn status_has_no_access_machine_on_a_lab_that_declares_none() {
    // The runner stands no box up of its own: nothing but the lab's machines, and no probe.
    let f = fixture();
    f.set(Containers::Up);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let status = run.status().await.unwrap();
    assert!(status.machines.iter().all(|m| m.role != isoloom_manifest::Role::Access));
}

#[tokio::test]
async fn a_lab_that_declares_no_access_machine_has_none_to_attach() {
    // flag-portal declares no access machine: attach, detach and the shell are refused, and
    // nothing is started.
    let f = fixture();
    f.set(Containers::Up);
    let run = f.runner.open(&f.manifest, None).unwrap();
    crate::testing::refuses_a_box_it_has_none_of(&run, &f.fake).await;
}

#[tokio::test]
async fn attach_refuses_an_unknown_named_machine() {
    let f = fixture_with(FLAG_PORTAL_ACCESS);
    f.set(Containers::Up);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.attach(Some("nope"), None, None, &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::MachineUnknown);
}

#[tokio::test]
async fn attach_refuses_a_lab_that_is_not_running() {
    let f = fixture_with(FLAG_PORTAL_ACCESS);
    // containers left at None: nothing of the run is up
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.attach(None, None, None, &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::NotRunning);
}

/// A runner over the vagrant access fixture (`FLAG_VM_ACCESS`), its Vagrant answered by `answer`.
fn vagrant_access(answer: impl Fn(&Call) -> Reply + Send + Sync + 'static) -> (LabFolder, Arc<FakeProcesses>, Runner) {
    let folder = LabFolder::new("vagrant", FLAG_VM_ACCESS);
    let fake = Arc::new(FakeProcesses::new(answer));
    let ports: Arc<dyn crate::HostPorts> = Arc::new(FakePorts::new(std::iter::empty::<u16>()));
    let runner = Runner::new(runner_config(folder.home.path(), processes(&fake), ports, crate::this_host())).unwrap();
    (folder, fake, runner)
}

#[tokio::test]
async fn vagrant_attach_boots_the_access_vm_with_the_picked_box() {
    // A fake Vagrant that reports the lab's web VM up, and succeeds at everything else.
    let (folder, fake, runner) = vagrant_access(|call| {
        let line = call.line();
        if line.contains("plugin list") {
            return Reply::ok("");
        }
        if line.contains("status") {
            return Reply::ok("1,web,state,running\n1,isoloom-controller,state,running\n1,attacker,state,not_created");
        }
        Reply::ok("")
    });
    let run = runner.open(&folder.manifest, None).unwrap();

    let report = run.attach(None, None, Some("mybox"), &Recorder::new()).await.unwrap();
    assert_eq!((report.machine.as_str(), report.native.as_str()), ("attacker", "attacker"));

    let up = fake.last("up attacker").expect("the access VM is booted");
    assert_eq!(up.env(isoloom_manifest::access::BOX_ENV), Some("mybox"), "the picked box is delivered");

    run.detach(None, &Recorder::new()).await.unwrap();
    assert!(fake.last("destroy -f attacker").is_some(), "the access VM is destroyed");
}

/// The shell is the access machine's console: its service by name, bash where its image has it,
/// else sh.
#[tokio::test]
async fn shell_command_opens_the_compose_service_by_name() {
    let f = fixture_with(FLAG_PORTAL_ACCESS);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let cmd = run.shell_command(None).unwrap();
    assert_eq!(cmd.program, "docker");
    let line = cmd.args.join(" ");
    assert!(line.starts_with("compose --project-name"), "{line}");
    assert!(line.contains(" exec attacker sh -c "), "addresses the service, no container guessing: {line}");
    assert!(line.ends_with(crate::driver::console::CONTAINER_SHELL), "{line}");
    assert_eq!(cmd.args, run.console_command("attacker").unwrap().args, "the access machine's console");
    assert!(cmd.cwd.is_some(), "runs in the run's folder");
}

#[tokio::test]
async fn shell_command_opens_the_vagrant_define_by_name() {
    let (folder, _fake, runner) = vagrant_access(|_| Reply::ok(""));
    let run = runner.open(&folder.manifest, None).unwrap();
    let cmd = run.shell_command(None).unwrap();
    assert_eq!(cmd.program, "vagrant");
    assert_eq!(cmd.args, vec!["ssh".to_owned(), "attacker".to_owned()]);
}

#[tokio::test]
async fn shell_command_refuses_an_unknown_named_machine() {
    // Naming a machine the lab doesn't have is still an error, access machine or not.
    let f = fixture_with(FLAG_PORTAL_ACCESS);
    let run = f.runner.open(&f.manifest, None).unwrap();
    assert_eq!(run.shell_command(Some("nope")).unwrap_err().code(), ErrorCode::MachineUnknown);

    let bare = fixture();
    let bare = bare.runner.open(&bare.manifest, None).unwrap();
    assert_eq!(bare.shell_command(Some("nope")).unwrap_err().code(), ErrorCode::MachineUnknown);
}

/// A console into any machine of a Compose run: `docker compose exec` of its service, bash where
/// the image has it, else sh.
#[tokio::test]
async fn a_console_opens_any_compose_service_by_name() {
    let f = fixture_with(FLAG_PORTAL_ACCESS);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let cmd = run.console_command("web").unwrap();
    assert_eq!(cmd.program, "docker");
    let line = cmd.args.join(" ");
    assert!(line.starts_with("compose --project-name"), "{line}");
    assert!(line.contains(" exec web sh -c "), "{line}");
    assert!(line.ends_with(crate::driver::console::CONTAINER_SHELL), "{line}");
    assert_eq!(run.console_command("nope").unwrap_err().code(), ErrorCode::MachineUnknown);
}

/// A console into a Vagrant machine is `vagrant ssh` of its define.
#[tokio::test]
async fn a_console_opens_any_vagrant_define_by_name() {
    let (folder, _fake, runner) = vagrant_access(|_| Reply::ok(""));
    let run = runner.open(&folder.manifest, None).unwrap();
    let cmd = run.console_command("web").unwrap();
    assert_eq!(cmd.program, "vagrant");
    assert_eq!(cmd.args, vec!["ssh".to_owned(), "web".to_owned()]);
}

/// Retiring a Vagrant machine destroys its define, and only it, ahead of its removal from the
/// manifest (an update leaves a define the Vagrantfile no longer has).
#[tokio::test]
async fn retiring_a_vagrant_machine_destroys_only_its_define() {
    let (folder, fake, runner) = vagrant_access(|call| {
        if call.line().contains("status") {
            return Reply::ok("1,web,state,running\n1,isoloom-controller,state,running\n1,attacker,state,running");
        }
        Reply::ok("")
    });
    let run = runner.open(&folder.manifest, None).unwrap();
    run.retire("attacker", &RunOptions::new(), &Recorder::new()).await.unwrap();
    let destroys: Vec<String> = fake.calls().iter().map(Call::line).filter(|l| l.contains("destroy")).collect();
    assert_eq!(destroys.len(), 1, "{destroys:?}");
    assert!(destroys[0].ends_with("destroy -f attacker"), "{destroys:?}");
    let err = run.retire("nope", &RunOptions::new(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::MachineUnknown);
}

/// Retiring a Compose service stops and removes it, with the services in its namespace, through
/// its profile: one that stays in the Compose files but leaves the default start (back to an
/// on-demand machine) would otherwise keep running whatever the update's start does.
#[tokio::test]
async fn retiring_a_compose_service_removes_it_with_its_namespace() {
    let f = fixture_answering(FLAG_PORTAL_ACCESS, routed_answer);
    f.set(Containers::Up);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.retire("attacker", &RunOptions::new(), &events).await.unwrap();
    let rm = f.fake.last(" rm -sf").expect("the service is removed");
    assert!(rm.line().contains(" --profile access rm -sf attacker attacker-routes"), "{}", rm.line());
    assert!(events.events().iter().all(|e| e.op == crate::state::Op::Retire));
    let Some(EventKind::Result { ok: true, data }) = kinds(&events).pop() else {
        panic!("{:?}", kinds(&events))
    };
    assert_eq!(data["machine"], "attacker");

    // A machine of the default start: no profile named.
    run.retire("web", &RunOptions::new(), &Recorder::new()).await.unwrap();
    let default = f.fake.last(" rm -sf").unwrap().line();
    assert!(default.ends_with(" rm -sf web") && !default.contains("--profile"), "{default}");
}

/// The lab's Docker, its config one Compose can't be read from.
fn unreadable_config(engine: &Mutex<Engine>, call: &Call) -> Reply {
    if call.line().contains(" config --format json") {
        return Reply::ok("not json");
    }
    answer(engine, call)
}

/// When the services in the access machine's namespace can't be read, attach and detach fail
/// rather than start the machine without its routes (or leave them running).
#[tokio::test]
async fn an_unreadable_config_fails_attach_and_detach_loudly() {
    let f = fixture_answering(FLAG_PORTAL_ACCESS, unreadable_config);
    f.set(Containers::Up);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.attach(None, None, None, &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::ToolFailed, "{err}");
    assert!(f.fake.last("--profile access up").is_none(), "nothing started");
    let detached = run.detach(None, &Recorder::new()).await.unwrap_err();
    assert_eq!(detached.code(), ErrorCode::ToolFailed, "{detached}");
    assert!(f.fake.last(" rm -sf").is_none(), "nothing removed");
}
