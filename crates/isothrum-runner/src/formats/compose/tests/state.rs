//! What a Compose run reads of its services: each one, its ports and the run's state, and what the project label marks.

use super::*;

#[tokio::test]
async fn observe_reads_each_service_and_its_published_ports_and_derives_the_runs_state() {
    let (_dir, project) = folder();
    let run = run();
    let listing = [
        ps_line("web", "running", "healthy", 0, Some((41001, 80))),
        ps_line("init-web-1", "exited", "", 0, None),
        ps_line("db", "running", "", 0, None),
    ]
    .join("\n");
    let fake = FakeProcesses::script()
        .expect("docker compose -p stack -f * ps --all --format json", Reply::ok(&listing))
        .expect("docker compose -p stack -f * config --services", Reply::ok("web\ndb\n"))
        .build();
    let (seen, state) = Compose.observe(&cx(&project, &run, &fake)).await.expect("read");
    assert_eq!(state, RunState::Ready, "an init job that ended well is no failure");
    let web = seen.machines.iter().find(|m| m.name == "web").expect("web");
    assert_eq!((web.published[0].guest.get(), web.published[0].host.get()), (80, 41001));
}

/// A service the project starts whose container was removed (`docker rm -f web`) reads absent, and
/// the run isn't whole: never ready, and a resume is refused naming it before anything acts.
#[tokio::test]
async fn a_service_removed_outside_the_run_reads_absent_and_its_resume_is_refused_naming_it() {
    let (_dir, project) = folder();
    let run = run();
    let ps = "docker compose -p stack -f * ps --all --format json";
    let services = "docker compose -p stack -f * config --services";
    let fake = FakeProcesses::script()
        .expect_times(ps, 2, Reply::ok(&ps_line("cache", "running", "", 0, None)))
        .expect_times(services, 2, Reply::ok("web\ncache\n"))
        .build();
    let cx = cx(&project, &run, &fake);
    let (seen, state) = Compose.observe(&cx).await.expect("read");
    let states: Vec<(&str, &MachineState)> = seen.machines.iter().map(|m| (m.name.as_str(), &m.state)).collect();
    assert_eq!(states, [("cache", &MachineState::Running), ("web", &MachineState::Absent)]);
    assert_eq!(state, RunState::Failed, "not whole: never ready");
    let e = Compose.resume(&cx).await.expect_err("web is gone");
    assert!(matches!(&e, Error::PartlyGone { machines, .. } if machines == &["web"]), "{e:?}");
    assert_eq!(e.code(), crate::ErrorCode::NothingToResume);
}

#[test]
fn the_runs_state_follows_its_services() {
    use MachineState::{Absent, Exited, Running, Stopped, Unhealthy};
    let m = |state: &MachineState| SeenMachine {
        name: "x".to_owned(),
        state: state.clone(),
        published: Vec::new(),
        kind: None,
        address: None,
        windows: false,
    };
    for (services, expected) in [
        (&[][..], RunState::Absent),
        (&[Absent, Absent][..], RunState::Absent),
        (&[Running, Exited(0)][..], RunState::Ready),
        (&[Stopped, Exited(137)][..], RunState::Parked),
        (&[Running, Unhealthy][..], RunState::Failed),
        // Regression: a lone unhealthy service read as nothing running, so the run read parked.
        (&[Unhealthy][..], RunState::Failed),
        (&[Running, Exited(1)][..], RunState::Failed),
        (&[Running, Absent][..], RunState::Failed),
    ] {
        let machines: Vec<SeenMachine> = services.iter().map(m).collect();
        assert_eq!(run_state(&machines), expected, "{services:?}");
    }
}

#[tokio::test]
async fn the_resources_are_what_the_project_label_marks_by_id() {
    let (_dir, project) = folder();
    let run = run();
    let fake = FakeProcesses::script()
        .expect(&format!("docker container ls --quiet --filter {LABEL} --all"), Reply::ok("c1\nc2\n"))
        .expect(&format!("docker network ls --quiet --filter {LABEL}"), Reply::ok("n1\n"))
        .expect(&format!("docker volume ls --quiet --filter {LABEL}"), Reply::ok("v1\n"))
        .expect("docker volume rm v9", Reply::fail(1, "Error: No such volume: v9"))
        .build();
    let cx = cx(&project, &run, &fake);
    let found = Compose.found(&cx).await.expect("found");
    let keys: Vec<(&str, ResourceKind, Lifetime)> = found.iter().map(|f| (f.key.as_str(), f.kind, f.lifetime)).collect();
    assert_eq!(
        keys,
        [
            ("container:c1", ResourceKind::Container, Lifetime::Machines),
            ("container:c2", ResourceKind::Container, Lifetime::Machines),
            ("network:n1", ResourceKind::Network, Lifetime::Run),
            ("volume:v1", ResourceKind::Volume, Lifetime::Run),
        ]
    );
    Compose
        .destroy_by_id(&cx, "volume:v9", ResourceKind::Volume, "v9")
        .await
        .expect("already gone is gone");
    let vm = Compose.destroy_by_id(&cx, "vm:x", ResourceKind::Vm, "x").await.expect_err("not Compose's");
    assert_eq!(vm.code(), crate::ErrorCode::Unsupported, "{vm}");
}
