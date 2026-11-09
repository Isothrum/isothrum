//! A step's time limit through a real `up`: the time it shows no progress, not its total time.

use super::*;

/// The engine, its `up` pulling an image for 40 minutes first: a line each minute, `progress(n)`.
fn pulling(engine: &Mutex<Engine>, call: &Call, progress: fn(u32) -> String) -> Reply {
    let mut reply = answer(engine, call);
    if call.line().contains(" up --detach ") {
        let pull = (1..=40).map(progress);
        reply.stdout = pull.chain(reply.stdout).collect();
        reply = reply.paced(Duration::from_secs(60));
    }
    reply
}

#[tokio::test(start_paused = true)]
async fn a_slow_pull_still_moving_runs_past_the_steps_limit() {
    // Regression: a 1.1 GB image pulled through a slow VM network was still downloading when its
    // 30-minute `up` step was stopped. Its limit now counts the time without progress.
    let f = fixture_answering(WEB_PORTAL, |e, call| {
        pulling(e, call, |n| format!("36b0bafd7f97 Downloading {}MB/1.121GB", n * 28))
    });
    let run = f.runner.open(&f.manifest, None).unwrap();
    let started = tokio::time::Instant::now();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    assert!(started.elapsed() >= Duration::from_secs(40 * 60), "past the step's 30 minutes");
    assert_eq!(f.containers(), Containers::Up);
}

#[tokio::test(start_paused = true)]
async fn a_stuck_pull_repeating_its_line_is_stopped_at_the_steps_limit() {
    let f = fixture_answering(WEB_PORTAL, |e, call| pulling(e, call, |_| "36b0bafd7f97 Downloading 1.121GB".to_owned()));
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    let started = tokio::time::Instant::now();
    let err = run.up(&opts(), &events).await.unwrap_err();
    // Its one new line came at a minute: 30 minutes later it is stopped.
    assert_eq!(started.elapsed(), Duration::from_secs(31 * 60));
    assert_eq!(
        err.to_string(),
        "step `up` showed no progress for 1800s and was stopped; its last line: `36b0bafd7f97 Downloading 1.121GB`"
    );
    let Some(EventKind::Error { code, step, timeout, .. }) = kinds(&events).pop() else {
        panic!()
    };
    assert_eq!((code, step.as_deref()), (crate::ErrorCode::Timeout, Some("up")));
    assert_eq!(
        timeout,
        Some(crate::TimedOut::NoProgress {
            after_s: 1800,
            last_line: Some("36b0bafd7f97 Downloading 1.121GB".into())
        })
    );
}
