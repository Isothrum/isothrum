//! Hot-adding a lab's own access machine into a running run, and taking it back out, through the
//! fake engines: the Compose `attacker` service in its `access` profile, and the Vagrant
//! `attacker` define. A lab that declares none has nothing to attach. No real tool ever runs.

use super::*;
use crate::ErrorCode;
use crate::testing::{FLAG_PORTAL_ACCESS, FLAG_VM_ACCESS, attach, detach};

/// The lab's Docker, the access service's container `c0ffee` once it is up; `connect` answers
/// its join to a network.
fn joining(engine: &Mutex<Engine>, call: &Call, connect: Reply) -> Reply {
    let line = call.line();
    if line.ends_with(" ps -q attacker") {
        return Reply::ok("c0ffee\n");
    }
    if line.starts_with("docker network connect") {
        return connect;
    }
    answer(engine, call)
}

#[tokio::test]
async fn attach_brings_the_access_service_up_in_its_profile_with_the_picked_image() {
    let f = fixture_answering(FLAG_PORTAL_ACCESS, |e, c| joining(e, c, Reply::ok("")));
    f.set(Containers::Up); // the lab is already up; the access machine was left out of it
    let run = f.runner.open(&f.manifest, None).unwrap();

    let events = Recorder::new();
    let report = attach(&run, None, Some("lab"), Some("myimage"), &events).await.unwrap();

    assert_eq!((report.machine.as_str(), report.native.as_str()), ("attacker", "attacker"));
    assert_eq!(report.network.as_deref(), Some("lab"));
    assert_eq!(report.address.as_deref(), Some("10.30.0.99"), "its static address, for the embedder to show");
    assert_eq!(report.image.as_deref(), Some("myimage"));

    let up = f.fake.last("--profile access up").expect("the access profile is brought up");
    assert!(up.line().contains("--no-deps attacker"), "{}", up.line());
    assert_eq!(up.env(isoloom_manifest::access::IMAGE_ENV), Some("myimage"), "the picked image is delivered");
    let joined = f.fake.last("network connect").expect("joined to the chosen network");
    assert_eq!(joined.line(), "docker network connect flag-portal_lab c0ffee", "its rendered native name");
    assert!(matches!(kinds(&events).last(), Some(EventKind::Result { ok: true, .. })));
}

/// A container already on the chosen network is where it should be; any other failure to join
/// it fails the attach, and a network the run doesn't have is refused before anything starts.
#[tokio::test]
async fn attach_to_a_network_tolerates_only_already_being_on_it() {
    let already = fixture_answering(FLAG_PORTAL_ACCESS, |e, c| {
        joining(
            e,
            c,
            Reply::fail(
                1,
                "Error response from daemon: endpoint with name attacker already exists in network flag-portal_lab",
            ),
        )
    });
    already.set(Containers::Up);
    let on_it = already.runner.open(&already.manifest, None).unwrap();
    attach(&on_it, None, Some("lab"), None, &Recorder::new()).await.unwrap();

    let refused = fixture_answering(FLAG_PORTAL_ACCESS, |e, c| {
        joining(e, c, Reply::fail(1, "Error response from daemon: network flag-portal_lab not found"))
    });
    refused.set(Containers::Up);
    let run = refused.runner.open(&refused.manifest, None).unwrap();
    let failed = attach(&run, None, Some("lab"), None, &Recorder::new()).await.unwrap_err();
    assert_eq!(failed.code(), ErrorCode::ToolFailed, "{failed}");
    assert!(failed.to_string().contains("not found"), "{failed}");

    let calls = refused.fake.calls().len();
    let unknown = attach(&run, None, Some("nope"), None, &Recorder::new()).await.unwrap_err();
    assert_eq!(unknown.code(), ErrorCode::Unsupported, "{unknown}");
    assert!(unknown.to_string().contains("network `nope`"), "{unknown}");
    assert_eq!(refused.fake.calls().len(), calls, "nothing ran");
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
    detach(&run, None, &Recorder::new()).await.unwrap();
    let rm = f.fake.last("--profile access rm").unwrap();
    assert!(rm.line().ends_with("rm -sf attacker attacker-routes"), "{}", rm.line());
}

/// The lab up without its access machine, then the machine attached with no image: its `up`
/// call.
async fn attach_plain(f: &Fixture) -> Call {
    f.set(Containers::Up);
    let run = f.runner.open(&f.manifest, None).unwrap();
    attach(&run, None, None, None, &Recorder::new()).await.unwrap();
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
    detach(&run, None, &events).await.unwrap();
    let rm = f.fake.last("--profile access rm").expect("the access service is removed");
    assert!(rm.line().contains("rm -sf attacker"), "{}", rm.line());
    assert!(
        events.events().iter().all(|e| e.op == Some(crate::state::Op::Detach)),
        "a detach's events, though a retire's work"
    );
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
    let err = attach(&run, Some("nope"), None, None, &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::MachineUnknown);
}

#[tokio::test]
async fn attach_refuses_a_lab_that_is_not_running() {
    let f = fixture_with(FLAG_PORTAL_ACCESS);
    // containers left at None: nothing of the run is up
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = attach(&run, None, None, None, &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::NotRunning);
}

/// A runner over the vagrant access fixture (`FLAG_VM_ACCESS`), its Vagrant answered by `answer`.
fn vagrant_access(answer: impl Fn(&Call) -> Reply + Send + Sync + 'static) -> (LabFolder, Arc<FakeProcesses>, Runner) {
    let folder = LabFolder::new("vagrant", FLAG_VM_ACCESS);
    let fake = Arc::new(FakeProcesses::new(answer));
    let ports: Arc<dyn crate::HostPorts> = Arc::new(FakePorts::new(std::iter::empty::<u16>()));
    let runner = Runner::new(runner_config(folder.home.path(), processes(&fake), ports, crate::runner::this_host())).unwrap();
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

    let report = attach(&run, None, None, Some("mybox"), &Recorder::new()).await.unwrap();
    assert_eq!((report.machine.as_str(), report.native.as_str()), ("attacker", "attacker"));

    let up = fake.last("up attacker").expect("the access VM is booted");
    assert_eq!(up.env(isoloom_manifest::access::BOX_ENV), Some("mybox"), "the picked box is delivered");

    detach(&run, None, &Recorder::new()).await.unwrap();
    assert!(fake.last("destroy -f attacker").is_some(), "the access VM is destroyed");

    // A VM's networks are the Vagrantfile's: a chosen one is refused, and no VM is booted.
    let ups = fake.calls().iter().filter(|c| c.line().contains("up attacker")).count();
    let err = attach(&run, None, Some("lab"), None, &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Unsupported, "{err}");
    assert_eq!(fake.calls().iter().filter(|c| c.line().contains("up attacker")).count(), ups);
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
    assert!(line.contains(" exec web sh -c "), "addresses the service, no container guessing: {line}");
    assert!(line.ends_with(crate::driver::shells::CONTAINER_SHELL), "{line}");
    assert_eq!(cmd.cwd, f.manifest.parent().unwrap(), "runs in the run's folder");
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
    assert!(events.events().iter().all(|e| e.op == Some(crate::state::Op::Retire)));
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
    let err = attach(&run, None, None, None, &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::ToolFailed, "{err}");
    assert!(f.fake.last("--profile access up").is_none(), "nothing started");
    let detached = detach(&run, None, &Recorder::new()).await.unwrap_err();
    assert_eq!(detached.code(), ErrorCode::ToolFailed, "{detached}");
    assert!(f.fake.last(" rm -sf").is_none(), "nothing removed");
}
