//! An operation cut short (its future dropped: an embedder's cancel, the command line's Ctrl-C).

use super::*;
use crate::{ErrorCode, RunState};

/// The engine, its `up` never returning once the containers are up.
fn stuck_up(engine: &Mutex<Engine>, call: &Call) -> Reply {
    let reply = answer(engine, call);
    if call.line().contains(" up --detach ") {
        reply.after(Duration::from_secs(24 * 3600))
    } else {
        reply
    }
}

#[tokio::test(start_paused = true)]
async fn an_operation_cut_short_says_cancelled_and_the_next_start_clears_what_it_left() {
    let f = fixture_answering(FLAG_PORTAL, stuck_up);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    let cut = tokio::time::timeout(Duration::from_secs(600), run.up(&opts(), &events)).await;
    assert!(cut.is_err(), "the start was still running");

    let k = kinds(&events);
    let [.., state, error] = k.as_slice() else { panic!("{k:?}") };
    assert_eq!(
        state,
        &EventKind::State {
            from: RunState::Starting,
            to: RunState::Failed
        }
    );
    assert!(
        matches!(
            error,
            EventKind::Error {
                code: ErrorCode::Cancelled,
                ..
            }
        ),
        "{error:?}"
    );
    let record = f.runner.store.record(run.name()).unwrap().unwrap();
    assert_eq!((record.state, record.last.and_then(|l| l.code)), (RunState::Failed, Some(ErrorCode::Cancelled)));

    // The next start removes the containers the cut one left (then its own `up` is stuck too).
    let next = tokio::time::timeout(Duration::from_secs(600), run.up(&opts(), &Recorder::new())).await;
    assert!(next.is_err());
    assert_eq!(f.called(" down --volumes"), 1, "{:?}", f.fake.lines());
}

#[tokio::test]
async fn an_operation_that_ends_says_nothing_more() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    let k = kinds(&events);
    assert!(matches!(k.last(), Some(EventKind::Result { ok: true, .. })), "{k:?}");
    assert!(!k.iter().any(|e| matches!(e, EventKind::Error { .. })), "no `cancelled` after its result");
}
