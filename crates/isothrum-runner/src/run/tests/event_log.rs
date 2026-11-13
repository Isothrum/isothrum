//! A run's events in the home's store, trimmed only by the operation holding the run's lock.

use super::*;

/// Fills run `name`'s events with one row more than it keeps, each one this runner can't read.
fn long_log(f: &Fixture, name: &crate::RunName) {
    f.runner
        .store
        .db()
        .write(|tx| {
            for seq in 0..=crate::store::EVENTS_KEPT {
                tx.append_raw_event(Some(name.as_str()), None, &format!("{{\"seq\":{seq}}}"))?;
            }
            Ok(())
        })
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_operation_finding_the_run_locked_leaves_its_events_whole() {
    // Regression: the log was trimmed before the run was locked, so an operation refused as
    // `locked` trimmed it under the one holding the lock.
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    long_log(&f, run.name());
    std::fs::create_dir_all(f.runner.store.run_dir_path(run.name())).unwrap();
    let other = std::fs::File::create(f.runner.store.lock_path(run.name())).unwrap();
    other.lock().unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::Locked, "{err}");
    let logged = f.runner.events(run.name()).unwrap();
    assert_eq!(
        (logged.unreadable, logged.events.len()),
        (crate::store::EVENTS_KEPT + 1, 1),
        "every row, then the `locked` error"
    );
}

#[tokio::test]
async fn an_operation_trims_long_events_once_the_run_is_locked_and_logs_after_them() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    long_log(&f, run.name());
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    let logged = f.runner.events(run.name()).unwrap();
    assert_eq!(logged.unreadable, crate::store::EVENTS_KEPT / 2, "the older rows' newer half");
    assert_eq!(logged.events, events.events(), "then every event of the operation");
}
