//! A resume and the services in the network namespace of one stopped outside the run.

use super::*;

/// A service stopped outside the run (`docker stop web`) leaves the services in its network
/// namespace (`network_mode: service:web`) running in the old one, its interfaces gone; Docker
/// gives web a new one when it starts. A resume stops them first, so its `up` starts them after
/// web, in the new one; one left running, `up --no-recreate` waited for it, and a sidecar that
/// sets web's routes ended there (exit 1) or came back unhealthy: the resume failed (the
/// launcher's resilience-docker-stopped, 2026-11-16). Those joining it through one of them go
/// too; those of a running service, and one already ended, stay as they are.
#[tokio::test]
async fn a_resume_stops_the_services_left_in_the_network_namespace_of_one_stopped_before_it_starts_them() {
    let (_dir, project) = folder();
    let run = run();
    let listing = [
        ps_line("web", "exited", "", 0, None),
        ps_line("isoloom-routes-web", "running", "healthy", 0, None),
        ps_line("keeper", "restarting", "", 1, None),
        ps_line("old", "exited", "", 1, None),
        ps_line("cache", "running", "healthy", 0, None),
        ps_line("isoloom-routes-cache", "running", "healthy", 0, None),
    ]
    .join("\n");
    let config = r#"{"services": {
        "web": {}, "cache": {},
        "isoloom-routes-web": {"network_mode": "service:web"},
        "keeper": {"network_mode": "service:isoloom-routes-web"},
        "old": {"network_mode": "service:web"},
        "isoloom-routes-cache": {"network_mode": "service:cache"}
    }}"#;
    let stop = "docker compose -p lab -f * stop isoloom-routes-web keeper";
    let up = "docker compose -p lab -f * up --detach --wait --no-recreate";
    let fake = FakeProcesses::script()
        .expect("docker compose -p lab -f * ps --all --format json", Reply::ok(&listing))
        .expect(
            "docker compose -p lab -f * config --services",
            Reply::ok("web\ncache\nisoloom-routes-web\nkeeper\nold\nisoloom-routes-cache\n"),
        )
        .expect(CONFIG, Reply::ok(config))
        .expect_times(stop, 1, Reply::ok(""))
        .expect_times(up, 1, Reply::ok(""))
        .build();
    Compose.resume(&cx(&project, &run, &fake)).await.expect("resumed");
    let [stopped] = fake.made("docker compose -p lab -f * stop").try_into().expect("one stop");
    assert_eq!(
        stopped.args.iter().skip_while(|a| *a != "stop").skip(1).collect::<Vec<_>>(),
        ["isoloom-routes-web", "keeper"]
    );
    let lines = fake.lines();
    let at = |what: &str| {
        lines
            .iter()
            .position(|l| l.contains(what))
            .unwrap_or_else(|| panic!("no `{what}` in {lines:?}"))
    };
    assert!(at(" stop ") < at(" up "), "stopped before the start: {lines:?}");
}
