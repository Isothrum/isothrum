//! What an embedder shares between threads: the runner's types are `Send` and `Sync`, and an
//! operation's future is `Send` (an embedder spawns operations onto its own runtime).

use std::error::Error;

use isothrum_runner::{Event, ExecReport, Park, RunOptions, Runner, RunnerConfig, Running, Started, Status, SystemProcesses};

/// A test's outcome: a failed step is its report.
type Outcome<T = ()> = Result<T, Box<dyn Error>>;

fn send_sync<T: Send + Sync>() {}

fn send<T: Send>(_: &T) {}

#[test]
fn the_types_an_embedder_shares_between_threads_are_send_and_sync() {
    send_sync::<Runner>();
    send_sync::<RunnerConfig>();
    send_sync::<Running<'_>>();
    send_sync::<RunOptions>();
    send_sync::<Event>();
    send_sync::<isothrum_runner::Error>();
    send_sync::<Status>();
    send_sync::<Started>();
    send_sync::<ExecReport>();
    send_sync::<SystemProcesses>();
}

#[test]
fn an_operation_can_run_on_any_thread() -> Outcome {
    let home = tempfile::tempdir()?;
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("compose.yaml"), "services:\n  web:\n    image: nginx\n")?;
    let runner = Runner::new(RunnerConfig::new(home.path()))?;
    let run = runner.open(dir.path(), None)?;
    let (opts, events) = (RunOptions::new(), |_: &Event| {});
    let argv = ["true".to_owned()];
    send(&run.up(&opts, &events));
    send(&run.down(&opts, &events));
    send(&run.park(Park::Pause, &opts, &events));
    send(&run.resume(&opts, &events));
    send(&run.exec("web", &argv, &opts, &events));
    send(&run.status());
    Ok(())
}
