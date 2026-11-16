//! Real processes: each one a regression a runtime met. A tool cut short, asked to stop and
//! ended is in `stopping`.

use std::time::Duration;

use isothrum_test_support::PidGuard;

use super::*;
use crate::process::capture;

mod stopping;

fn spec(program: &str, args: &[&str]) -> ProcessSpec {
    ProcessSpec::new(program, args.iter().copied())
}

/// Regression (#124): a tool started in a folder that isn't there was "not installed": the OS
/// refuses the start with `NotFound` for the folder as for the program.
#[tokio::test]
async fn a_missing_working_folder_is_not_a_missing_tool() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("no such folder");
    let program = if cfg!(windows) { "cmd" } else { "true" };
    let mut job = spec(program, if cfg!(windows) { &["/C", "exit 0"] } else { &[] });
    job.cwd = Some(gone.clone());
    let err = capture(&SystemProcesses::default(), &job).await.unwrap_err();
    assert!(matches!(err, ProcessError::Io { .. }), "{err:?}");
    let said = err.to_string();
    assert!(said.contains(&gone.display().to_string()) && said.contains("working folder"), "{said}");
}

#[tokio::test]
async fn a_missing_tool_is_named() {
    let err = capture(&SystemProcesses::default(), &spec("isothrum-no-such-tool", &[])).await.unwrap_err();
    assert!(
        matches!(err, ProcessError::Missing { ref program } if program == "isothrum-no-such-tool"),
        "{err:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn output_that_isnt_utf8_is_read_lossily_and_the_tool_runs_on() {
    // Regression: the first byte that wasn't UTF-8 failed the stream, which killed the tool.
    let script = r"printf 'Cr\351ation\r\n'; printf 'err \377\n' >&2; sleep 0.2; printf 'done'";
    let out = capture(&SystemProcesses::default(), &spec("sh", &["-c", script])).await.unwrap();
    assert_eq!(out.stdout, ["Cr\u{fffd}ation", "done"]);
    assert_eq!(out.stderr_tail, ["err \u{fffd}"]);
    assert!(out.success());
}

#[cfg(unix)]
#[tokio::test]
async fn a_tool_that_exited_is_not_waited_for_past_what_it_left_holding_its_output() {
    // Regression: a process the tool left behind (an ssh ControlMaster) held its pipes open, and
    // a finished `terraform destroy` hung the run, and its lock, for good.
    // What it leaves runs five minutes: a run that waited for it fails the test's deadline long
    // before, and the guard ends it either way.
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left");
    let script = format!("sleep 300 & echo $! > '{}'; printf 'done'", left.display());
    let run = tokio::time::timeout(Duration::from_secs(120), capture(&SystemProcesses::default(), &spec("sh", &["-c", &script]))).await;
    let _left = PidGuard::from_file(&left);
    let out = run.expect("the run ended with its tool, not with what the tool left").unwrap();
    assert_eq!(out.stdout, ["done"]);
}

#[cfg(unix)]
#[tokio::test]
async fn a_failure_keeps_its_exit_code_and_last_lines() {
    let out = capture(
        &SystemProcesses::default(),
        &spec("sh", &["-c", "for i in $(seq 1 12); do echo e$i >&2; done; exit 4"]),
    )
    .await
    .unwrap();
    assert_eq!(out.exit, Exit { code: Some(4) });
    assert_eq!(out.stderr_tail.first().map(String::as_str), Some("e3"));
    assert_eq!(out.stderr_tail.len(), 10);
}

#[cfg(unix)]
#[tokio::test]
async fn stdin_comes_from_a_file_and_env_and_cwd_are_the_specs() {
    use crate::events::Stream;
    // Isoloom's exec check runners are piped into `docker compose exec -T … sh -s` this way.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("in.sh"), "echo \"$GREETING from $(basename \"$PWD\")\"\n").unwrap();
    let mut job = spec("sh", &["-s"]);
    job.stdin = Some(dir.path().join("in.sh"));
    job.env = vec![("GREETING".into(), "hello".into())];
    job.cwd = Some(dir.path().to_path_buf());
    let mut lines = Vec::new();
    let exit = SystemProcesses::default()
        .run(&job, &mut |s: Stream, l: &str| {
            lines.push((s, l.to_owned()));
        })
        .await
        .unwrap();
    let name = dir.path().file_name().unwrap().to_string_lossy().to_string();
    assert!(exit.success());
    assert_eq!(lines, [(Stream::Stdout, format!("hello from {name}"))]);
}

#[test]
fn only_the_windows_wrappers_go_through_cmd() {
    let az = spec("az", &["account", "show"]);
    let (program, args) = wrapped(&az);
    if cfg!(windows) {
        assert_eq!((program, args), ("cmd", vec!["/C", "az", "account", "show"]));
    } else {
        assert_eq!((program, args), ("az", vec!["account", "show"]));
    }
    assert_eq!(wrapped(&spec("docker", &["ps"])), ("docker", vec!["ps"]));
}

#[tokio::test]
async fn an_argument_cmd_would_interpret_is_refused_before_anything_starts() {
    let err = capture(&SystemProcesses::default(), &spec("az", &["group", "create", "--name", "x & calc"]))
        .await
        .unwrap_err();
    assert!(matches!(err, ProcessError::Refused { .. }), "{err:?}");
}

#[test]
fn the_docker_cli_config_is_read_from_docker_config_before_the_home() {
    let (env_dir, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let context = |name: &str| format!(r#"{{"currentContext":"{name}"}}"#);
    std::fs::create_dir_all(home.path().join(".docker")).unwrap();
    std::fs::write(home.path().join(".docker/config.json"), context("colima")).unwrap();
    std::fs::write(env_dir.path().join("config.json"), context("default")).unwrap();
    let env = env_dir.path().display().to_string();
    let with = |docker_config: Option<&str>| {
        SystemProcesses::for_host(
            |v| (v == "DOCKER_CONFIG").then(|| docker_config.map(std::ffi::OsString::from)).flatten(),
            Some(home.path()),
        )
    };
    assert!(with(None).strip_engine_overrides, "the home's config picks a context");
    assert!(!with(Some(&env)).strip_engine_overrides, "$DOCKER_CONFIG's config comes first");
    assert!(!SystemProcesses::for_host(|_| None, None).strip_engine_overrides, "no config: nothing picked");
}

/// Regression: a `DOCKER_CONFIG` that isn't UTF-8 was read as unset, and the home's config,
/// another one, was read instead.
#[cfg(unix)]
#[test]
fn a_docker_config_that_isnt_utf8_is_still_the_one_read() {
    use std::os::unix::ffi::OsStringExt;
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".docker")).unwrap();
    std::fs::write(home.path().join(".docker/config.json"), r#"{"currentContext":"colima"}"#).unwrap();
    let elsewhere = std::ffi::OsString::from_vec(b"/nowhere/bad\xff".to_vec());
    let read = SystemProcesses::for_host(|v| (v == "DOCKER_CONFIG").then(|| elsewhere.clone()), Some(home.path()));
    assert!(!read.strip_engine_overrides, "the home's config isn't the one DOCKER_CONFIG names");
}

/// A Docker CLI config that is there but can't be read, or isn't JSON, read as "no context
/// picked" without a word: the inherited `DOCKER_HOST` stayed, and nothing said why.
#[tokio::test]
async fn a_docker_cli_config_that_cant_be_read_or_isnt_json_is_traced() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let docker_config = dir.path().as_os_str().to_owned();
    let read = || SystemProcesses::for_host(|v| (v == "DOCKER_CONFIG").then(|| docker_config.clone()), None);
    let (none, quiet) = crate::testing::traced(async { read() }).await;
    assert!(!none.strip_engine_overrides && quiet.is_empty(), "none there is fine: {quiet}");
    std::fs::create_dir(&config).unwrap();
    let (unreadable, traced) = crate::testing::traced(async { read() }).await;
    assert!(!unreadable.strip_engine_overrides);
    assert!(
        traced.contains("WARN") && traced.contains("config couldn't be read") && traced.contains("config.json"),
        "{traced}"
    );
    std::fs::remove_dir(&config).unwrap();
    std::fs::write(&config, r#"{"currentContext": "colima""#).unwrap();
    let (cut_short, not_json) = crate::testing::traced(async { read() }).await;
    assert!(!cut_short.strip_engine_overrides);
    assert!(not_json.contains("WARN") && not_json.contains("config isn't JSON"), "{not_json}");
}

/// A tool run with a folder to record it in is there while it runs (it lists its own entry,
/// named by its process id), and gone once it exited.
#[cfg(unix)]
#[tokio::test]
async fn a_recorded_tool_is_in_the_runs_tools_while_it_runs_and_gone_after() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(crate::process::ledger::DIR);
    // The entry is written once the tool started: it waits for it (a minute at most, for a
    // loaded host), then lists.
    let script = "echo \"$$\"; i=0; while [ $i -lt 600 ] && ! ls \"$0\" | grep -q -e \"-$$.tool\"; do sleep 0.1; i=$((i+1)); done; ls \"$0\"";
    let mut listing = spec("sh", &["-c", script, dir.to_str().unwrap()]);
    listing.ledger_dir = Some(dir.clone());
    let out = capture(&SystemProcesses::default(), &listing).await.unwrap();
    assert!(out.success(), "{out:?}");
    let pid = &out.stdout[0];
    assert!(
        out.stdout.iter().any(|l| l.ends_with(&format!("-{pid}.tool"))),
        "the tool's own entry, named by its id: {:?}",
        out.stdout
    );
    let tools: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "tool"))
        .collect();
    assert!(tools.is_empty(), "{tools:?}");
}

/// A tool runs in the C locale for its messages, numbers and times (what `tool_output` reads),
/// whatever the user's session speaks, and the spec's own variables are set after it, so a run
/// that names one of these still decides it.
#[cfg(unix)]
#[test]
fn a_tool_starts_in_the_c_locale_and_the_specs_variables_come_after() {
    let mut job = spec("vagrant", &["status"]);
    job.env = vec![("LC_TIME".into(), "en_GB.UTF-8".into())];
    let env = SystemProcesses::default().launch(&job).unwrap().env;
    let at = |name: &str, value: Option<&str>| {
        env.iter()
            .position(|(k, v)| k == name && v.as_deref() == value.map(std::ffi::OsStr::new))
            .unwrap_or_else(|| panic!("{name}={value:?} in {env:?}"))
    };
    for category in ["LC_MESSAGES", "LC_NUMERIC"] {
        at(category, Some("C"));
    }
    at("LC_ALL", None);
    assert!(at("LC_TIME", Some("C")) < at("LC_TIME", Some("en_GB.UTF-8")), "the spec's own last: {env:?}");
}
