//! The images of a Compose start in the VM, pulled in a step the run manifest marks a download
//! (`pull-start`): no ceiling while it moves, `download_stalled` once it shows no progress for its
//! limit, `download_failed` when refused, the start never run after either.

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
}

/// What the pull in the VM answers, as `pull` says.
pub(super) fn pulled(pull: Pull) -> Reply {
    let minute = std::time::Duration::from_secs(60);
    let paced = |lines: Vec<String>, pace| {
        let mut reply = Reply::ok("");
        reply.stdout = lines;
        reply.paced(pace)
    };
    match pull {
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
