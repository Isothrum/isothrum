//! The commands an embedder drives (the launcher, the desktop app, a CI job) as the binary runs
//! them, on a stand-in `docker`: each one's exit code, and with `--json` what it prints, read as
//! JSON. What each operation does with the tools is the runner's tests'; here, what the command
//! line makes of it.
#![cfg(unix)]

mod common;

use common::{Lab, Outcome, json_lines, last_event};
use serde_json::{Value, json};

/// `value[key]`, or null.
fn at<'v>(value: &'v Value, key: &str) -> &'v Value {
    value.get(key).unwrap_or(&Value::Null)
}

/// `isothrum <args>` in `lab`: its exit code and its JSON lines.
fn run(lab: &Lab, args: &[&str]) -> Outcome<(Option<i32>, Vec<Value>)> {
    let out = lab.isothrum(args)?;
    Ok((out.status.code(), json_lines(&out)?))
}

/// The run's state, as `status --json` says it.
fn state(lab: &Lab) -> Outcome<Value> {
    let (code, lines) = run(lab, &["status", "--json", lab.dir()])?;
    assert_eq!((code, lines.len()), (Some(0), 1), "{lines:?}");
    Ok(at(lines.first().unwrap_or(&Value::Null), "state").clone())
}

/// What an operation's last event says: its kind, and its `result`'s `data.state` or its
/// `error`'s `code`.
fn outcome(events: &[Value]) -> (Option<&str>, Option<&str>) {
    let last = events.last().unwrap_or(&Value::Null);
    let said = at(at(last, "data"), "state").as_str().or(at(last, "code").as_str());
    (at(last, "event").as_str(), said)
}

#[test]
fn up_park_resume_and_down_exit_0_and_end_with_their_result() -> Outcome<()> {
    let lab = Lab::compose()?;
    for (args, state_after) in [
        (["up", "--json"], "ready"),
        (["park", "--json"], "parked"),
        (["resume", "--json"], "ready"),
        (["down", "--json"], "absent"),
    ] {
        let (code, events) = run(&lab, &[&args[..], &[lab.dir()]].concat())?;
        assert_eq!(code, Some(0), "{args:?}: {events:?}");
        assert_eq!(outcome(&events), (Some("result"), Some(state_after)), "{args:?}");
        isothrum_test_support::assert_every!(events.iter(), |e: &&Value| at(e, "op") == args[0]);
        assert_eq!(state(&lab)?, state_after, "{args:?}");
    }
    assert!(!lab.docker_file("web").exists(), "`down` left the container in the engine");
    Ok(())
}

#[test]
fn status_with_json_is_one_document_naming_each_machine_and_its_state() -> Outcome<()> {
    let lab = Lab::compose()?;
    assert_eq!(run(&lab, &["up", "--json", lab.dir()])?.0, Some(0));
    let (code, lines) = run(&lab, &["status", "--json", lab.dir()])?;
    assert_eq!((code, lines.len()), (Some(0), 1), "{lines:?}");
    let status = lines.first().unwrap_or(&Value::Null);
    assert_eq!((at(status, "state"), at(status, "live")), (&json!("ready"), &json!(true)), "{status}");
    assert_eq!(at(status, "machines"), &json!([{ "name": "web", "state": "running", "published": [] }]));
    Ok(())
}

#[test]
fn an_operation_the_runs_state_refuses_exits_1_with_its_error_code() -> Outcome<()> {
    let lab = Lab::compose()?;
    let (code, events) = run(&lab, &["resume", "--json", lab.dir()])?;
    assert_eq!((code, outcome(&events)), (Some(1), (Some("error"), Some("nothing_to_resume"))));
    for args in [["up", "--json"], ["park", "--json"]] {
        assert_eq!(run(&lab, &[&args[..], &[lab.dir()]].concat())?.0, Some(0), "{args:?}");
    }
    let (again, refused) = run(&lab, &["park", "--json", lab.dir()])?;
    assert_eq!((again, outcome(&refused)), (Some(1), (Some("error"), Some("illegal_transition"))));
    assert_eq!(state(&lab)?, "parked", "a refused operation changes nothing");
    Ok(())
}

#[test]
fn a_tool_that_fails_exits_1_and_ends_the_stream_with_tool_failed_naming_the_step() -> Outcome<()> {
    let lab = Lab::compose()?;
    let out = lab
        .command()
        .args(["up", "--json", lab.dir()])
        .env("ISOTHRUM_TEST_DOCKER_FAILS", "up")
        .output()?;
    assert_eq!(out.status.code(), Some(1));
    let error = last_event(&out)?;
    assert_eq!(
        (at(&error, "event").as_str(), at(&error, "code").as_str(), at(&error, "step").as_str()),
        (Some("error"), Some("tool_failed"), Some("up")),
        "{error}"
    );
    let logged = json_lines(&out)?;
    assert!(
        logged
            .iter()
            .any(|e| at(e, "event") == "log" && at(e, "stream") == "stderr" && at(e, "line") == "Error response from daemon: up refused"),
        "the tool's own words are in the stream: {logged:?}"
    );
    Ok(())
}

#[test]
fn exec_exits_0_only_when_its_command_did_and_streams_its_output_as_log_events() -> Outcome<()> {
    let lab = Lab::compose()?;
    assert_eq!(run(&lab, &["up", "--json", lab.dir()])?.0, Some(0));
    let (code, events) = run(&lab, &["exec", "web", "--json", lab.dir(), "--", "sh", "-c", "echo out; echo err >&2; exit 3"])?;
    assert_eq!(code, Some(1), "{events:?}");
    let mut logs: Vec<(&str, &str)> = events
        .iter()
        .filter(|e| at(e, "event") == "log")
        .filter_map(|e| Some((at(e, "stream").as_str()?, at(e, "line").as_str()?)))
        .collect();
    // Two pipes: which line comes first is the scheduler's.
    logs.sort_unstable();
    assert_eq!(logs, [("stderr", "err"), ("stdout", "out")]);
    let result = events.last().cloned().unwrap_or_default();
    assert_eq!(
        (at(&result, "event"), at(&result, "ok"), at(at(&result, "data"), "machine")),
        (&json!("result"), &json!(false), &json!("web"))
    );

    let (passed, said) = run(&lab, &["exec", "web", "--json", lab.dir(), "--", "true"])?;
    assert_eq!((passed, said.last().map(|e| at(e, "ok"))), (Some(0), Some(&json!(true))));
    Ok(())
}

#[test]
fn key_with_json_prints_the_homes_public_key_as_one_document() -> Outcome<()> {
    let lab = Lab::empty()?;
    let ssh = lab.home.path().join("ssh");
    std::fs::create_dir_all(&ssh)?;
    std::fs::write(ssh.join("id_ed25519"), "private")?;
    std::fs::write(ssh.join("id_ed25519.pub"), "ssh-ed25519 AAAAfake isothrum\n")?;
    let (code, lines) = run(&lab, &["key", "--json"])?;
    assert_eq!((code, lines), (Some(0), vec![json!({ "public_key": "ssh-ed25519 AAAAfake isothrum" })]));
    Ok(())
}

#[test]
fn reap_with_nothing_expired_exits_0_with_an_empty_result() -> Outcome<()> {
    let lab = Lab::compose()?;
    assert_eq!(run(&lab, &["up", "--json", lab.dir()])?.0, Some(0));
    let (code, lines) = run(&lab, &["reap", "--json"])?;
    assert_eq!(code, Some(0), "{lines:?}");
    assert_eq!(outcome(&lines).0, Some("result"));
    let data = at(lines.last().unwrap_or(&Value::Null), "data");
    assert_eq!(data, &json!({ "destroyed": [], "pending": [], "failed": [] }));
    assert_eq!(state(&lab)?, "ready", "a run with no lifetime is never reaped");
    Ok(())
}
