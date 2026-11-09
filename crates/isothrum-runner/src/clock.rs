//! Time, passed in: the runner reads the clock only through [`Clock`], so a test fixes it.

use std::time::{SystemTime, UNIX_EPOCH};

/// Where the runner reads the time (event and record timestamps).
pub trait Clock: Send + Sync {
    /// Now.
    fn now(&self) -> SystemTime;
}

/// The system's clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// A clock that always reads the same instant, for tests.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct FixedClock(pub(crate) SystemTime);

#[cfg(test)]
impl Clock for FixedClock {
    fn now(&self) -> SystemTime {
        self.0
    }
}

/// `time` in whole seconds since the Unix epoch (0 before it).
pub(crate) fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// `time` as `YYYY-MM-DDTHH:MM:SSZ` (UTC, whole seconds; before 1970 reads as 1970).
#[must_use]
pub fn rfc3339(time: SystemTime) -> String {
    let secs = unix_seconds(time);
    let (hour, minute, second) = ((secs % 86_400) / 3600, (secs % 3600) / 60, secs % 60);
    let (year, month, day) = civil(secs / 86_400);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Days since 1970-01-01 as a civil date (Howard Hinnant's algorithm, as Isoloom's registry wrote
/// it; the short names are the published algorithm's own).
fn civil(days: u64) -> (i64, i64, i64) {
    let z = days.cast_signed() + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    (y, mo, d)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn times_read_as_rfc3339_in_utc() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(UNIX_EPOCH + Duration::from_secs(1_792_065_600)), "2026-10-15T12:00:00Z");
        assert_eq!(rfc3339(UNIX_EPOCH + Duration::from_secs(951_782_400)), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(UNIX_EPOCH - Duration::from_secs(5)), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn a_fixed_clock_stands_still() {
        let clock = FixedClock(UNIX_EPOCH + Duration::from_secs(60));
        assert_eq!(clock.now(), clock.now());
        assert_eq!(rfc3339(clock.now()), "1970-01-01T00:01:00Z");
    }
}
