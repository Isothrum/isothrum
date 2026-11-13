//! The event stream: every event of every operation, numbered by a cursor, the latest kept for
//! subscribers that resume after one, and sent live to every subscriber.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use isothrum_client::v1::{Event, EventCursor};
use tokio::sync::broadcast;

/// An event and its place.
pub(crate) type Numbered = (EventCursor, Arc<Event>);

/// How many events the bus keeps for replay.
const KEPT: usize = 20_000;

/// How many live events a subscriber may fall behind before it resumes from the kept ones.
const LIVE: usize = 4096;

/// The stream.
pub(crate) struct Bus {
    inner: Mutex<Kept>,
    live: broadcast::Sender<Numbered>,
}

/// The latest events, and the next cursor.
struct Kept {
    next: u64,
    events: VecDeque<Numbered>,
}

impl Bus {
    /// A bus whose first cursor is `first`: a cursor of an earlier life of the service is below
    /// it, so a client resuming after one gets every event of this life.
    pub(crate) fn new(first: u64) -> Bus {
        Bus {
            inner: Mutex::new(Kept {
                next: first.max(1),
                events: VecDeque::new(),
            }),
            live: broadcast::channel(LIVE).0,
        }
    }

    /// Appends `event`, and sends it to every subscriber.
    pub(crate) fn publish(&self, event: Event) -> EventCursor {
        let mut kept = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let cursor = EventCursor(kept.next);
        kept.next += 1;
        let numbered = (cursor, Arc::new(event));
        if kept.events.len() == KEPT {
            kept.events.pop_front();
        }
        kept.events.push_back(numbered.clone());
        // No subscriber: nobody to tell.
        drop(self.live.send(numbered));
        cursor
    }

    /// The kept events after `after` (every kept one when `None`), and the live ones from then
    /// on: taken together, so none is missed and none comes twice.
    pub(crate) fn subscribe(&self, after: Option<EventCursor>) -> (Vec<Numbered>, broadcast::Receiver<Numbered>) {
        let kept = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let live = self.live.subscribe();
        let replay = kept.events.iter().filter(|(c, _)| after.is_none_or(|a| *c > a)).cloned().collect();
        (replay, live)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use isothrum_client::v1::{OperationId, OperationPhase};
    use isothrum_runner::{Op, RunName};

    fn event(n: u64) -> Event {
        Event::phase(
            &RunName::new("web").unwrap(),
            Op::Up,
            OperationId(n.try_into().unwrap()),
            OperationPhase::Started,
            None,
        )
    }

    #[tokio::test]
    async fn a_subscriber_gets_the_kept_events_after_its_cursor_then_the_live_ones_once_each() {
        let bus = Bus::new(10);
        let first = bus.publish(event(1));
        let second = bus.publish(event(2));
        assert_eq!((first, second), (EventCursor(10), EventCursor(11)));
        let (replay, mut live) = bus.subscribe(Some(first));
        assert_eq!(replay.iter().map(|(c, _)| *c).collect::<Vec<_>>(), [second]);
        let third = bus.publish(event(3));
        assert_eq!(live.recv().await.unwrap().0, third);
        let (all, _) = bus.subscribe(None);
        assert_eq!(all.len(), 3);
        let (older_life, _) = bus.subscribe(Some(EventCursor(3)));
        assert_eq!(older_life.len(), 3, "a cursor of an earlier life replays this one's");
    }
}
