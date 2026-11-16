//! What an operation leaves of a run on disk: a down none of its machines' pinned host keys, a
//! query nothing at all, an operation refused for the lock the run's log as it was.

use super::*;

/// Regression: only the Terraform driver forgot the run's pinned host keys, so a Compose or
/// Vagrant run brought up again refused its recreated machines' new keys at the same addresses.
#[tokio::test]
async fn a_down_forgets_the_runs_pinned_host_keys_whatever_its_format() {
    let (_root, project, runner, _engine) = setup();
    let run = runner.open(&project, None).expect("opened");
    run.up(&RunOptions::new(), &Recorder::new()).await.expect("up");
    let known = runner.store.run_dir_path(run.name()).join(crate::ssh::KNOWN_HOSTS);
    std::fs::write(&known, "10.0.0.9 ssh-ed25519 AAAAold\n").expect("a pinned key");
    run.down(&RunOptions::new(), &Recorder::new()).await.expect("down");
    assert!(!known.exists(), "the pins go with the machines");
}

/// Regression: the console query made the run's state folder, though a container's console
/// writes nothing there; a run never started was left a folder.
#[tokio::test]
async fn a_console_query_of_a_run_never_started_leaves_no_state_folder() {
    let (_root, project, runner, _engine) = setup();
    let run = runner.open(&project, None).expect("opened");
    let console = run.console_command("web", None).await.expect("a console");
    assert_eq!(console.program, "docker");
    assert!(!runner.store.run_dir_path(run.name()).exists(), "no state folder for a query");
}

/// Regression: the log was trimmed before the run was locked, so an operation refused as
/// `locked` trimmed it under the one holding the lock.
#[tokio::test(start_paused = true)]
async fn an_operation_refused_for_the_lock_leaves_the_runs_log_whole() {
    let (_root, project, runner, _engine) = setup();
    let run = runner.open(&project, None).expect("opened");
    let kept = crate::store::EVENTS_KEPT + 1;
    runner
        .store
        .db()
        .write(|tx| {
            (0..kept).try_for_each(|_| {
                tx.append_raw_event(Some(run.name().as_str()), None, "{\"from\": \"another version\"}")
                    .map(drop)
            })
        })
        .expect("a long log");
    std::fs::create_dir_all(runner.store.run_dir_path(run.name())).expect("the run's folder");
    let other = std::fs::File::create(runner.store.lock_path(run.name())).expect("the lock file");
    other.lock().expect("held by another");
    let refused = run.up(&RunOptions::new(), &Recorder::new()).await.expect_err("locked");
    assert_eq!(refused.code(), ErrorCode::Locked, "{refused}");
    let logged = runner.events(run.name()).expect("the log");
    assert_eq!(logged.unreadable, kept, "every row kept");
}
