use super::*;
use crate::cli::testing;
use crate::process::Exit;
use crate::process::fake::{FakeProcesses, Reply};
use crate::project::platform::Protocol::{Tcp, Udp};

fn failed(stderr: &str) -> Failure {
    Failure::of(&Captured::new(
        TOOL,
        Exit::new(Some(1)),
        Vec::new(),
        stderr.lines().map(str::to_owned).collect(),
    ))
}

/// The project `stack` of `compose.yaml`, under profile `access`.
fn stack<'a>(files: &'a [PathBuf], profiles: &'a [String]) -> Project<'a> {
    Project {
        name: "stack",
        files,
        pins: None,
        profiles,
    }
}

/// The engine's own failures, as the Docker CLI prints them (24 to 28, Linux and Docker
/// Desktop on Windows): told apart, a refused account isn't a stopped engine.
#[test]
fn an_engine_down_or_refusing_and_a_pool_overlap_read_as_what_they_are() {
    let kind = |f: Failure| match f {
        Failure::EngineDown(_) => "down",
        Failure::EngineDenied(_) => "denied",
        Failure::PoolOverlap(_) => "overlap",
        _ => "other",
    };
    for (said, expected) in [
        (include_str!("../../tool_output/fixtures/compose/27.1-engine-down.txt"), "down"),
        (include_str!("../../tool_output/fixtures/compose/27.1-windows-engine-gone.txt"), "down"),
        (include_str!("../../tool_output/fixtures/compose/29.9-engine-stopped-during-up.txt"), "down"),
        (include_str!("../../tool_output/fixtures/compose/28.0-socket-denied.txt"), "denied"),
        (include_str!("../../tool_output/fixtures/compose/24.0-socket-denied.txt"), "denied"),
        (include_str!("../../tool_output/fixtures/compose/27.1-windows-pipe-denied.txt"), "denied"),
        (include_str!("../../tool_output/fixtures/compose/2.29-pool-overlaps.txt"), "overlap"),
        ("Access is denied.", "other"),
        ("Error response from daemon: pull access denied for x, repository does not exist", "other"),
    ] {
        assert_eq!(kind(failed(said)), expected, "{said}");
    }
}

#[test]
fn failures_read_as_compose_prints_them() {
    assert_eq!(
        failed(include_str!("../../tool_output/fixtures/compose/2.29-name-in-use.txt")),
        Failure::NameInUse
    );
    assert_eq!(
        failed(include_str!("../../tool_output/fixtures/compose/2.29-network-incorrect-label.txt")),
        Failure::NetworkExists
    );
    for (said, port) in [
        (include_str!("../../tool_output/fixtures/compose/2.29-port-allocated.txt"), Some((8080, Tcp))),
        (include_str!("../../tool_output/fixtures/compose/2.29-port-allocated-v6.txt"), Some((8180, Tcp))),
        (
            include_str!("../../tool_output/fixtures/compose/2.29-port-allocated-loopback.txt"),
            Some((8180, Tcp)),
        ),
        (
            include_str!("../../tool_output/fixtures/compose/29.9-bind-host-port-in-use.txt"),
            Some((18180, Tcp)),
        ),
        (
            include_str!("../../tool_output/fixtures/compose/29.9-bind-loopback-port-in-use.txt"),
            Some((18181, Tcp)),
        ),
        (
            include_str!("../../tool_output/fixtures/compose/4.30-desktop-listen-in-use.txt"),
            Some((8180, Tcp)),
        ),
        ("Error: listen udp 0.0.0.0:5353: bind: address already in use", Some((5353, Udp))),
        ("Error: port is already allocated", None),
    ] {
        assert_eq!(failed(said), Failure::PortTaken(port), "{said}");
    }
    for refused in [
        include_str!("../../tool_output/fixtures/compose/5.6-pull-access-denied.txt"),
        include_str!("../../tool_output/fixtures/compose/5.6-tag-not-found.txt"),
        include_str!("../../tool_output/fixtures/compose/2.29-pull-rate-limited.txt"),
    ] {
        assert!(matches!(failed(refused), Failure::PullRefused(_)), "{refused}");
    }
    for cut in [
        "Error response from daemon: Get \"https://registry-1.docker.io/v2/\": net/http: TLS handshake timeout",
        "Error response from daemon: Head \"https://registry-1.docker.io/v2/stack/web/manifests/1\": read: connection reset by peer",
        "Error response from daemon: received unexpected HTTP status: 502 Bad Gateway",
    ] {
        assert!(matches!(failed(cut), Failure::PullTransient(_)), "{cut}");
    }
    assert_eq!(
        failed("no such service: nope"),
        Failure::Other("no such service: nope (exited with code 1)".to_owned())
    );
}

#[tokio::test]
async fn a_call_names_the_project_its_files_and_its_profiles_before_the_verb() {
    let fake = FakeProcesses::script().expect("docker compose -p stack", Reply::ok("")).build();
    let files = [PathBuf::from("compose.yaml")];
    let profiles = ["access".to_owned()];
    up(&testing::call(&fake), &stack(&files, &profiles), &Up { wait: true, ..Up::default() })
        .await
        .expect("up");
    let [call] = fake.made("docker compose").try_into().expect("one call");
    assert_eq!(
        call.args,
        [
            "compose",
            "-p",
            "stack",
            "-f",
            "compose.yaml",
            "--profile",
            "access",
            "up",
            "--detach",
            "--wait"
        ]
    );
}

#[tokio::test]
async fn a_build_up_asks_compose_to_build_before_it_starts() {
    let fake = FakeProcesses::script().expect("docker compose -p stack", Reply::ok("")).build();
    let files = [PathBuf::from("compose.yaml")];
    up(
        &testing::call(&fake),
        &stack(&files, &[]),
        &Up {
            existing: Existing::Rebuilt,
            ..Up::default()
        },
    )
    .await
    .expect("up");
    let [call] = fake.made("docker compose").try_into().expect("one call");
    assert_eq!(call.args.last().map(String::as_str), Some("--build"), "{:?}", call.args);
}

#[tokio::test]
async fn a_ps_listing_that_isnt_json_containers_is_unexpected_never_empty() {
    let fake = FakeProcesses::script()
        .expect(
            "docker compose -p stack -f compose.yaml ps --all --format json",
            Reply::ok("{\"Service\": \"web\", \"State\""),
        )
        .build();
    let files = [PathBuf::from("compose.yaml")];
    let read = ps(&testing::call(&fake), &stack(&files, &[])).await;
    assert!(matches!(read, Err(CliError::Unexpected { tool: TOOL, .. })), "{read:?}");
}

#[test]
fn a_console_execs_with_a_terminal_and_a_command_without_one() {
    let fake = FakeProcesses::script().build();
    let files = [PathBuf::from("compose.yaml")];
    let project = stack(&files, &[]);
    let call = testing::call(&fake);
    for (tty, user, expected) in [
        (true, Some("root"), &["exec", "--user", "root", "web", "sh"][..]),
        (false, None, &["exec", "-T", "web", "sh"][..]),
    ] {
        let what = Exec {
            service: "web",
            user,
            argv: &["sh".to_owned()],
            tty,
        };
        let spec = exec_spec(&call, &project, &what);
        let args: Vec<&str> = spec.args.iter().map(String::as_str).collect();
        assert_eq!(args.get(5..), Some(expected), "tty {tty}: {args:?}");
    }
}

#[tokio::test]
async fn a_compose_older_than_2_17_is_too_old_and_one_saying_no_version_is_used_unchecked() {
    for (said, too_old) in [("2.29.7\n", false), ("v2.17.0", false), ("2.16.9-desktop.1", true)] {
        let fake = FakeProcesses::script().expect("docker compose version --short", Reply::ok(said)).build();
        let read = version(&testing::call(&fake)).await;
        if too_old {
            assert!(matches!(read, Err(CliError::TooOld { ref min, .. }) if min == MIN_VERSION), "{said}: {read:?}");
        } else {
            assert!(matches!(read, Ok(Some(_))), "{said}: {read:?}");
        }
    }
    let empty = FakeProcesses::script().expect("docker compose version --short", Reply::ok("")).build();
    assert!(matches!(version(&testing::call(&empty)).await, Ok(None)), "no version is used, unchecked");
}
