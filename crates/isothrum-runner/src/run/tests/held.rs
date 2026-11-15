//! A Vagrant run read while something else acts on it: an operation of another process that
//! moves no state (an attach provisioning its machine for minutes), and a Vagrant process outside
//! the runner holding a machine's lock. Its status never contends with them: the run as last
//! read, not live, and a machine another process acts on is busy.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use super::vagrant::{a_modelled_vagrant_lab, vagrant_answer};
use super::*;
use crate::process::fake::FakeProcesses;

/// What Vagrant 2.4.9 prints when another process holds machine `name`'s lock.
fn locked(name: &str) -> Reply {
    let said = include_str!("../../tool_output/fixtures/vagrant/2.4.9-error-machine-locked.txt").replace("Name: probe", &format!("Name: {name}"));
    Reply { code: 1, ..Reply::ok(&said) }
}

/// VirtualBox's machines, whether each runs; the machine another Vagrant process holds; how long
/// an attach provisions; and the `status` reads made.
#[derive(Debug, Default)]
struct Lab {
    made: BTreeMap<&'static str, bool>,
    held_by_another: Option<&'static str>,
    attach_takes: Duration,
    up_takes: Duration,
    reads: usize,
}

/// Vagrant over VirtualBox for `web` (started by `up`) and `attackbox` (by an attach).
fn answer(lab: &Mutex<Lab>, call: &Call) -> Reply {
    vagrant_answer(lab, call, |lab, args| match args {
        ["up", "--provider", "virtualbox", "attackbox"] => {
            lab.made.insert("attackbox", true);
            Reply {
                delay: lab.attach_takes,
                ..Reply::ok("")
            }
        }
        ["up", "--provider", "virtualbox", ..] => {
            lab.made.insert("web", true);
            Reply {
                delay: lab.up_takes,
                ..Reply::ok("")
            }
        }
        ["status", "--machine-readable"] => {
            lab.reads += 1;
            if let Some(name) = lab.held_by_another {
                return locked(name);
            }
            let line = |m: &str| format!("1,{m},state,{}", lab.made.get(m).map_or("not_created", |_| "running"));
            Reply::ok(&format!("{}\n{}", line("attackbox"), line("web")))
        }
        ["port", m, "--machine-readable"] => Reply::ok(&format!("1,{m},forwarded_port,22,2222")),
        ["destroy", "-f"] => {
            lab.made.clear();
            Reply::ok("")
        }
        _ => Reply::ok(""),
    })
}

/// The model, its fake, and the project with a runner on it.
fn lab() -> (Arc<Mutex<Lab>>, Arc<FakeProcesses>, tempfile::TempDir, std::path::PathBuf, Runner) {
    a_modelled_vagrant_lab(answer)
}

/// Another runner on `root`'s home, over `fake`: an app reading the run from another process.
fn another(root: &Path, fake: &Arc<FakeProcesses>) -> Runner {
    let config = runner_config(&root.join("home"), processes(fake), no_ports(), crate::runner::this_host());
    Runner::new(config).expect("a second runner")
}

/// Each machine's state, by name.
fn states(status: &crate::Status) -> BTreeMap<String, MachineState> {
    status.machines.iter().map(|m| (m.name.clone(), m.state.clone())).collect()
}

#[tokio::test(start_paused = true)]
async fn a_status_while_another_process_attaches_reads_the_run_as_last_read_without_asking_vagrant() {
    let (lab, fake, root, project, runner) = lab();
    let opts = RunOptions::new().with_provider(crate::Provider::Virtualbox);
    let run = runner.open(&project, None).expect("opened");
    run.up(&opts, &Recorder::new()).await.expect("up");
    assert!(run.status_with(&opts).await.expect("status").live);
    lab.lock().unwrap_or_else(PoisonError::into_inner).attach_takes = Duration::from_secs(600);

    let app = another(root.path(), &fake);
    let watched = app.open(&project, None).expect("opened from the app");
    let reads = async {
        tokio::time::sleep(Duration::from_secs(120)).await;
        let before = lab.lock().unwrap_or_else(PoisonError::into_inner).reads;
        let read = watched.status_with(&opts).await;
        (read, before, lab.lock().unwrap_or_else(PoisonError::into_inner).reads)
    };
    let events = Recorder::new();
    let (attached, (read, before, after)) = tokio::join!(run.attach("attackbox", &opts, &events), reads);
    attached.expect("attached");
    let read = read.expect("a status while the attach provisions");
    assert_eq!(
        (read.state, read.live, before, after),
        (RunState::Ready, false, before, before),
        "Vagrant isn't asked"
    );
    assert_eq!(states(&read).get("web"), Some(&MachineState::Running), "as last read");

    let live = watched.status_with(&opts).await.expect("status");
    assert!(live.live && states(&live).get("attackbox") == Some(&MachineState::Running), "{live:?}");
}

#[tokio::test(start_paused = true)]
async fn a_machine_another_vagrant_process_holds_reads_busy_and_its_screen_says_why() {
    let (lab, _fake, _root, project, runner) = lab();
    let opts = RunOptions::new().with_provider(crate::Provider::Virtualbox);
    let run = runner.open(&project, None).expect("opened");
    run.up(&opts, &Recorder::new()).await.expect("up");
    run.status_with(&opts).await.expect("read live once");
    lab.lock().unwrap_or_else(PoisonError::into_inner).held_by_another = Some("web");

    let read = run.status_with(&opts).await.expect("a status Vagrant's lock refused still reads");
    assert_eq!((read.state, read.live), (RunState::Ready, false));
    assert_eq!(states(&read).get("web"), Some(&MachineState::Busy), "{:?}", read.machines);
    let screen = run.graphical_console("web", &opts).await.expect_err("busy");
    assert_eq!(screen.code(), ErrorCode::NoConsole, "{screen}");
    assert!(screen.to_string().contains("another process is acting on it"), "{screen}");

    lab.lock().unwrap_or_else(PoisonError::into_inner).held_by_another = None;
    let after = run.status_with(&opts).await.expect("status");
    assert!(after.live && states(&after).values().all(|s| *s != MachineState::Busy), "{after:?}");
}

#[tokio::test(start_paused = true)]
async fn a_held_status_after_a_down_shows_none_of_the_machines_that_went() {
    let (lab, fake, root, project, runner) = lab();
    let opts = RunOptions::new().with_provider(crate::Provider::Virtualbox);
    let run = runner.open(&project, None).expect("opened");
    run.up(&opts, &Recorder::new()).await.expect("up");
    assert_eq!(run.status_with(&opts).await.expect("read once").machines.len(), 2);
    run.down(&opts, &Recorder::new()).await.expect("down");
    lab.lock().unwrap_or_else(PoisonError::into_inner).up_takes = Duration::from_secs(600);

    // The next start, read from the app while it brings the machines up.
    let app = another(root.path(), &fake);
    let watched = app.open(&project, None).expect("opened from the app");
    let reads = async {
        tokio::time::sleep(Duration::from_secs(60)).await;
        watched.status_with(&opts).await
    };
    let events = Recorder::new();
    let (started, read) = tokio::join!(run.up(&opts, &events), reads);
    started.expect("up again");
    let read = read.expect("status");
    assert_eq!((read.state, read.live), (RunState::Creating, false));
    assert_eq!(read.machines, [], "nothing of the run that went");
}
