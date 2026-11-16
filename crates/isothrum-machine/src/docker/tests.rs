use std::sync::Arc;

use isothrum_runner::ProcessRunner;
use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::testing::linux_host;

const DENIED: &str = "permission denied while trying to connect to the Docker daemon socket at unix:///var/run/docker.sock";
const DOWN: &str = "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?";

/// Docker on a host running `os`, its CLI answered by `tools`, which the test keeps to read the
/// calls made.
fn on(os: HostOs, tools: FakeProcesses) -> (Context, Arc<FakeProcesses>) {
    let tools = Arc::new(tools);
    let runner: Arc<dyn ProcessRunner> = Arc::<FakeProcesses>::clone(&tools);
    let mut host = linux_host(std::path::Path::new("/nonexistent/isothrum-machine-test"));
    host.os = os;
    (Context::new(runner, host), tools)
}

#[test]
fn every_engine_has_its_id_back_and_its_serde_name_is_its_id() {
    for engine in Engine::ALL {
        assert_eq!(Engine::from_id(engine.id()), Some(engine));
        assert_eq!(serde_json::to_value(engine).unwrap(), serde_json::json!(engine.id()));
        assert!(engine.url().starts_with("https://") && !engine.label().is_empty());
    }
    assert_eq!(Engine::from_id("some-engine"), None);
}

#[test]
fn names_the_engine_from_the_daemon_and_the_context() {
    assert_eq!(engine_named("OrbStack", "default"), Engine::Orbstack);
    assert_eq!(engine_named("Ubuntu 24.04", "colima"), Engine::Colima);
    assert_eq!(engine_named("Rancher Desktop WSL Distribution", "default"), Engine::RancherDesktop);
    assert_eq!(engine_named("Docker Desktop", "default"), Engine::DockerDesktop);
    assert_eq!(engine_named("Debian", "desktop-linux"), Engine::DockerDesktop);
    assert_eq!(engine_named("Ubuntu 24.04 LTS", "default"), Engine::Native);
    assert_eq!(engine_named("podman", "default"), Engine::Podman);
    assert_eq!(engine_named("Ubuntu", "rancher-desktop"), Engine::RancherDesktop);
}

#[test]
fn contexts_are_read_with_the_current_one_marked_by_true_or_a_star() {
    assert_eq!(
        contexts_from("default\ttrue\ndesktop-linux\tfalse\norbstack\tfalse\n"),
        [("default".to_owned(), true), ("desktop-linux".into(), false), ("orbstack".into(), false)]
    );
    assert_eq!(
        contexts_from("default *\ncolima-dev\n\n"),
        [("default".to_owned(), true), ("colima-dev".into(), false)]
    );
    assert_eq!(contexts_from(""), [] as [(String, bool); 0]);
}

#[test]
fn context_names_map_to_engines_and_default_only_counts_on_linux() {
    assert_eq!(context_engine("desktop-linux", HostOs::Macos), Some(Engine::DockerDesktop));
    assert_eq!(context_engine("colima-dev", HostOs::Macos), Some(Engine::Colima));
    assert_eq!(context_engine("colima", HostOs::Macos), Some(Engine::Colima));
    assert_eq!(context_engine("my-remote", HostOs::Macos), None);
    assert_eq!(context_engine("default", HostOs::Linux), Some(Engine::Native));
    assert_eq!(context_engine("default", HostOs::Macos), None);
    assert_eq!(context_engine("podman-machine-default", HostOs::Macos), Some(Engine::Podman));
    assert_eq!(context_engine("rancher-desktop", HostOs::Windows), Some(Engine::RancherDesktop));
}

#[test]
fn a_refused_socket_or_pipe_is_told_from_a_stopped_daemon() {
    assert!(says_denied(
        "permission denied while trying to connect to the docker API at unix:///var/run/docker.sock"
    ));
    assert!(says_denied(DENIED));
    assert!(!says_denied(DOWN));
    assert!(says_denied(
        "permission denied while trying to connect to the docker API at npipe:////./pipe/dockerDesktopLinuxEngine: open //./pipe/dockerDesktopLinuxEngine: Access is denied."
    ));
    assert!(says_denied("error during connect: open //./pipe/docker_engine: Access is denied."));
    assert!(!says_denied(
        "error during connect: open //./pipe/dockerDesktopLinuxEngine: The system cannot find the file specified."
    ));
    assert!(!says_denied("Access is denied."), "an unrelated refusal");
}

/// Several engines side by side: each context is asked with `docker --context <c> info`, so each
/// engine has its own state, and the current context is marked; one that doesn't answer is still
/// listed from its name, stopped; a context that names no engine and doesn't answer is left out.
#[tokio::test(start_paused = true)]
async fn every_engine_is_listed_with_its_own_state_and_the_current_one_marked() {
    let tools = FakeProcesses::script()
        .expect(
            "docker context ls",
            Reply::ok("desktop-linux\tfalse\norbstack\ttrue\ncolima\tfalse\nmy-remote\tfalse\n"),
        )
        .expect("docker --context orbstack info", Reply::ok("OrbStack\n"))
        .expect("docker --context desktop-linux info", Reply::ok("Docker Desktop\n"))
        .expect("docker --context colima info", Reply::fail(1, "Cannot connect to the Docker daemon for colima"))
        .expect("docker --context my-remote info", Reply::fail(1, "Cannot connect"))
        .build();
    let (cx, tools) = on(HostOs::Macos, tools);
    let found = engines(&cx).await;
    let brief: Vec<(Engine, &str, bool, bool)> = found.iter().map(|e| (e.engine, e.context.as_str(), e.running, e.current)).collect();
    assert_eq!(
        brief,
        [
            (Engine::Orbstack, "orbstack", true, true),
            (Engine::DockerDesktop, "desktop-linux", true, false),
            (Engine::Colima, "colima", false, false)
        ]
    );
    assert_eq!(running_of(&found), [Engine::Orbstack, Engine::DockerDesktop]);
    assert_eq!(tools.made("docker --context * info").len(), 4, "every context probed once");
}

/// A context that says nothing by its name (`default` on a Mac) is known by the daemon it
/// reaches; two contexts to one engine count once.
#[tokio::test(start_paused = true)]
async fn a_context_without_a_telling_name_is_known_by_its_daemon() {
    let tools = FakeProcesses::script()
        .expect("docker context ls", Reply::ok("default\ttrue\ndesktop-linux\tfalse\n"))
        .expect("docker --context * info", Reply::ok("Docker Desktop\n"))
        .build();
    let found = engines(&on(HostOs::Macos, tools).0).await;
    let brief: Vec<(Engine, &str, bool, bool)> = found.iter().map(|e| (e.engine, e.context.as_str(), e.running, e.current)).collect();
    assert_eq!(brief, [(Engine::DockerDesktop, "default", true, true)]);
}

#[tokio::test(start_paused = true)]
async fn no_docker_cli_lists_no_engine_and_says_so() {
    let tools = FakeProcesses::script().expect("docker", Reply::missing()).build();
    let (cx, _tools) = on(HostOs::Linux, tools);
    assert_eq!(engines(&cx).await, []);
    assert_eq!(daemon_os(&cx).await, Err(NoAnswer::NoCli));
}

/// Regression (Debian 13's `docker.io` 26.1.5): with the daemon unreachable, `docker info --format
/// …` printed an empty line and exited 0, so a check read Docker as running while every call
/// failed.
#[tokio::test(start_paused = true)]
async fn an_empty_docker_info_is_a_daemon_that_doesnt_answer() {
    let tools = FakeProcesses::script()
        .expect("docker context ls", Reply::ok("default\ttrue\n"))
        .allow("docker info", Reply::ok("\n"))
        .expect("docker --context default info", Reply::ok("\n"))
        .expect("docker version", Reply::fail(1, DOWN))
        .build();
    let (cx, tools) = on(HostOs::Linux, tools);
    let err = daemon_os(&cx).await.unwrap_err();
    assert!(
        matches!(&err, NoAnswer::Down(why) if why.contains("Cannot connect to the Docker daemon")),
        "{err:?}"
    );
    assert!(!tools.made("docker version").is_empty(), "the reason is asked of docker version");
    let found = engines(&cx).await;
    assert_eq!(found.iter().map(|e| (e.engine, e.running)).collect::<Vec<_>>(), [(Engine::Native, false)]);
    // Refused to this account: the same reply tells it apart.
    let denied = FakeProcesses::script()
        .expect("docker info", Reply::ok(""))
        .expect("docker version", Reply::fail(1, DENIED))
        .build();
    assert!(daemon_os(&on(HostOs::Linux, denied).0).await.unwrap_err().denied());
}

/// A daemon that answers `docker version` but leaves `docker info`'s field empty isn't read as up
/// either: there is no answer to go on.
#[tokio::test(start_paused = true)]
async fn an_empty_docker_info_from_a_daemon_that_answers_version_is_still_no_answer() {
    let tools = FakeProcesses::script()
        .expect("docker --context colima info", Reply::ok(" \n"))
        .expect("docker --context colima version", Reply::ok("linux\n"))
        .build();
    let err = daemon_info(&on(HostOs::Linux, tools).0, "{{.ServerVersion}}", Some("colima"))
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "docker info --format {{.ServerVersion}} answered nothing, though the Docker daemon answers"
    );
}

#[tokio::test(start_paused = true)]
async fn a_daemon_that_refuses_this_account_is_denied_not_down() {
    let tools = FakeProcesses::script().expect("docker info", Reply::fail(1, DENIED)).build();
    let err = daemon_os(&on(HostOs::Linux, tools).0).await.unwrap_err();
    assert!(err.denied(), "{err:?}");
    let down = FakeProcesses::script().expect("docker info", Reply::fail(1, DOWN)).build();
    assert!(!daemon_os(&on(HostOs::Linux, down).0).await.unwrap_err().denied());
}

#[tokio::test(start_paused = true)]
async fn the_engine_in_use_is_named_from_the_daemon_and_the_current_context() {
    let tools = FakeProcesses::script().expect("docker context show", Reply::ok("colima\n")).build();
    assert_eq!(engine_in_use(&on(HostOs::Macos, tools).0, "Ubuntu 24.04 LTS").await, Engine::Colima);
    let tools = FakeProcesses::script().expect("docker context show", Reply::fail(1, "no")).build();
    assert_eq!(engine_in_use(&on(HostOs::Linux, tools).0, " Debian GNU/Linux 13 ").await, Engine::Native);
}

#[tokio::test(start_paused = true)]
async fn an_engines_context_is_resolved_to_run_on_it() {
    let tools = FakeProcesses::script()
        .allow("docker context ls", Reply::ok("default *\norbstack\ncolima-dev\n"))
        .build();
    let (cx, _tools) = on(HostOs::Macos, tools);
    assert_eq!(engine_context(&cx, Engine::Colima).await.unwrap(), "colima-dev");
    assert_eq!(engine_context(&cx, Engine::Orbstack).await.unwrap(), "orbstack");
    let none = engine_context(&cx, Engine::Podman).await.unwrap_err();
    assert_eq!(
        (none.clone(), none.to_string()),
        (ContextError::None(Engine::Podman), "no Docker context for podman".to_owned())
    );
}

#[tokio::test(start_paused = true)]
async fn contexts_that_cant_be_listed_say_so_not_that_the_engine_has_none() {
    // A failed `docker context ls` read as no contexts, and a run on a chosen engine failed with
    // "no Docker context for orbstack".
    let tools = FakeProcesses::script()
        .expect("docker context ls", Reply::fail(1, "error: invalid config file"))
        .build();
    let err = engine_context(&on(HostOs::Macos, tools).0, Engine::Orbstack).await.unwrap_err();
    assert!(
        matches!(&err, ContextError::Unlisted(NoAnswer::Down(why)) if why.contains("invalid config file")),
        "{err:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn the_cli_is_pointed_at_the_context_of_the_engine_asked_for() {
    let tools = FakeProcesses::script()
        .allow("docker context ls", Reply::ok("default *\norbstack\ncolima-dev\n"))
        .expect_times("docker context use colima-dev", 1, Reply::ok(""))
        .build();
    let (cx, _tools) = on(HostOs::Macos, tools);
    use_engine(&cx, Engine::Colima).await.unwrap();
    assert_eq!(use_engine(&cx, Engine::Podman).await, Err(ContextError::None(Engine::Podman)));
}
