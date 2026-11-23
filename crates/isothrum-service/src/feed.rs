//! The event stream: the home's store keeps every event (the runner appends its operations'
//! events, the service its own `operation` events), numbered by the store's cursor; a counter
//! wakes the subscribers, which read what is new from the store. Replay and live are one path:
//! a subscriber that resumes after a cursor misses nothing the store keeps.

use std::sync::Arc;

use isothrum_runner::RunName;
use isothrum_runner::store::{EventCursor, OperationId, Store};
use tokio::sync::watch;

use crate::engine::Refusal;
use isothrum_client::v1::Event;

/// How many events a subscriber reads from the store at a time.
const BATCH: usize = 500;

/// The stream.
pub(crate) struct Feed {
    store: Arc<Store>,
    news: watch::Sender<u64>,
    /// Set once the service stopped: a subscriber sends what is kept, then ends.
    closed: watch::Sender<bool>,
}

impl Feed {
    /// The stream of `store`.
    pub(crate) fn new(store: Arc<Store>) -> Feed {
        Feed {
            store,
            news: watch::channel(0).0,
            closed: watch::channel(false).0,
        }
    }

    /// Keeps `event` (of `operation`, if any), and wakes the subscribers.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub(crate) fn keep(&self, operation: Option<OperationId>, event: &Event) -> Result<EventCursor, Refusal> {
        let cursor = self.store.write(|tx| tx.append_event(operation, event))?;
        self.wake();
        Ok(cursor)
    }

    /// Wakes the subscribers: something was kept (by the runner, or the service).
    pub(crate) fn wake(&self) {
        self.news.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// What wakes a subscriber.
    pub(crate) fn news(&self) -> watch::Receiver<u64> {
        self.news.subscribe()
    }

    /// Closes the stream: the service stopped. Each subscriber sends what is kept, then ends, and
    /// each connection ends once its replies are written.
    pub(crate) fn close(&self) {
        self.closed.send_replace(true);
    }

    /// What says the stream closed ([`Feed::close`]).
    pub(crate) fn closed(&self) -> watch::Receiver<bool> {
        self.closed.subscribe()
    }

    /// The kept events after `after`, of `run` or of every run, oldest first (a batch of them),
    /// and the cursor of the last row scanned, readable or not: where the next batch starts, so
    /// rows another version wrote never hold a subscriber back.
    ///
    /// # Errors
    ///
    /// The store can't be read.
    pub(crate) fn after(&self, after: EventCursor, run: Option<&RunName>) -> Result<(Vec<(EventCursor, Event)>, EventCursor), Refusal> {
        let page = self.store.read(|tx| tx.events_page(after, run, BATCH))?;
        if page.unreadable > 0 {
            tracing::debug!(unreadable = page.unreadable, "event rows another version wrote were skipped");
        }
        Ok((page.events, page.last))
    }
}
