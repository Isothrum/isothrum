//! The journal as a service reads it: a page of events past rows it can't read, and the queued
//! operations a dead service left.

use super::*;

#[test]
fn a_page_of_rows_none_of_them_readable_still_moves_the_reader_on() {
    let (_home, store) = store();
    let r = name("r");
    let first = store
        .write(|tx| {
            let first = tx.append_raw_event(Some("r"), None, r#"{"v":9,"event":"a_later_kind"}"#)?;
            tx.append_raw_event(Some("r"), None, r#"{"v":9,"event":"a_later_kind"}"#)?;
            Ok(first)
        })
        .unwrap();
    let page = store.read(|tx| tx.events_page(EventCursor::default(), Some(&r), 2)).unwrap();
    assert_eq!((page.events.len(), page.unreadable), (0, 2));
    assert!(page.last > first, "past both rows: {page:?}");
    let next = store.read(|tx| tx.events_page(page.last, Some(&r), 2)).unwrap();
    assert_eq!(
        (next.events.len(), next.unreadable, next.last),
        (0, 0, page.last),
        "nothing after, the reader stays"
    );
}

#[test]
fn a_dead_services_queued_operations_are_cancelled_and_a_live_ones_wait() {
    let (_home, store) = store();
    let r = name("r");
    let mut gone = Owner::this_process();
    gone.pid = u32::MAX;
    gone.boot = "a boot before this one".into();
    let me = Owner::this_process();
    let (orphan, waiting, running) = store
        .write(|tx| {
            let orphan = tx.queue_operation(&r, Op::Up, &gone)?.id;
            let waiting = tx.queue_operation(&r, Op::Park, &me)?.id;
            let running = tx.begin_operation(&name("other"), Op::Up, &gone, None)?.id;
            Ok((orphan, waiting, running))
        })
        .unwrap();
    #[expect(
        clippy::redundant_closure_for_method_calls,
        reason = "the method as a path isn't general over the transaction's lifetime"
    )]
    let cancelled = store.write(|tx| tx.cancel_orphaned_queued()).unwrap();
    assert_eq!(
        cancelled,
        [orphan],
        "only the dead owner's queued row; a started one is the next operation's to recover"
    );
    let outcome = |id| store.read(|tx| tx.operation(id)).unwrap().unwrap().ended.map(|e| e.outcome);
    assert_eq!((outcome(orphan), outcome(waiting), outcome(running)), (Some(Outcome::Cancelled), None, None));
}
