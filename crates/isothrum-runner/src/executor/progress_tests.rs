use super::*;
use crate::process::fake::{FakeProcesses, Reply};
use crate::process::{Captured, ProcessSpec};

/// Runs `reply` as step `pull` under `limit`, feeding its progress as a step does: its outcome
/// and how long it ran.
async fn run(reply: Reply, limit: Duration) -> (Result<Captured>, Duration) {
    let fake = FakeProcesses::new(move |_| reply.clone());
    let spec = ProcessSpec::new("docker", ["compose", "up"]);
    let started = Instant::now();
    let progress = Progress::new();
    let out = within("pull", limit, &progress, progress.capture(&fake, &spec)).await;
    (out, started.elapsed())
}

fn lines(make: impl Fn(usize) -> String, count: usize) -> String {
    (1..=count).map(make).collect::<Vec<_>>().join("\n")
}

const MINUTE: Duration = Duration::from_secs(60);
const TEN_MINUTES: Duration = Duration::from_secs(600);

#[tokio::test(start_paused = true)]
async fn a_step_printing_new_lines_runs_past_its_limit() {
    // A slow pull still moving: a new line every minute for 40 minutes, under a 10-minute limit.
    let pull = Reply::ok(&lines(|n| format!("36b0bafd7f97 Downloading {n}MB/1.121GB"), 40)).paced(MINUTE);
    let (out, took) = run(pull, TEN_MINUTES).await;
    assert!(out.unwrap().success());
    assert_eq!(took, 40 * MINUTE);
}

#[tokio::test(start_paused = true)]
async fn a_step_repeating_its_lines_stops_at_its_limit_quoting_the_last() {
    // A stuck pull: two layers take turns printing the same two lines, a minute apart, forever.
    let stuck = lines(
        |n| {
            if n % 2 == 0 {
                "36b0bafd7f97 Downloading 1.121GB".to_owned()
            } else {
                "4f4fb700ef54 Downloading 32B".to_owned()
            }
        },
        200,
    );
    let (out, took) = run(Reply::ok(&stuck).paced(MINUTE), TEN_MINUTES).await;
    let err = out.unwrap_err();
    // Its last new line came at two minutes: ten minutes later it is stopped.
    assert_eq!(took, 12 * MINUTE);
    let Error::Timeout { timed_out, .. } = &err else { panic!("{err:?}") };
    assert_eq!(
        *timed_out,
        TimedOut::NoProgress {
            after_s: 600,
            last_line: Some("36b0bafd7f97 Downloading 1.121GB".into())
        }
    );
    assert_eq!(
        err.to_string(),
        "step `pull` showed no progress for 600s and was stopped; its last line: `36b0bafd7f97 Downloading 1.121GB`"
    );
}

#[tokio::test(start_paused = true)]
async fn a_silent_step_stops_at_its_limit_from_its_start() {
    // A step that prints nothing (a quiet `terraform apply`): its limit counts from its start.
    let (out, took) = run(Reply::ok("done").after(Duration::from_secs(3600)), TEN_MINUTES).await;
    let err = out.unwrap_err();
    assert_eq!(took, TEN_MINUTES);
    assert_eq!(err.to_string(), "step `pull` printed nothing for 600s and was stopped");
    // Blank lines are no progress either.
    let blank = Reply::ok(&lines(|_| "  ".to_owned(), 100)).paced(MINUTE);
    let (blank_out, blank_took) = run(blank, TEN_MINUTES).await;
    assert!(matches!(
        blank_out,
        Err(Error::Timeout {
            timed_out: TimedOut::NoProgress { last_line: None, .. },
            ..
        })
    ));
    assert_eq!(blank_took, TEN_MINUTES);
    // Quiet within its limit, it runs to its end.
    let (quiet, _) = run(Reply::ok("done").after(9 * MINUTE), TEN_MINUTES).await;
    assert!(quiet.unwrap().success());
}

#[tokio::test(start_paused = true)]
async fn the_ceiling_stops_a_step_that_keeps_printing_new_lines() {
    // A script looping on changing lines, a minute apart, for 16 hours.
    let looping = Reply::ok(&lines(|n| format!("tick {n}"), 1000)).paced(MINUTE);
    let (out, took) = run(looping, TEN_MINUTES).await;
    assert_eq!(took, CEILING);
    let err = out.unwrap_err();
    assert!(
        matches!(
            &err,
            Error::Timeout {
                timed_out: TimedOut::Ceiling { after_s: 21_600 },
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        "step `pull` was stopped after 21600s, the most a step may run, though it kept making progress"
    );
}

#[tokio::test(start_paused = true)]
async fn a_limit_longer_than_the_ceiling_is_kept() {
    // A step allowed eight hours quiet is allowed them: the ceiling never cuts a limit short.
    let eight_hours = Duration::from_secs(8 * 3600);
    let (silent, silent_took) = run(Reply::ok("").after(Duration::from_secs(9 * 3600)), eight_hours).await;
    assert!(matches!(
        silent,
        Err(Error::Timeout {
            timed_out: TimedOut::NoProgress { after_s: 28_800, .. },
            ..
        })
    ));
    assert_eq!(silent_took, eight_hours);
    let (looping, looping_took) = run(Reply::ok(&lines(|n| format!("tick {n}"), 600)).paced(MINUTE), eight_hours).await;
    assert!(matches!(
        looping,
        Err(Error::Timeout {
            timed_out: TimedOut::Ceiling { after_s: 28_800 },
            ..
        })
    ));
    assert_eq!(looping_took, eight_hours);
}

#[tokio::test(start_paused = true)]
async fn a_line_long_forgotten_is_progress_again_and_a_quoted_line_is_cut() {
    let progress = Progress::new();
    let start = progress.seen().at;
    progress.saw("first");
    tokio::time::advance(MINUTE).await;
    for n in 0..RECENT {
        progress.saw(&format!("line {n}"));
    }
    let after_lines = progress.seen().at;
    assert_eq!(after_lines, start + MINUTE);
    tokio::time::advance(MINUTE).await;
    progress.saw("  line 3 ");
    assert_eq!(progress.seen().at, after_lines, "seen recently, spaces aside");
    progress.saw("first");
    assert_eq!(progress.seen().at, start + 2 * MINUTE, "pushed out by the newer lines");
    let long = "é".repeat(400);
    let (out, _) = run(Reply::ok(&[long.as_str(); 3].join("\n")).paced(6 * MINUTE), TEN_MINUTES).await;
    let Err(Error::Timeout {
        timed_out: TimedOut::NoProgress { last_line: Some(line), .. },
        ..
    }) = out
    else {
        panic!("{out:?}")
    };
    assert!(line.ends_with("...") && line.len() <= LAST_LINE_MAX, "{} bytes", line.len());
}
