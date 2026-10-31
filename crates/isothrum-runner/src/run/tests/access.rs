//! Hot-adding the learner's attacker box into a running run, and taking it back out, through the
//! fake engines: an ad-hoc Docker container on the lab's network (the default, no manifest machine),
//! the Compose `attacker` service in its `access` profile, and the Vagrant `attacker` define. No
//! real tool ever runs.

use super::*;
use crate::ErrorCode;
use crate::testing::{FLAG_PORTAL, FLAG_PORTAL_ACCESS, FLAG_VM_ACCESS};

/// The fake of the lab's Docker, extended to answer the ad-hoc box's calls: `docker inspect`
/// reports a lab address, and `run`/`rm`/`network connect` succeed (left to the lab model).
fn adhoc_answer(engine: &Mutex<Engine>, call: &Call) -> Reply {
    if call.line().contains("inspect -f") {
        return Reply::ok(r#"{"flag-portal_lab":{"IPAddress":"10.30.0.123"}}"#);
    }
    answer(engine, call)
}

#[tokio::test]
async fn attach_brings_the_access_service_up_in_its_profile_with_the_picked_image() {
    let f = fixture_with(FLAG_PORTAL_ACCESS);
    f.set(Containers::Up); // the lab is already up; the access box was left out of it
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

#[tokio::test]
async fn attach_without_an_image_lets_the_services_default_stand() {
    let f = fixture_with(FLAG_PORTAL_ACCESS);
    f.set(Containers::Up);
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.attach(None, None, None, &Recorder::new()).await.unwrap();
    let up = f.fake.last("--profile access up").unwrap();
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

/// The lab's Docker, extended to report the ad-hoc box up when its status is inspected.
fn adhoc_status_answer(engine: &Mutex<Engine>, call: &Call) -> Reply {
    if call.line().contains("inspect -f") {
        return Reply::ok(r#"true|{"flag-portal_lab":{"IPAddress":"10.30.0.123"}}"#);
    }
    answer(engine, call)
}

#[tokio::test]
async fn status_shows_the_ad_hoc_access_box_once_it_is_up() {
    // flag-portal declares no access machine, so the box is ad-hoc and `ps` never lists it; the
    // run's status probes for it and shows it as an access machine with its lab address.
    let f = fixture_answering(FLAG_PORTAL, adhoc_status_answer);
    f.set(Containers::Up);
    let run = f.runner.open(&f.manifest, None).unwrap();

    let status = run.status().await.unwrap();
    let access = status
        .machines
        .iter()
        .find(|m| m.role == isoloom_manifest::Role::Access)
        .expect("the ad-hoc box shows as an access machine");
    assert_eq!(access.state, crate::MachineState::Running);
    assert_eq!(access.addresses.get("lab").map(String::as_str), Some("10.30.0.123"));
    // The lab's own targets are unaffected, and the run is still ready off its targets alone.
    assert!(status.machines.iter().any(|m| m.name == "web" && m.role == isoloom_manifest::Role::Target));
    assert_eq!(status.state, crate::RunState::Ready);
}

#[tokio::test]
async fn status_has_no_access_machine_when_no_box_is_attached() {
    // No ad-hoc box: the inspect reads empty, so the status carries no access machine.
    let f = fixture();
    f.set(Containers::Up);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let status = run.status().await.unwrap();
    assert!(status.machines.iter().all(|m| m.role != isoloom_manifest::Role::Access));
}

#[tokio::test]
async fn attach_stands_up_an_ad_hoc_box_when_the_lab_has_no_access_machine() {
    // flag-portal declares no access machine: attach no longer errors, it makes one ad hoc.
    let f = fixture_answering(FLAG_PORTAL, adhoc_answer);
    f.set(Containers::Up);
    let run = f.runner.open(&f.manifest, None).unwrap();

    let events = Recorder::new();
    let report = run.attach(None, None, Some("myimage"), &events).await.unwrap();

    assert_eq!(report.machine, "flag-portal-isoloom-access");
    assert_eq!(report.native, "flag-portal-isoloom-access");
    assert_eq!(report.network.as_deref(), Some("lab"), "the lab's first network");
    assert_eq!(report.address.as_deref(), Some("10.30.0.123"), "its address, read with docker inspect");
    assert_eq!(report.image.as_deref(), Some("myimage"));

    let run_call = f.fake.last("run -d").expect("an ad-hoc container is started detached");
    let line = run_call.line();
    assert!(line.contains("--name flag-portal-isoloom-access"), "named after the project: {line}");
    assert!(line.contains("--network flag-portal_lab"), "on the lab's project network: {line}");
    assert!(line.contains("myimage") && line.ends_with("sleep infinity"), "the picked image, idle: {line}");
    assert!(matches!(kinds(&events).last(), Some(EventKind::Result { ok: true, .. })));
}

#[tokio::test]
async fn down_reaps_a_leftover_ad_hoc_attack_box() {
    // The ad-hoc box has no Compose project, so the lab's `down` would leave it running (and a
    // workload scan would still group it under the lab). `down` must remove it by name.
    let f = fixture_answering(FLAG_PORTAL, adhoc_answer);
    f.set(Containers::Up);
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.attach(None, None, None, &Recorder::new()).await.unwrap();

    run.down(&crate::RunOptions::default(), &Recorder::new()).await.unwrap();
    assert!(
        f.fake.last("rm -f flag-portal-isoloom-access").is_some(),
        "the ad-hoc box is removed on down: {:?}",
        f.fake.lines()
    );
}

#[tokio::test]
async fn an_ad_hoc_box_defaults_to_kali() {
    let f = fixture_answering(FLAG_PORTAL, adhoc_answer);
    f.set(Containers::Up);
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.attach(None, None, None, &Recorder::new()).await.unwrap();
    let run_call = f.fake.last("run -d").unwrap();
    assert!(
        run_call.line().contains(isoloom_manifest::access::DEFAULT_IMAGE),
        "the Kali default stands when the learner picks nothing: {}",
        run_call.line()
    );
}

#[tokio::test]
async fn detach_removes_the_ad_hoc_box_by_name() {
    let f = fixture_answering(FLAG_PORTAL, adhoc_answer);
    f.set(Containers::Up);
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.detach(None, &Recorder::new()).await.unwrap();
    let rm = f.fake.last("rm -f flag-portal-isoloom-access").expect("the ad-hoc box is removed by name");
    assert!(rm.line().contains("rm -f flag-portal-isoloom-access"), "{}", rm.line());
}

#[tokio::test]
async fn shell_command_execs_into_the_ad_hoc_box() {
    let f = fixture(); // flag-portal: no access machine, so an ad-hoc box
    let run = f.runner.open(&f.manifest, None).unwrap();
    let cmd = run.shell_command(None).unwrap();
    assert_eq!(cmd.program, "docker");
    let line = cmd.args.join(" ");
    assert!(line.starts_with("exec -it flag-portal-isoloom-access"), "{line}");
    assert!(line.contains("exec bash") && line.contains("exec sh"), "bash with an sh fallback: {line}");
    assert!(cmd.cwd.is_some(), "runs in the run's folder");
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

#[tokio::test]
async fn shell_command_opens_the_compose_service_by_name() {
    let f = fixture_with(FLAG_PORTAL_ACCESS);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let cmd = run.shell_command(None).unwrap();
    assert_eq!(cmd.program, "docker");
    let line = cmd.args.join(" ");
    assert!(line.starts_with("compose --project-name"), "{line}");
    assert!(line.ends_with("exec attacker bash"), "addresses the service, no container guessing: {line}");
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
