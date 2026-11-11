//! The images of a Compose start in the VM, pulled in a step the run manifest marks a download
//! (`pull-start`): no ceiling while it moves, `download_stalled` once it shows no progress for its
//! limit, `download_failed` when refused, the start never run after either; tried again, a bounded
//! number of times, when the network cut it.

use super::*;

/// What `docker compose … pull` in the VM does in a test.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Pull {
    /// Pulls at once.
    #[default]
    Done,
    /// A slow download still moving: a new line every 10 minutes for 7 hours.
    Slow,
    /// A stuck download: two new lines a minute apart, then the last one again every minute.
    Stuck,
    /// Refused by the registry.
    Refused,
    /// The registry's connection reset in the middle of a layer, once: the next pull is [`Pull::Done`].
    ResetOnce,
    /// The registry's connection reset in the middle of a layer, at every pull.
    Resets,
    /// Refused by the registry: no such image, or not to this user.
    Denied,
}

/// What a pull in the VM prints when the registry's connection resets in the middle of a layer
/// (as seen in the rig: Compose's progress on its output, the error on its standard error).
fn reset() -> Reply {
    let mut reply = Reply::fail(1, "failed to copy: read tcp 10.0.2.15:42512->13.249.228.76:443: read: connection reset by peer");
    reply.stdout = vec!["36b0bafd7f97 Downloading 1.932GB".to_owned()];
    reply
}

/// What the pull in the VM answers, as `pull` says (a reset once, then done).
pub(super) fn pulled(pull: &mut Pull) -> Reply {
    let minute = std::time::Duration::from_secs(60);
    let paced = |lines: Vec<String>, pace| {
        let mut reply = Reply::ok("");
        reply.stdout = lines;
        reply.paced(pace)
    };
    match *pull {
        Pull::Done => Reply::ok(" web Pulled"),
        Pull::Slow => paced((1..=42).map(|n| format!("36b0bafd7f97 Downloading {}MB", n * 100)).collect(), 10 * minute),
        Pull::Stuck => paced(
            ["36b0bafd7f97 Downloading 1.282GB", "36b0bafd7f97 Downloading 1.283GB"]
                .into_iter()
                .chain(std::iter::repeat_n("36b0bafd7f97 Downloading 1.283GB", 24 * 60))
                .map(str::to_owned)
                .collect(),
            minute,
        ),
        Pull::Refused => Reply::fail(18, "Error response from daemon: toomanyrequests: You have reached your pull rate limit"),
        Pull::ResetOnce => {
            *pull = Pull::Done;
            reset()
        }
        Pull::Resets => reset(),
        Pull::Denied => Reply::fail(
            18,
            "Error response from daemon: pull access denied for isoloom/kali, repository does not exist or may require 'docker login'",
        ),
    }
}

/// The states of step `id` among `events`, in order.
fn states(events: &Recorder, id: &str) -> Vec<StepState> {
    events
        .events()
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::Step { step, state, .. } if step == id => Some(state),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn the_images_are_pulled_in_the_vm_in_a_download_of_their_own_with_no_limit_while_they_move() {
    let l = local();
    l.engine.lock().unwrap_or_else(PoisonError::into_inner).pull = Pull::Slow;
    let run = l.runner.open(&l.manifest, None).unwrap();
    let events = Recorder::new();
    let started = tokio::time::Instant::now();
    run.up(&opts(), &events).await.unwrap();
    // Past the six hours any step stops at, and its limit of 15 minutes without progress.
    assert!(started.elapsed() >= std::time::Duration::from_secs(7 * 3600), "{:?}", started.elapsed());
    assert_eq!(states(&events, "pull-start"), [StepState::Running, StepState::Done]);
    let order: Vec<String> = events
        .events()
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::Step {
                step,
                state: StepState::Running,
                ..
            } if step.contains("start") => Some(step),
            _ => None,
        })
        .collect();
    assert_eq!(order, ["pull-start", "start"], "the pull, then the start");
    let pull = l.fake.last(" pull --policy ").unwrap();
    assert_eq!(pull.args[..3], ["ssh", "--no-tty", "--command"]);
}

#[tokio::test(start_paused = true)]
async fn a_stuck_pull_in_the_vm_is_a_stalled_download_and_a_refused_one_a_failed_download() {
    let stuck = local();
    stuck.engine.lock().unwrap_or_else(PoisonError::into_inner).pull = Pull::Stuck;
    let run = stuck.runner.open(&stuck.manifest, None).unwrap();
    let started = tokio::time::Instant::now();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    // The VM's boot takes no time here; its last new line came at two minutes.
    assert_eq!(started.elapsed(), std::time::Duration::from_secs(17 * 60));
    assert_eq!(err.code(), ErrorCode::DownloadStalled);
    assert!(
        matches!(&err, crate::Error::DownloadStalled { step, after_s: 900, last_line: Some(line) } if step == "pull-start" && line == "36b0bafd7f97 Downloading 1.283GB"),
        "{err:?}"
    );
    assert!(!stuck.engine.lock().unwrap_or_else(PoisonError::into_inner).up, "nothing started");

    let refused = local();
    refused.engine.lock().unwrap_or_else(PoisonError::into_inner).pull = Pull::Refused;
    let again = refused.runner.open(&refused.manifest, None).unwrap();
    let failed = again.up(&opts(), &Recorder::new()).await.unwrap_err();
    let crate::Error::DownloadFailed { step, reason } = &failed else {
        panic!("{failed:?}")
    };
    assert_eq!(step, "pull-start");
    assert!(reason.contains("toomanyrequests"), "{reason}");
}

/// A start in the VM whose pull answers as `pull` says: the run, its events, the start's outcome
/// and how long it took (on the paused clock).
async fn up_pulling(pull: Pull) -> (Running, Recorder, crate::Result<()>, std::time::Duration) {
    let l = local();
    l.engine.lock().unwrap_or_else(PoisonError::into_inner).pull = pull;
    let run = l.runner.open(&l.manifest, None).unwrap();
    let events = Recorder::new();
    let started = tokio::time::Instant::now();
    let done = run.up(&opts(), &events).await.map(|_| ());
    let took = started.elapsed();
    (l, events, done, took)
}

/// What the `retrying` warnings for step `id` say, after the step's name: each new try of a
/// download.
fn retries(events: &Recorder, id: &str) -> Vec<String> {
    let of = format!("step `{id}`: ");
    events
        .events()
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::Warning {
                code: crate::WarningCode::Retrying,
                message,
            } => message.strip_prefix(&of).map(str::to_owned),
            _ => None,
        })
        .collect()
}

/// How many pulls of the start's images the VM was asked for (not the init container's, after it).
fn pulls(l: &Running) -> usize {
    l.fake
        .lines()
        .iter()
        .filter(|line| line.contains(" pull --policy ") && line.contains(" --include-deps "))
        .count()
}

#[tokio::test(start_paused = true)]
async fn a_pull_in_the_vm_the_network_cut_once_runs_again_and_the_start_goes_on() {
    // Regression: one `connection reset by peer` in the middle of a 1.93 GB layer failed the start.
    let (l, events, done, took) = up_pulling(Pull::ResetOnce).await;
    done.unwrap();
    assert_eq!(pulls(&l), 2);
    assert_eq!(took, std::time::Duration::from_secs(5), "the first wait, on the paused clock");
    assert_eq!(
        retries(&events, "pull-start"),
        [
            "download failed for the network, retrying in 5s (attempt 2 of 4): failed to copy: read tcp 10.0.2.15:42512->13.249.228.76:443: read: connection reset by peer (exited with code 1)"
        ]
    );
    assert_eq!(states(&events, "pull-start"), [StepState::Running, StepState::Done]);
    assert!(l.engine.lock().unwrap_or_else(PoisonError::into_inner).up, "started");
}

#[tokio::test(start_paused = true)]
async fn a_pull_in_the_vm_the_registry_refused_is_not_tried_again() {
    for refused in [Pull::Denied, Pull::Refused] {
        let (l, events, done, took) = up_pulling(refused).await;
        let err = done.unwrap_err();
        assert!(matches!(&err, crate::Error::DownloadFailed { step, .. } if step == "pull-start"), "{err:?}");
        assert_eq!((pulls(&l), took), (1, std::time::Duration::ZERO), "{refused:?}");
        assert_eq!(retries(&events, "pull-start"), Vec::<String>::new());
        assert!(!l.engine.lock().unwrap_or_else(PoisonError::into_inner).up, "nothing started");
    }
}

#[tokio::test(start_paused = true)]
async fn a_pull_in_the_vm_the_network_keeps_cutting_gives_up_after_three_more_tries() {
    let (l, events, done, took) = up_pulling(Pull::Resets).await;
    let err = done.unwrap_err();
    let crate::Error::DownloadFailed { step, reason } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(step, "pull-start");
    assert!(reason.ends_with("read: connection reset by peer (exited with code 1)"), "{reason}");
    assert_eq!(err.code(), ErrorCode::DownloadFailed);
    assert_eq!(pulls(&l), 4);
    assert_eq!(took, std::time::Duration::from_secs(5 + 15 + 45));
    let said: Vec<String> = retries(&events, "pull-start")
        .iter()
        .map(|line| line.split_once(':').unwrap().0.to_owned())
        .collect();
    assert_eq!(
        said,
        [
            "download failed for the network, retrying in 5s (attempt 2 of 4)",
            "download failed for the network, retrying in 15s (attempt 3 of 4)",
            "download failed for the network, retrying in 45s (attempt 4 of 4)",
        ]
    );
    assert!(!l.engine.lock().unwrap_or_else(PoisonError::into_inner).up, "nothing started");
}

#[tokio::test(start_paused = true)]
async fn a_start_cut_short_while_a_pull_waits_to_run_again_runs_it_no_more() {
    let l = local();
    l.engine.lock().unwrap_or_else(PoisonError::into_inner).pull = Pull::Resets;
    let run = l.runner.open(&l.manifest, None).unwrap();
    // Cut in the second wait (5 s, a pull, then 15 s).
    let cut = tokio::time::timeout(std::time::Duration::from_secs(10), run.up(&opts(), &Recorder::new())).await;
    assert!(cut.is_err(), "still waiting");
    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    assert_eq!(pulls(&l), 2, "{:?}", l.fake.lines());
}
