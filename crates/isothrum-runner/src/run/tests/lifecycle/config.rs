//! What a start and an update do with the config Compose prints: an unreadable one fails the
//! port checks before anything starts and removes no network; the networks it no longer defines go.

use super::*;

/// Sets the fake engine's project networks and whether its config can be read.
fn engine_with(f: &Fixture, networks: &[&str], unreadable_config: bool) {
    let mut e = f.engine.lock().unwrap_or_else(PoisonError::into_inner);
    e.networks = networks.iter().map(|&n| n.to_owned()).collect();
    e.unreadable_config = unreadable_config;
}

/// A config Compose prints that can't be read fails the free-ports check: its ports would
/// otherwise pass unchecked. Nothing starts.
#[tokio::test]
async fn an_unreadable_config_fails_the_free_ports_check_before_anything_starts() {
    let f = fixture_answering(WEB_PORTAL, unreadable_once_pinned);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert!(matches!(&err, crate::Error::ToolFailed { step, .. } if step == "ports-free"), "{err}");
    assert_eq!(f.called(" up --detach "), 0, "nothing started");
}

/// The run's Docker, its config unreadable once the run's ports are pinned.
fn unreadable_once_pinned(engine: &Mutex<Engine>, call: &Call) -> Reply {
    if call.line().contains(" config --format json") && pinned(call).is_some() {
        return Reply::ok("not json");
    }
    answer(engine, call)
}

/// A fresh run whose config can't be read fails the pinning step rather than start on ephemeral
/// ports a resume would not get back. Nothing starts.
#[tokio::test]
async fn an_unreadable_config_fails_the_pinning_of_a_fresh_run() {
    let f = fixture();
    engine_with(&f, &[], true);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert!(matches!(&err, crate::Error::ToolFailed { step, .. } if step == "pin-ports"), "{err}");
    assert_eq!(f.called(" up --detach "), 0, "nothing started");
}

/// The networks the config no longer defines go; one it defines stays.
#[tokio::test]
async fn a_stale_network_goes_and_a_defined_one_stays() {
    let f = fixture();
    engine_with(&f, &["web-portal_lab", "web-portal_old"], false);
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(f.called("docker network rm web-portal_old"), 1);
    assert_eq!(f.called("docker network rm web-portal_lab"), 0);
}

/// An update whose config can't be read removes no network: read as empty, every live network of
/// the project would be stale. The step fails (tolerated by the manifest, so a warning).
#[tokio::test]
async fn an_update_with_an_unreadable_config_removes_no_network() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    engine_with(&f, &["web-portal_lab", "web-portal_old"], true);
    let events = Recorder::new();
    run.update(&opts(), &events).await.unwrap();
    assert_eq!(f.called("docker network rm"), 0, "no network removed");
    assert!(kinds(&events).iter().any(|k| matches!(
        k,
        EventKind::Warning { code: crate::WarningCode::StepTolerated, message } if message.contains("networks it defines")
    )));
}
