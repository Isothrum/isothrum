//! Time, passed in: the runner reads the clock only through `Clock`, so a test fixes it.

use std::time::{SystemTime, UNIX_EPOCH};

/// Where the runner reads the time (event and record timestamps).
pub(crate) trait Clock: Send + Sync {
    /// Now.
    fn now(&self) -> SystemTime;
}

/// The system's clock.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SystemClock;

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

/// A time [`rfc3339`] wrote (`YYYY-MM-DDTHH:MM:SSZ`, a fraction of a second allowed and dropped)
/// as whole seconds since the Unix epoch; none for any other text.
#[must_use]
pub fn rfc3339_seconds(text: &str) -> Option<u64> {
    const SECS_PER_DAY: u64 = 86_400;
    let (date, time) = text.strip_suffix('Z')?.split_once('T')?;
    let ymd: Vec<i64> = date.split('-').map(str::parse).collect::<Result<_, _>>().ok()?;
    let [year, month, day] = ymd.as_slice() else { return None };
    let whole = time
        .split_once('.')
        .map_or(time, |(whole, fraction)| if fraction.bytes().all(|b| b.is_ascii_digit()) { whole } else { "" });
    let hms: Vec<u64> = whole.split(':').map(str::parse).collect::<Result<_, _>>().ok()?;
    // A leap second reads as 60.
    let [h @ 0..24, m @ 0..60, s @ 0..=60] = hms.as_slice() else { return None };
    Some(days_from_civil(*year, *month, *day)? * SECS_PER_DAY + h * 3600 + m * 60 + s)
}

/// Days since 1970-01-01 as a civil date (Howard Hinnant's algorithm; the short names are the
/// published algorithm's own).
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

/// The civil date `year`-`month`-`day` as days since 1970-01-01: [`civil`]'s way back (Howard
/// Hinnant's `days_from_civil`); none for a month or day out of range, or a year outside
/// 1970..=9999 (four digits, as [`rfc3339`] writes them).
pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> Option<u64> {
    if !(1970..=9999).contains(&year) {
        return None;
    }
    let length = match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return None,
    };
    if !(1..=length).contains(&day) {
        return None;
    }
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    u64::try_from(era * 146_097 + doe - 719_468).ok()
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
    fn a_civil_date_and_its_days_since_1970_are_each_others_way_back() {
        for days in (0..30_000).chain([146_097, 2_932_896]) {
            let (y, m, d) = civil(days);
            assert_eq!(days_from_civil(y, m, d), Some(days), "{y}-{m}-{d}");
        }
        assert_eq!(days_from_civil(2024, 2, 29), Some(19_782));
        for (y, m, d) in [
            (2023, 2, 29),
            (2026, 4, 31),
            (2026, 13, 1),
            (2026, 0, 1),
            (2026, 1, 0),
            (1969, 12, 31),
            (i64::MAX, 1, 1),
        ] {
            assert_eq!(days_from_civil(y, m, d), None, "{y}-{m}-{d}");
        }
    }

    #[test]
    fn an_rfc3339_time_reads_back_as_its_seconds() {
        for secs in [0, 951_782_400, 1_792_065_600, 1_792_065_659] {
            assert_eq!(rfc3339_seconds(&rfc3339(UNIX_EPOCH + Duration::from_secs(secs))), Some(secs));
        }
        assert_eq!(rfc3339_seconds("2026-10-15T12:00:00.250Z"), Some(1_792_065_600));
        for text in [
            "",
            "2026-10-15T12:00:00",
            "2026-10-15 12:00:00Z",
            "2026-13-15T12:00:00Z",
            "2026-10-15T24:00:00Z",
            "2026-10-15T12:00:00.x1Z",
        ] {
            assert_eq!(rfc3339_seconds(text), None, "{text}");
        }
    }
}
