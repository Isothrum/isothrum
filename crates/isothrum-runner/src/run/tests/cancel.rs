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
    let f = fixture_answering(WEB_PORTAL, stuck_up);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    let cut = tokio::time::timeout(Duration::from_secs(600), run.up(&opts(), &events)).await;
    assert!(cut.is_err(), "the start was still running");

    let k = kinds(&events);
    let [.., state, error] = k.as_slice() else { panic!("{k:?}") };
    assert_eq!(
        state,
        &EventKind::State {
            from: RunState::Creating,
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

/// The engine, each of its answers taking a moment: every call is a point the operation yields at.
fn each_call_yields(engine: &Mutex<Engine>, call: &Call) -> Reply {
    answer(engine, call).after(Duration::from_millis(1))
}

/// `op`, cut short (dropped) once it was polled `polls` times without ending: `None` then.
async fn cut_after<T>(polls: usize, op: impl Future<Output = T>) -> Option<T> {
    let mut op = std::pin::pin!(op);
    let mut left = polls;
    std::future::poll_fn(move |cx| {
        if left == 0 {
            return std::task::Poll::Ready(None);
        }
        left -= 1;
        op.as_mut().poll(cx).map(Some)
    })
    .await
}

#[tokio::test(start_paused = true)]
async fn a_start_cut_short_at_any_point_leaves_a_settled_record_a_free_run_and_one_last_event() {
    // Every point an operation can be cut at (each `.await` it yields at), one after another:
    // its future dropped there runs the guards' drops (`CutShort`, `Unfinished`, the run's lock,
    // the in-flight entry), whose writes are synchronous, so none is ever left half-done.
    for polls in 1.. {
        let f = fixture_answering(WEB_PORTAL, each_call_yields);
        let run = f.runner.open(&f.manifest, None).unwrap();
        let events = Recorder::new();
        let Some(done) = Box::pin(cut_after(polls, run.up(&opts(), &events))).await else {
            let k = kinds(&events);
            let ends = k.iter().filter(|e| matches!(e, EventKind::Result { .. } | EventKind::Error { .. })).count();
            assert_eq!(ends, 1, "cut after {polls} polls: {k:?}");
            assert!(
                matches!(
                    k.last(),
                    Some(EventKind::Error {
                        code: ErrorCode::Cancelled,
                        ..
                    })
                ),
                "cut after {polls} polls: {k:?}"
            );
            let record = f.runner.store.record(run.name()).unwrap();
            assert!(!record.as_ref().is_some_and(|r| r.state.is_moving()), "cut after {polls} polls: {record:?}");
            assert!(!f.runner.store.held(run.name()).unwrap(), "cut after {polls} polls: the run is still held");
            assert!(f.runner.op_of(run.name()).is_none(), "cut after {polls} polls: still in flight");
            run.up(&opts(), &Recorder::new())
                .await
                .unwrap_or_else(|e| panic!("cut after {polls} polls, the next start: {e}"));
            continue;
        };
        done.unwrap();
        assert!(polls > 10, "the start yielded at each call: {polls} polls");
        break;
    }
}
