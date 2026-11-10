//! A run's event log, trimmed only by the operation holding the run's lock.

use super::*;

/// Fills run `name`'s log with one line more than it keeps, and returns the log's path.
fn long_log(f: &Fixture, name: &crate::RunName) -> std::path::PathBuf {
    use std::io::Write;
    let path = f.runner.store.run_dir(name).unwrap().join("events.jsonl");
    let mut log = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    for seq in 0..=crate::store::EVENTS_KEPT {
        writeln!(log, "{{\"seq\":{seq}}}").unwrap();
    }
    log.flush().unwrap();
    path
}

#[tokio::test(start_paused = true)]
async fn an_operation_finding_the_run_locked_leaves_its_log_whole() {
    // Regression: the log was trimmed before the run was locked, so an operation refused as
    // `locked` replaced the log under the one holding the lock, whose later events were lost.
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    let log = long_log(&f, run.name());
    let other = std::fs::File::create(f.runner.store.lock_path(run.name())).unwrap();
    other.lock().unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::Locked, "{err}");
    let text = std::fs::read_to_string(&log).unwrap();
    assert_eq!(text.lines().count(), crate::store::EVENTS_KEPT + 2, "every line, then the `locked` error");
    assert!(text.starts_with("{\"seq\":0}\n"));
}

#[tokio::test]
async fn an_operation_trims_a_long_log_once_the_run_is_locked_and_logs_to_the_trimmed_one() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    long_log(&f, run.name());
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    let logged = f.runner.events(run.name()).unwrap();
    assert_eq!(logged.unreadable, crate::store::EVENTS_KEPT / 2, "the older lines' newer half");
    assert_eq!(logged.events, events.events(), "then every event of the operation");
}
