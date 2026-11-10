//! Ready conditions through the fake engine: a `tcp` one waits for its port, says once that it
//! waits, and one whose port isn't a number is the manifest's mistake.

use super::*;

/// The fixture with a `tcp` condition on the loopback's `port`, waited for 10 s.
fn with_tcp(port: &str) -> String {
    let mut m: serde_json::Value = serde_json::from_str(WEB_PORTAL).unwrap();
    let condition = serde_json::json!({ "kind": "tcp", "address": "127.0.0.1", "port": port, "timeout_s": 10 });
    m["ready"].as_array_mut().unwrap().push(condition);
    m.to_string()
}

/// A `tcp` ready condition holds once its address answers, and fails a start when it never does.
#[tokio::test(start_paused = true)]
async fn a_tcp_condition_holds_once_its_port_answers() {
    let manifest = with_tcp("${published.web.portal}");
    let silent = fixture_with(&manifest);
    let unanswered = silent.runner.open(&silent.manifest, None).unwrap();
    let silent_events = Recorder::new();
    let err = unanswered.up(&opts(), &silent_events).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::NotReady, "{err}");

    // Waiting is said once, with what it waits for, not every try.
    let waited: Vec<String> = kinds(&silent_events)
        .into_iter()
        .filter_map(|k| match k {
            EventKind::Ready {
                state: crate::ReadyState::Waiting,
                detail,
                ..
            } => Some(detail),
            _ => None,
        })
        .collect();
    assert_eq!(waited, ["no answer yet; trying every 2s for 10s"]);

    let f = fixture_with(&manifest);
    f.ports.answer("127.0.0.1", 41873);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    assert!(kinds(&events).contains(&EventKind::Ready {
        condition: "tcp 127.0.0.1:41873".into(),
        state: crate::ReadyState::Ok,
        detail: String::new(),
    }));
}

/// Regression: `${host.<machine>}` was the loopback whatever address the manifest publishes on,
/// so a run published on a LAN address was checked, and reported, where nothing listens. It is
/// the manifest's `publish.address`, filled; one on every interface answers at the loopback.
#[tokio::test(start_paused = true)]
async fn a_published_host_is_the_address_the_manifest_publishes_on() {
    let mut m: serde_json::Value = serde_json::from_str(WEB_PORTAL).unwrap();
    let condition = serde_json::json!({ "kind": "tcp", "address": "${host.web}", "port": "${published.web.portal}", "timeout_s": 10 });
    m["ready"].as_array_mut().unwrap().push(condition);
    let manifest = m.to_string();
    for (address, host) in [("192.0.2.7", "192.0.2.7"), ("0.0.0.0", "127.0.0.1")] {
        let f = fixture_with(&manifest);
        f.ports.answer(host, 41873);
        let run = f.runner.open(&f.manifest, None).unwrap();
        let events = Recorder::new();
        run.up(&opts().with_param("ISOLOOM_PUBLISH_ADDRESS", address), &events).await.unwrap();
        assert!(
            kinds(&events).contains(&EventKind::Ready {
                condition: format!("tcp {host}:41873"),
                state: crate::ReadyState::Ok,
                detail: String::new(),
            }),
            "{address}: {:?}",
            kinds(&events)
        );
    }
}

/// A `tcp` condition whose port isn't a number is the manifest's mistake, not a run that isn't
/// ready.
#[tokio::test(start_paused = true)]
async fn a_tcp_condition_on_no_port_is_manifest_invalid() {
    let f = fixture_with(&with_tcp("http"));
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::ManifestInvalid, "{err}");
    assert_eq!(err.to_string(), "ready.tcp.port: `http` isn't a port");
}

/// Regression: a failed `step_ok` said what the step's success means in its `ready` event, and
/// "the step didn't succeed" in its error: one failure said two ways.
#[tokio::test(start_paused = true)]
async fn a_failed_step_ok_says_the_same_in_its_event_and_its_error() {
    let mut m: serde_json::Value = serde_json::from_str(WEB_PORTAL).unwrap();
    for step in m["lifecycle"]["up"].as_array_mut().unwrap() {
        if step["id"] == "job-isoloom-init-web-1" {
            step["failure"] = "tolerate".into();
        }
    }
    let f = fixture_with(&m.to_string());
    f.engine.lock().unwrap().failing_job = true;
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    let err = run.up(&opts(), &events).await.unwrap_err();
    let crate::Error::NotReady { condition, detail } = &err else {
        panic!("{err}")
    };
    assert_eq!(condition, "step_ok job-isoloom-init-web-1");
    let said = kinds(&events).into_iter().find_map(|k| match k {
        EventKind::Ready {
            condition: c,
            state: crate::ReadyState::Failed,
            detail: d,
        } if c == *condition => Some(d),
        _ => None,
    });
    assert_eq!(said.as_deref(), Some(detail.as_str()));
    assert!(
        detail.starts_with("the step didn't succeed") && detail.contains("isoloom-init-web-1) exited 0"),
        "{detail}"
    );
}
