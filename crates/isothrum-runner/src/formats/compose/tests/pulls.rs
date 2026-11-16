//! A start's pull: stopped when it prints nothing for its stall limit, run on while it moves,
//! tried again when the network cut it short, and never when the registry refused an image.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::*;
use crate::events::WarningCode;
use crate::formats::compose::pull::{PULL_STALL, RETRY_AFTER};

/// The warnings a start gave.
type Warned = Arc<Mutex<Vec<WarningCode>>>;

/// A start of a project of one service on `fake`: its outcome and its warnings.
async fn start(fake: &FakeProcesses) -> (Result<()>, Vec<WarningCode>) {
    let (_dir, project) = folder();
    let run = run();
    let warned: Warned = Arc::default();
    let into = Arc::clone(&warned);
    let warn = move |code: WarningCode, _: &str| into.lock().unwrap_or_else(PoisonError::into_inner).push(code);
    let outcome = Compose.up(&cx_warned(&project, &run, fake, &warn)).await;
    let said = warned.lock().unwrap_or_else(PoisonError::into_inner).clone();
    (outcome, said)
}

/// A pull that prints `lines`, each after `pace`, then ends with `code` printing `stderr`.
fn pulling(lines: usize, pace: Duration, code: i32, stderr: &str) -> Reply {
    Reply {
        stdout: (0..lines).map(|i| format!("web Downloading layer {i}")).collect(),
        stderr: stderr.lines().map(str::to_owned).collect(),
        code,
        pace,
        ..Reply::default()
    }
}

#[tokio::test(start_paused = true)]
async fn a_pull_that_prints_nothing_for_its_stall_limit_is_stopped_and_one_that_moves_runs_on() {
    let silent = Reply {
        delay: PULL_STALL * 3,
        ..Reply::ok("")
    };
    let stuck = FakeProcesses::script()
        .expect(CONFIG, Reply::ok(NO_PORTS))
        .expect_times(PULL, 1, silent)
        .build();
    let (e, _) = start(&stuck).await;
    let e = e.expect_err("stalled");
    assert!(
        matches!(&e, Error::DownloadStalled { step, after_s, .. } if step == "pull-up" && *after_s == PULL_STALL.as_secs()),
        "{e:?}"
    );
    assert_eq!(e.code(), crate::ErrorCode::DownloadStalled);

    // Six layers, one every half of the limit: three limits in all, never one without a line.
    let moving = FakeProcesses::script()
        .expect(CONFIG, Reply::ok(NO_PORTS))
        .expect_times(PULL, 1, pulling(6, PULL_STALL / 2, 0, ""))
        .expect_times(UP, 1, Reply::ok(""))
        .build();
    let (ok, _) = start(&moving).await;
    ok.expect("a pull that moves runs as long as it moves");
}

#[tokio::test(start_paused = true)]
async fn a_pull_the_network_cut_short_is_tried_again_and_one_refused_never() {
    let reset = "Error response from daemon: Get \"https://registry-1.docker.io/v2/\": net/http: TLS handshake timeout";
    let tries = Arc::new(Mutex::new(0_usize));
    let flaky = FakeProcesses::model({
        let tries = Arc::clone(&tries);
        move |call| match call.args.iter().find(|a| ["pull", "up", "config"].contains(&a.as_str())).map(String::as_str) {
            Some("pull") => {
                let mut n = tries.lock().unwrap_or_else(PoisonError::into_inner);
                *n += 1;
                if *n < 3 { Reply::fail(1, reset) } else { Reply::ok("") }
            }
            Some("config") => Reply::ok(NO_PORTS),
            _ => Reply::ok(""),
        }
    });
    let started = tokio::time::Instant::now();
    let (ok, warned) = start(&flaky).await;
    ok.expect("the third try pulled");
    assert_eq!(*tries.lock().unwrap_or_else(PoisonError::into_inner), 3);
    assert_eq!(warned, [WarningCode::Retrying, WarningCode::Retrying]);
    assert!(started.elapsed() >= RETRY_AFTER.iter().sum::<Duration>(), "it waited before each try");

    let always = FakeProcesses::script()
        .expect(CONFIG, Reply::ok(NO_PORTS))
        .expect_times(PULL, 3, Reply::fail(1, reset))
        .build();
    let (cut, _) = start(&always).await;
    assert_eq!(cut.expect_err("cut short every time").code(), crate::ErrorCode::DownloadFailed);

    let refused = "Error response from daemon: pull access denied for lab/web, repository does not exist or may require 'docker login'";
    let denied = FakeProcesses::script()
        .expect(CONFIG, Reply::ok(NO_PORTS))
        .expect_times(PULL, 1, Reply::fail(1, refused))
        .build();
    let (refusal, said) = start(&denied).await;
    let refusal = refusal.expect_err("refused");
    assert!(
        matches!(&refusal, Error::DownloadFailed { step, reason, .. } if step == "pull-up" && reason.contains("pull access denied")),
        "{refusal:?}"
    );
    assert!(said.is_empty(), "never tried again: {said:?}");
}
