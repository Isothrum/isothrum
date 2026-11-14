use std::path::Path;

use super::*;
use crate::RunName;
use crate::process::fake::{FakeProcesses, Reply};
use crate::project::{Format, Project as Folder};

/// A Compose project in a temporary folder.
fn folder() -> (tempfile::TempDir, Folder) {
    let dir = tempfile::tempdir().expect("a folder");
    std::fs::write(dir.path().join("compose.yaml"), "services: {}\n").expect("a compose file");
    let project = Folder::open(dir.path(), Some(Format::Compose)).expect("a project");
    (dir, project)
}

fn quiet(_: crate::events::Stream, _: &str) {}

fn unwarned(_: crate::events::WarningCode, _: &str) {}

/// Runs `f` with a context for run `lab` of `project` on `fake`.
fn cx<'a>(project: &'a Folder, run: &'a RunName, fake: &'a FakeProcesses) -> ToolCx<'a> {
    ToolCx {
        project,
        run,
        key: "k3y9ab",
        state_dir: Path::new("/nowhere"),
        provider: None,
        env: vec![("ISOTHRUM_RUN_KEY".to_owned(), "k3y9ab".to_owned())],
        processes: fake,
        sink: &quiet,
        warn: &unwarned,
    }
}

fn run() -> RunName {
    RunName::new("lab").expect("a name")
}

/// One `ps` line for `service` in `state` (healthy when `health`), publishing `published` →
/// `target` on TCP when given.
fn ps_line(service: &str, state: &str, health: &str, exit: i32, ports: Option<(u16, u16)>) -> String {
    let publishers = ports.map_or_else(
        || "[]".to_owned(),
        |(host, target)| format!(r#"[{{"PublishedPort":{host},"TargetPort":{target},"Protocol":"tcp"}}]"#),
    );
    format!(r#"{{"Service":"{service}","State":"{state}","Health":"{health}","ExitCode":{exit},"Publishers":{publishers},"Labels":""}}"#)
}

const UP: &str = "docker compose -p lab -f * up --detach --wait --remove-orphans";
const LABEL: &str = "label=com.docker.compose.project=lab";

#[tokio::test]
async fn up_waits_for_the_services_and_names_the_project_after_the_run() {
    let (_dir, project) = folder();
    let run = run();
    let fake = FakeProcesses::script().expect_times(UP, 1, Reply::ok("")).build();
    Compose.up(&cx(&project, &run, &fake)).await.expect("up");
    let call = fake.last("up").expect("an up");
    assert!(call.env.iter().any(|(k, v)| k == "ISOTHRUM_RUN_KEY" && v == "k3y9ab"), "{:?}", call.env);
    assert_eq!(call.cwd.as_deref(), Some(project.dir.as_path()));
}

#[tokio::test]
async fn an_up_over_an_earlier_starts_leftovers_clears_them_by_label_and_starts_once_more() {
    let (_dir, project) = folder();
    let run = run();
    let taken = Reply::fail(
        1,
        "Error response from daemon: Conflict. The container name \"/lab-web-1\" is already in use by container \"abc\"",
    );
    let fake = FakeProcesses::script()
        .expect_times(UP, 2, taken)
        .expect_times(&format!("docker container ls --quiet --filter {LABEL} --all"), 1, Reply::ok("abc\n"))
        .expect_times("docker container rm --force --volumes abc", 1, Reply::ok(""))
        .expect_times(&format!("docker network ls --quiet --filter {LABEL}"), 1, Reply::ok(""))
        .expect_times(&format!("docker volume ls --quiet --filter {LABEL}"), 1, Reply::ok(""))
        .build();
    let e = Compose.up(&cx(&project, &run, &fake)).await.expect_err("taken twice");
    assert_eq!(e.code(), crate::ErrorCode::ToolFailed, "{e}");
    // Once is the recovery's only try: two refusals end the start, never a loop.
    let ups = fake.lines().iter().filter(|l| l.contains(" up ")).count();
    assert_eq!(ups, 2);
}

#[tokio::test]
async fn a_taken_host_port_is_port_in_use() {
    let (_dir, project) = folder();
    let run = run();
    let fake = FakeProcesses::script()
        .expect(UP, Reply::fail(1, "Bind for 127.0.0.1:8080 failed: port is already allocated"))
        .build();
    let e = Compose.up(&cx(&project, &run, &fake)).await.expect_err("taken");
    assert_eq!(e.code(), crate::ErrorCode::PortInUse);
}

#[tokio::test]
async fn down_takes_the_volumes_then_whatever_still_carries_the_label() {
    let (_dir, project) = folder();
    let run = run();
    let fake = FakeProcesses::script()
        .expect("docker compose -p lab -f * down --volumes --remove-orphans", Reply::ok(""))
        .expect(&format!("docker container ls --quiet --filter {LABEL} --all"), Reply::ok(""))
        .expect(&format!("docker network ls --quiet --filter {LABEL}"), Reply::ok("n1\n"))
        .expect("docker network rm n1", Reply::fail(1, "Error: network n1 not found"))
        .expect(&format!("docker volume ls --quiet --filter {LABEL}"), Reply::ok(""))
        .build();
    Compose.down(&cx(&project, &run, &fake)).await.expect("a network gone meanwhile is gone");
}

#[tokio::test]
async fn park_pauses_or_stops_and_resume_unpauses_what_was_paused_then_starts_without_remaking() {
    let (_dir, project) = folder();
    let run = run();
    let fake = FakeProcesses::script()
        .expect("docker compose -p lab -f * pause", Reply::ok(""))
        .expect("docker compose -p lab -f * stop", Reply::ok(""))
        .expect(
            "docker compose -p lab -f * ps --all --format json",
            Reply::ok(&[ps_line("web", "paused", "", 0, None), ps_line("db", "exited", "", 0, None)].join("\n")),
        )
        .expect("docker compose -p lab -f * unpause web", Reply::ok(""))
        .expect("docker compose -p lab -f * up --detach --wait --no-recreate", Reply::ok(""))
        .build();
    let cx = cx(&project, &run, &fake);
    Compose.park(&cx, Park::Pause).await.expect("paused");
    Compose.park(&cx, Park::Shutdown).await.expect("stopped");
    Compose.resume(&cx).await.expect("resumed");
}

#[tokio::test]
async fn observe_reads_each_service_and_its_published_ports_and_derives_the_runs_state() {
    let (_dir, project) = folder();
    let run = run();
    let listing = [
        ps_line("web", "running", "healthy", 0, Some((41001, 80))),
        ps_line("isoloom-init-web-1", "exited", "", 0, None),
        ps_line("db", "running", "", 0, None),
    ]
    .join("\n");
    let fake = FakeProcesses::script()
        .expect("docker compose -p lab -f * ps --all --format json", Reply::ok(&listing))
        .build();
    let (seen, state) = Compose.observe(&cx(&project, &run, &fake)).await.expect("read");
    assert_eq!(state, RunState::Ready, "an init job that ended well is no failure");
    let web = seen.machines.iter().find(|m| m.name == "web").expect("web");
    assert_eq!((web.published[0].guest, web.published[0].host), (80, 41001));
}

#[test]
fn the_runs_state_follows_its_services() {
    let m = |state: MachineState| SeenMachine {
        name: "x".to_owned(),
        state,
        published: Vec::new(),
    };
    assert_eq!(run_state(&[]), RunState::Absent);
    assert_eq!(run_state(&[m(MachineState::Running), m(MachineState::Exited(0))]), RunState::Ready);
    assert_eq!(run_state(&[m(MachineState::Stopped), m(MachineState::Exited(137))]), RunState::Parked);
    assert_eq!(run_state(&[m(MachineState::Running), m(MachineState::Unhealthy)]), RunState::Failed);
    assert_eq!(run_state(&[m(MachineState::Running), m(MachineState::Exited(1))]), RunState::Failed);
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
    assert!(Compose.destroy_by_id(&cx, "vm:x", ResourceKind::Vm, "x").await.is_err(), "not Compose's");
}

#[tokio::test]
async fn exec_runs_in_the_running_container_or_as_a_one_off_and_says_how_it_ended() {
    let (_dir, project) = folder();
    let run = run();
    let fake = FakeProcesses::script()
        .allow(
            "docker compose -p lab -f * ps --all --format json",
            Reply::ok(&ps_line("web", "running", "", 0, None)),
        )
        .expect("docker compose -p lab -f * exec -T web cat /flag", Reply::ok("flag{x}"))
        .expect("docker compose -p lab -f * run --rm -T checks ./run.sh", Reply::fail(3, "one check failed"))
        .build();
    let cx = cx(&project, &run, &fake);
    let ran = Compose.exec(&cx, "web", &["cat".to_owned(), "/flag".to_owned()]).await.expect("ran");
    assert_eq!((ran.exit, ran.stdout.as_str()), (0, "flag{x}"));
    let failed = Compose.exec(&cx, "checks", &["./run.sh".to_owned()]).await.expect("ran");
    assert_ne!(failed.exit, 0);
    assert!(failed.stderr.contains("one check failed"), "{failed:?}");
}

#[tokio::test]
async fn attach_names_the_service_so_its_profile_starts_it_and_retire_removes_it_with_its_volumes() {
    let (_dir, project) = folder();
    let run = run();
    let fake = FakeProcesses::script()
        .expect("docker compose -p lab -f * up --detach --wait attackbox", Reply::ok(""))
        .expect("docker compose -p lab -f * rm --stop --force --volumes attackbox", Reply::ok(""))
        .build();
    let cx = cx(&project, &run, &fake);
    Compose.attach(&cx, "attackbox").await.expect("attached");
    Compose.retire(&cx, "attackbox").await.expect("retired");
    assert_eq!(
        Compose.provision(&cx, None).await.expect_err("nothing to provision").code(),
        crate::ErrorCode::Unsupported
    );
}

#[tokio::test]
async fn machines_are_the_services_the_files_declare_and_reapply_is_one_up() {
    let (_dir, project) = folder();
    let run = run();
    let fake = FakeProcesses::script()
        .expect("docker compose -p lab -f * config --services", Reply::ok("web\ndb\n"))
        .expect(UP, Reply::ok(""))
        .build();
    let cx = cx(&project, &run, &fake);
    assert_eq!(Compose.machines(&cx).await.expect("read"), ["web", "db"]);
    let change = Change {
        added: vec!["cache".to_owned()],
        ..Change::default()
    };
    Compose.reapply(&cx, &change).await.expect("reapplied");
}

#[test]
fn the_console_is_an_exec_with_a_terminal_in_the_project_folder() {
    let (_dir, project) = folder();
    let run = run();
    let fake = FakeProcesses::script().build();
    let console = Compose.console(&cx(&project, &run, &fake), "web", Some("root")).expect("a console");
    let line = console.args.join(" ");
    assert!(line.contains("exec --user root web sh -c"), "{line}");
    assert!(!line.contains("-T"), "a console has a terminal: {line}");
}
