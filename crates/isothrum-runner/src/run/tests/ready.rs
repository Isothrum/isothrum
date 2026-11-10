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
