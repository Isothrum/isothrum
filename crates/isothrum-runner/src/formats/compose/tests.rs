mod contract;
mod namespaces;
mod pins;
mod pulls;

use std::path::Path;

use super::*;
use crate::RunName;
use crate::process::fake::{CallView as _, FakeProcesses, Reply};
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

/// A context for run `lab` of `project` on `fake`, with no state folder and no port to pick.
fn cx<'a>(project: &'a Folder, run: &'a RunName, fake: &'a FakeProcesses) -> ToolCx<'a> {
    cx_in(project, run, fake, Path::new("/nowhere"), &crate::formats::NoPorts)
}

/// [`cx`], its state in `state_dir` and its host ports picked by `ports`.
fn cx_in<'a>(project: &'a Folder, run: &'a RunName, fake: &'a FakeProcesses, state_dir: &'a Path, ports: &'a dyn crate::formats::PickPorts) -> ToolCx<'a> {
    ToolCx {
        state_dir,
        ports,
        ..cx_warned(project, run, fake, &unwarned)
    }
}

/// [`cx`], its warnings given to `warn`.
fn cx_warned<'a>(
    project: &'a Folder,
    run: &'a RunName,
    fake: &'a FakeProcesses,
    warn: &'a (dyn Fn(crate::events::WarningCode, &str) + Send + Sync),
) -> ToolCx<'a> {
    // No state folder, no port to pick, no provider: a Compose project's own.
    ToolCx {
        warn,
        sink: &quiet,
        made: crate::formats::NOTHING_MADE,
        processes: fake,
        ports: &crate::formats::NoPorts,
        state_dir: Path::new("/nowhere"),
        ssh_key: std::path::Path::new("/nowhere/id_ed25519"),
        provider: None,
        host_env: &crate::host_env::EMPTY,
        env: vec![("ISOTHRUM_RUN_KEY".to_owned(), "k3y9ab".to_owned())],
        key: "k3y9ab",
        project,
        run,
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
/// The pull a start makes first.
const PULL: &str = "docker compose -p lab -f * pull --ignore-buildable";
/// The config a project of no published port prints: nothing to pin.
const CONFIG: &str = "docker compose -p lab -f * config --format json";
const NO_PORTS: &str = r#"{"services": {"web": {}}}"#;
const LABEL: &str = "label=com.docker.compose.project=lab";

#[tokio::test]
async fn up_waits_for_the_services_and_names_the_project_after_the_run() {
    let (_dir, project) = folder();
    let run = run();
    let fake = FakeProcesses::script()
        .expect_times(UP, 1, Reply::ok(""))
        .allow(PULL, Reply::ok(""))
        .expect(CONFIG, Reply::ok(NO_PORTS))
        .build();
    Compose.up(&cx(&project, &run, &fake)).await.expect("up");
    let [call] = fake.made(UP).try_into().expect("one up");
    assert_eq!(call.flag("-p"), Some("lab"), "the project is the run: {:?}", call.args);
    assert!(call.env.contains(&("ISOTHRUM_RUN_KEY".to_owned(), "k3y9ab".to_owned())), "{:?}", call.env);
    assert_eq!(call.cwd.as_deref(), Some(project.dir.as_path()));
}

#[tokio::test]
async fn an_up_refused_for_a_name_leftovers_hold_clears_them_by_label_and_is_tried_once_more_only() {
    let (_dir, project) = folder();
    let run = run();
    let taken = Reply::fail(
        1,
        "Error response from daemon: Conflict. The container name \"/lab-web-1\" is already in use by container \"abc\"",
    );
    let fake = FakeProcesses::script()
        .expect_times(UP, 2, taken)
        .allow(PULL, Reply::ok(""))
        .expect(CONFIG, Reply::ok(NO_PORTS))
        .expect_times(&format!("docker container ls --quiet --filter {LABEL} --all"), 1, Reply::ok("abc\n"))
        .expect_times("docker container rm --force --volumes abc", 1, Reply::ok(""))
        .expect_times(&format!("docker network ls --quiet --filter {LABEL}"), 1, Reply::ok(""))
        .expect_times(&format!("docker volume ls --quiet --filter {LABEL}"), 1, Reply::ok(""))
        .build();
    // Once is the recovery's only try (`expect_times(UP, 2`): two refusals end the start, never a
    // loop.
    let e = Compose.up(&cx(&project, &run, &fake)).await.expect_err("taken twice");
    assert_eq!(e.code(), crate::ErrorCode::ToolFailed, "{e}");
}

/// A script reading the config of the project and of `attackbox` (a service under a profile,
/// publishing nothing), as an attach of it does before its `up`.
fn attackbox_configs() -> crate::process::fake::Script {
    FakeProcesses::script()
        .expect(CONFIG, Reply::ok(NO_PORTS))
        .expect(&format!("{CONFIG} attackbox"), Reply::ok(r#"{"services": {"attackbox": {}}}"#))
}

/// `script` with the `down` that undoes a refused first start: the project's objects, then
/// whatever still carries its label.
fn undone(script: crate::process::fake::Script) -> FakeProcesses {
    script
        .expect_times("docker compose -p lab -f * down --volumes --remove-orphans", 1, Reply::ok(""))
        .expect_times(&format!("docker container ls --quiet --filter {LABEL} --all"), 1, Reply::ok(""))
        .expect_times(&format!("docker network ls --quiet --filter {LABEL}"), 1, Reply::ok(""))
        .expect_times(&format!("docker volume ls --quiet --filter {LABEL}"), 1, Reply::ok(""))
        .build()
}

/// A host port Docker refuses at a first start (nothing of the run made yet) is `port_in_use`
/// naming the port Docker names, never 0, in each wording Docker has; Compose made the containers
/// before the refusal, and they go (`down`), so the next start finds none in its way.
#[tokio::test]
async fn a_first_start_docker_refuses_a_host_port_is_port_in_use_naming_it_and_removes_what_it_made() {
    let (_dir, project) = folder();
    let run = run();
    for (said, port) in [
        (include_str!("../../tool_output/fixtures/compose/2.29-port-allocated-v6.txt"), 8180),
        (include_str!("../../tool_output/fixtures/compose/4.30-desktop-listen-in-use.txt"), 8180),
        (include_str!("../../tool_output/fixtures/compose/29.9-bind-host-port-in-use.txt"), 18180),
    ] {
        let fake = undone(
            FakeProcesses::script()
                .expect_times(UP, 1, Reply::fail(1, said))
                .allow(PULL, Reply::ok(""))
                .expect(CONFIG, Reply::ok(NO_PORTS)),
        );
        let e = Compose.up(&cx(&project, &run, &fake)).await.expect_err("taken");
        assert_eq!(
            e.detail().map(|d| d.data),
            Some(crate::Detail::PortInUse {
                port,
                protocol: "tcp".to_owned(),
                holder: None,
            }),
            "{said}"
        );
    }
}

#[tokio::test]
async fn a_run_with_machines_or_an_update_refused_for_a_taken_port_keeps_them() {
    let (_dir, project) = folder();
    let run = run();
    let taken = include_str!("../../tool_output/fixtures/compose/29.9-bind-host-port-in-use.txt");
    // A run with machines (a parked one started again), and an update of a running one, keep
    // theirs: no `down` is scripted.
    let fake = FakeProcesses::script()
        .expect_times(UP, 2, Reply::fail(1, taken))
        .allow(PULL, Reply::ok(""))
        .expect(CONFIG, Reply::ok(NO_PORTS))
        .build();
    let made: &(dyn Fn() -> Result<Vec<String>> + Send + Sync) = &|| Ok(vec!["container:abc".to_owned()]);
    let with_machines = ToolCx {
        made,
        ..cx(&project, &run, &fake)
    };
    let started = Compose.up(&with_machines).await.expect_err("taken");
    assert_eq!(started.code(), crate::ErrorCode::PortInUse, "{started}");
    let updated = Compose.update(&cx(&project, &run, &fake)).await.expect_err("taken");
    assert_eq!(updated.code(), crate::ErrorCode::PortInUse, "{updated}");
}

#[tokio::test]
async fn a_refused_pull_is_download_failed() {
    let (_dir, project) = folder();
    let run = run();
    let refused = include_str!("../../tool_output/fixtures/compose/5.6-pull-access-denied.txt");
    let fake = FakeProcesses::script()
        .expect(UP, Reply::fail(1, refused))
        .allow(PULL, Reply::ok(""))
        .expect(CONFIG, Reply::ok(NO_PORTS))
        .build();
    let e = Compose.up(&cx(&project, &run, &fake)).await.expect_err("refused");
    assert_eq!(e.code(), crate::ErrorCode::DownloadFailed, "{e}");
    assert!(matches!(&e, Error::DownloadFailed { step, .. } if step == "pull-up"), "{e:?}");
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
        .expect("docker compose -p lab -f * config --services", Reply::ok("web\ndb\n"))
        .expect(CONFIG, Reply::ok(r#"{"services": {"web": {}, "db": {}}}"#))
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
        .expect("docker compose -p lab -f * config --services", Reply::ok("web\ndb\n"))
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
    let ps = "docker compose -p lab -f * ps --all --format json";
    let services = "docker compose -p lab -f * config --services";
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
    assert_eq!((failed.exit, failed.stderr.as_str()), (3, "one check failed"), "its own exit status");
}

#[tokio::test]
async fn attach_names_the_service_so_its_profile_starts_it_and_retire_removes_it_with_its_volumes() {
    let (_dir, project) = folder();
    let run = run();
    let state = tempfile::tempdir().expect("a state folder");
    let fake = attackbox_configs()
        .expect("docker compose -p lab -f * up --detach --wait attackbox", Reply::ok(""))
        .expect("docker compose -p lab -f * rm --stop --force --volumes attackbox", Reply::ok(""))
        .build();
    let cx = cx_in(&project, &run, &fake, state.path(), &crate::formats::NoPorts);
    Compose.attach(&cx, "attackbox").await.expect("attached");
    Compose.retire(&cx, "attackbox").await.expect("retired");
}

/// Regression: a machine that stood in for an access machine (`user`, under the `access` profile)
/// retired, the files give the access machine back, its route keeper (which shares `user`'s
/// namespace) under the profile with it. `--remove-orphans` left the keeper's container, a
/// declared service, exited once `user`'s namespace went: the run read failed after the update.
/// The update removes it, as one the files no longer start; an attached service's stays.
#[tokio::test]
async fn an_update_removes_the_containers_of_services_moved_under_a_profile_the_run_hasnt_attached() {
    let (_dir, project) = folder();
    let run = run();
    let state = tempfile::tempdir().expect("a state folder");
    let listing = [
        ps_line("web", "running", "", 0, None),
        ps_line("isoloom-routes-user", "exited", "", 1, None),
        ps_line("isoloom-init-web-1", "exited", "", 0, None),
        ps_line("attackbox", "running", "", 0, None),
    ]
    .join("\n");
    let rm = "docker compose -p lab -f * rm --stop --force --volumes isoloom-routes-user";
    let fake = attackbox_configs()
        .expect("docker compose -p lab -f * up --detach --wait attackbox", Reply::ok(""))
        .expect(UP, Reply::ok(""))
        .allow(PULL, Reply::ok(""))
        .expect("docker compose -p lab -f * config --services", Reply::ok("web\nisoloom-init-web-1\n"))
        .expect("docker compose -p lab -f * ps --all --format json", Reply::ok(&listing))
        .expect_times(rm, 1, Reply::ok(""))
        .build();
    let cx = cx_in(&project, &run, &fake, state.path(), &crate::formats::NoPorts);
    Compose.attach(&cx, "attackbox").await.expect("attached");
    Compose.update(&cx).await.expect("updated");
    let [call] = fake.made(rm).try_into().expect("one removal");
    assert!(!call.args.iter().any(|a| a == "attackbox" || a == "web"), "{:?}", call.args);
}

#[tokio::test]
async fn machines_are_the_services_the_files_declare_and_reapply_is_one_up() {
    let (_dir, project) = folder();
    let run = run();
    let fake = FakeProcesses::script()
        .expect("docker compose -p lab -f * config --services", Reply::ok("web\ndb\n"))
        .expect(UP, Reply::ok(""))
        .allow(PULL, Reply::ok(""))
        .expect(CONFIG, Reply::ok(NO_PORTS))
        .build();
    let cx = cx(&project, &run, &fake);
    assert_eq!(Compose.machines(&cx).await.expect("read"), ["web", "db"]);
    let change = Change {
        added: vec!["cache".to_owned()],
        ..Change::default()
    };
    Compose.reapply(&cx, &change).await.expect("reapplied");
}

#[tokio::test]
async fn the_console_is_an_exec_with_a_terminal_in_the_project_folder() {
    let (_dir, project) = folder();
    let run = run();
    let fake = FakeProcesses::script().build();
    let console = Compose.console(&cx(&project, &run, &fake), "web", Some("root")).await.expect("a console");
    let args: Vec<&str> = console.args.iter().map(String::as_str).collect();
    let exec = args.iter().position(|a| *a == "exec").expect("an exec");
    assert_eq!(args.get(exec..exec + 6), Some(&["exec", "--user", "root", "web", "sh", "-c"][..]), "{args:?}");
    assert!(!args.contains(&"-T"), "a console has a terminal: {args:?}");
}
