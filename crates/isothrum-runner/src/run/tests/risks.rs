//! The launcher's runtime risks (runner-extraction.md §6), each pinned by a test that failed on
//! its old behaviour, and the hooks around lifecycle edges.

use super::*;
use crate::hooks::{HookError, Hooks, RunView};
use crate::{BoxFuture, ErrorCode, Park};

#[tokio::test]
async fn a_run_whose_lock_cant_be_taken_never_starts() {
    // R2: the launcher's run lock failed open, and the operation ran unlocked.
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    std::fs::create_dir_all(f.runner.store.lock_path(run.name())).unwrap();
    let events = Recorder::new();
    let err = run.up(&opts(), &events).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Io);
    assert!(f.fake.calls().is_empty(), "no tool ran: {:?}", f.fake.lines());
    assert!(matches!(kinds(&events).as_slice(), [EventKind::Error { code: ErrorCode::Io, .. }]));
}

#[tokio::test]
async fn a_down_replays_the_params_the_run_was_started_with() {
    // R2: a destroy used the profile's current settings (region, credentials), not the applied ones.
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let mut now = RunOptions::default();
    now.params.insert("ISOLOOM_PUBLISH_ADDRESS".into(), "0.0.0.0".into());
    run.down(&now, &Recorder::new()).await.unwrap();
    let down = f.fake.last(" down --volumes").unwrap();
    assert_eq!(down.env("ISOLOOM_PUBLISH_ADDRESS"), Some("127.0.0.1"));
}

#[tokio::test]
async fn a_pinned_port_another_run_is_picking_is_never_handed_out_twice() {
    // R9: ports were picked, then released before Compose bound them, so two starts could pin
    // the same one. The OS here hands 41873 out twice; the runner's reservation refuses the second.
    let f = fixture();
    let other = crate::RunName::new("other-run").unwrap();
    assert!(
        f.runner.reserve(&other, 41873, isoloom_manifest::Protocol::Tcp),
        "another run starting meanwhile"
    );
    let run = f.runner.open(&f.manifest, None).unwrap();
    let started = run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(started.outputs[0].value.as_deref(), Some("http://127.0.0.1:41874/"));
}

#[tokio::test]
async fn a_port_held_on_the_host_is_refused_before_anything_starts() {
    let f = fixture();
    f.ports.take(8080, isoloom_manifest::Protocol::Tcp);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let mut fixed = opts();
    fixed.params.insert("ISOLOOM_PUBLISH_FIXED".into(), "1".into());
    let err = run.up(&fixed, &Recorder::new()).await.unwrap_err();
    assert_eq!(err.to_string(), "host port 8080/tcp is in use");
    assert_eq!(f.called(" up --detach "), 0);
}

#[tokio::test]
async fn a_run_recorded_elsewhere_is_refused() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    let mut here = opts();
    here.host_label = Some("pve-run".into());
    run.up(&here, &Recorder::new()).await.unwrap();
    here.host_label = Some("other-server".into());
    assert_eq!(run.down(&here, &Recorder::new()).await.unwrap_err().code(), ErrorCode::RunningElsewhere);
}

/// Hooks that record their calls, failing after `up`.
#[derive(Default)]
struct Sidecar(std::sync::Mutex<Vec<String>>);

impl Sidecar {
    fn note(&self, what: &str, run: &RunView<'_>) {
        self.0
            .lock()
            .unwrap()
            .push(format!("{what} {} {}", run.name, run.networks[0].native.clone().unwrap_or_default()));
    }
}

impl Hooks for Sidecar {
    fn after_up<'a>(&'a self, run: &'a RunView<'a>) -> BoxFuture<'a, Result<(), HookError>> {
        self.note("after_up", run);
        Box::pin(async { Err(HookError::from("the attack box didn't start")) })
    }

    fn before_park<'a>(&'a self, run: &'a RunView<'a>) -> BoxFuture<'a, Result<(), HookError>> {
        self.note("before_park", run);
        Box::pin(async { Ok(()) })
    }

    fn after_resume<'a>(&'a self, run: &'a RunView<'a>) -> BoxFuture<'a, Result<(), HookError>> {
        self.note("after_resume", run);
        Box::pin(async { Ok(()) })
    }

    fn before_down<'a>(&'a self, run: &'a RunView<'a>) -> BoxFuture<'a, Result<(), HookError>> {
        self.note("before_down", run);
        Box::pin(async { Ok(()) })
    }
}

/// The fixture, its runner with a [`Sidecar`] as its hooks.
fn with_sidecar() -> (Fixture, Arc<Sidecar>) {
    let mut f = fixture();
    let sidecar = Arc::new(Sidecar::default());
    let hooks: Arc<dyn Hooks> = Arc::<Sidecar>::clone(&sidecar);
    f.runner.add_hooks(hooks);
    (f, sidecar)
}

/// Whether `events` hold the warning of a failed hook whose message starts with `start`.
fn hook_failed(events: &Recorder, start: &str) -> bool {
    kinds(events)
        .iter()
        .any(|k| matches!(k, EventKind::Warning { code, message } if *code == crate::WarningCode::HookFailed && message.starts_with(start)))
}

#[tokio::test]
async fn hooks_go_around_the_lifecycle_and_their_failure_is_a_warning() {
    let (f, sidecar) = with_sidecar();
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    assert!(hook_failed(&events, "after_up: the attack box"));
    run.park(Park::Shutdown, &Recorder::new()).await.unwrap();
    run.resume(&RunOptions::default(), &Recorder::new()).await.unwrap();
    run.down(&RunOptions::default(), &Recorder::new()).await.unwrap();
    let calls = sidecar.0.lock().unwrap().clone();
    let expected = ["after_up", "before_park", "after_resume", "before_down"].map(|e| format!("{e} flag-portal flag-portal_lab"));
    assert_eq!(calls, expected);
}

/// An update brings the run up, so `after_up` runs after it too (an embedder's sidecar rejoins a
/// run whose machines or networks may have changed); its failure is the update's warning.
#[tokio::test]
async fn after_up_runs_after_an_update_too() {
    let (f, sidecar) = with_sidecar();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let events = Recorder::new();
    run.update(&opts(), &events).await.unwrap();
    assert!(
        kinds(&events)
            .iter()
            .any(|k| matches!(k, EventKind::Warning { code, message } if *code == crate::WarningCode::HookFailed && message.starts_with("after_up: ")))
    );
    let calls = sidecar.0.lock().unwrap().clone();
    assert_eq!(calls, ["after_up flag-portal flag-portal_lab"; 2], "once after the up, once after the update");
}

#[tokio::test]
async fn an_op_enters_the_flight_map_only_under_the_run_lock() {
    // R1: `operate` marked the op in flight (and its guard freed the op's port reservations on
    // drop) BEFORE taking the per-run lock, so two overlapping ops on one run clobbered each
    // other's in-flight entry and freed each other's reservations. Entering now happens under the
    // lock, so an op still waiting for the lock is not yet in flight and its guard cannot outlive it.
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    let name = run.name().clone();
    // Hold the run's lock, as another operation on the same run would (whose `operate` created the
    // run's state folder before it locked).
    let lock_path = f.runner.store.lock_path(&name);
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let held = f.runner.locks.acquire(&name, &lock_path).await.unwrap();
    let o = opts();
    let events = Recorder::new();
    let mut up = std::pin::pin!(run.up(&o, &events));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), up.as_mut()).await.is_err(),
        "up must wait for the held run lock, not run",
    );
    assert!(
        f.runner.in_flight().is_empty(),
        "an op still waiting for the lock has not marked itself in flight: {:?}",
        f.runner.in_flight(),
    );
    drop(held);
    up.await.unwrap();
    assert!(f.runner.in_flight().is_empty(), "the op cleared itself once it finished");
}
