//! The captures in progress, by lab and network: a new capture of a network replaces the old
//! one, stopping a lab stops its captures, leaving the app stops them all.

use std::collections::HashMap;
use std::sync::Mutex;

/// Something that runs until stopped (a capture).
pub(crate) trait Stop {
    fn stop(&mut self);
}

impl Stop for crate::runner::Capture {
    fn stop(&mut self) {
        crate::runner::Capture::stop(self);
    }
}

/// Captures in progress, by `<lab>/<network>`.
pub(crate) struct Captures<C: Stop = crate::runner::Capture>(Mutex<HashMap<String, C>>);

impl<C: Stop> Default for Captures<C> {
    fn default() -> Self {
        Captures(Mutex::new(HashMap::new()))
    }
}

fn key(lab: &str, network: &str) -> String {
    format!("{lab}/{network}")
}

impl<C: Stop> Captures<C> {
    fn all(&self) -> std::sync::MutexGuard<'_, HashMap<String, C>> {
        // A panic while holding the lock leaves the map itself usable.
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records a capture of `network`, stopping the one it replaces.
    pub(crate) fn add(&self, lab: &str, network: &str, capture: C) {
        if let Some(mut old) = self.all().insert(key(lab, network), capture) {
            old.stop();
        }
    }

    /// Stops the capture of `network`, if there is one.
    pub(crate) fn stop(&self, lab: &str, network: &str) {
        if let Some(mut c) = self.all().remove(&key(lab, network)) {
            c.stop();
        }
    }

    /// Stops every capture of `lab`.
    pub(crate) fn stop_lab(&self, lab: &str) {
        let prefix = key(lab, "");
        for (_, mut c) in self.all().extract_if(|k, _| k.starts_with(&prefix)) {
            c.stop();
        }
    }

    /// Stops every capture.
    pub(crate) fn stop_all(&self) {
        for (_, mut c) in self.all().drain() {
            c.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// The names of the captures stopped, in order.
    type Log = Arc<Mutex<Vec<&'static str>>>;

    /// A capture that writes its name in the log when stopped.
    struct Fake(&'static str, Log);

    impl Stop for Fake {
        fn stop(&mut self) {
            self.1.lock().unwrap().push(self.0);
        }
    }

    #[test]
    fn a_new_capture_of_a_network_stops_the_old_one() {
        let (caps, stopped) = (Captures::default(), Log::default());
        let fake = |name| Fake(name, Arc::clone(&stopped));
        caps.add("lab", "office", fake("first"));
        caps.add("lab", "office", fake("second"));
        assert_eq!(*stopped.lock().unwrap(), ["first"]);
        caps.stop("lab", "office");
        caps.stop("lab", "office");
        assert_eq!(*stopped.lock().unwrap(), ["first", "second"]);
    }

    #[test]
    fn stopping_a_lab_stops_its_captures_only() {
        let (caps, stopped) = (Captures::default(), Log::default());
        let fake = |name| Fake(name, Arc::clone(&stopped));
        caps.add("lab", "office", fake("office"));
        caps.add("lab", "link-0", fake("link"));
        caps.add("lab-2", "office", fake("other lab"));
        caps.stop_lab("lab");
        let mut done = stopped.lock().unwrap().clone();
        done.sort_unstable();
        assert_eq!(done, ["link", "office"]);
        caps.stop_all();
        assert_eq!(stopped.lock().unwrap().last(), Some(&"other lab"));
    }
}
