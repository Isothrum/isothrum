//! Reading `docker stats --no-stream --format '{{json .}}'`: each running container's CPU and
//! memory use, one JSON object per line. A figure the engine has none for (`--`, a container just
//! started or gone between two reads) is none, never zero; a line that isn't a container's is
//! skipped, so one odd line costs its container's figures, not the others'.

use serde::Deserialize;

/// One container's use, as `stats` prints it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ContainerStats {
    /// Its id, as `stats` prints it (shortened).
    pub(crate) id: String,
    /// Its CPU use, in percent of one CPU (over 100 on several).
    pub(crate) cpu_percent: Option<f64>,
    /// The memory it uses, in bytes.
    pub(crate) memory_bytes: Option<u64>,
}

impl ContainerStats {
    /// Whether it is the container `id` (either id may be the other shortened).
    pub(crate) fn is(&self, id: &str) -> bool {
        !id.is_empty() && !self.id.is_empty() && (id.starts_with(&self.id) || self.id.starts_with(id))
    }
}

/// A line as the engine prints it.
#[derive(Deserialize)]
struct Line {
    #[serde(rename = "ID", default)]
    id: String,
    #[serde(rename = "CPUPerc", default)]
    cpu: String,
    /// `used / limit`.
    #[serde(rename = "MemUsage", default)]
    memory: String,
}

/// The containers in `out`.
pub(crate) fn parse(out: &str) -> Vec<ContainerStats> {
    out.lines()
        .filter_map(|l| serde_json::from_str::<Line>(l.trim()).ok())
        .filter(|l| !l.id.is_empty())
        .map(|l| ContainerStats {
            cpu_percent: percent(&l.cpu),
            memory_bytes: l.memory.split('/').next().and_then(size),
            id: l.id,
        })
        .collect()
}

/// `1.78%` as `1.78`.
fn percent(s: &str) -> Option<f64> {
    s.trim().strip_suffix('%')?.trim().parse::<f64>().ok().filter(|p| p.is_finite() && *p >= 0.0)
}

/// `10.27MiB`, `1.5GB`, `512B` as bytes (a fraction of a byte dropped), worked out in integers so
/// a size is exact and never wraps.
fn size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s.find(|c: char| c.is_ascii_alphabetic())?;
    let (number, unit) = s.split_at(split);
    let unit = unit_bytes(unit.trim())?;
    let (whole, fraction) = number.trim().split_once('.').unwrap_or((number.trim(), ""));
    let digits = |p: &str| p.bytes().all(|b| b.is_ascii_digit());
    if (whole.is_empty() && fraction.is_empty()) || !digits(whole) || !digits(fraction) {
        return None;
    }
    // Past 20 digits a fraction adds nothing a byte count holds (and its product stays in range).
    let fraction = fraction.get(..fraction.len().min(20)).unwrap_or(fraction);
    let value = |p: &str| if p.is_empty() { Some(0) } else { p.parse::<u128>().ok() };
    let scale = 10u128.pow(u32::try_from(fraction.len()).ok()?);
    let bytes = value(whole)?.saturating_mul(unit).saturating_add(value(fraction)?.saturating_mul(unit) / scale);
    Some(u64::try_from(bytes).unwrap_or(u64::MAX))
}

/// The bytes in one `unit` of the engine's sizes (decimal and binary).
fn unit_bytes(unit: &str) -> Option<u128> {
    Some(match unit {
        "B" => 1,
        "kB" | "KB" => 1_000,
        "KiB" => 1 << 10,
        "MB" => 1_000_000,
        "MiB" => 1 << 20,
        "GB" => 1_000_000_000,
        "GiB" => 1 << 30,
        "TB" => 1_000_000_000_000,
        "TiB" => 1 << 40,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATS: &str = include_str!("../../tool_output/fixtures/compose/29.9-stats.jsonl");

    #[test]
    fn each_running_containers_cpu_and_memory_are_read() {
        let stats = parse(STATS);
        assert_eq!(stats.len(), 3);
        assert_eq!(stats[0].id, "1f77731b33ce");
        assert_eq!(stats[0].cpu_percent, Some(1.78));
        assert_eq!(stats[0].memory_bytes, Some(10_768_875));
        assert_eq!(stats[1].cpu_percent, Some(0.0));
        assert_eq!(stats[1].memory_bytes, Some(1_499_463));
        assert!(stats[0].is("1f77731b33ce31d18de8e49eabcc73b83c68dafecd58d42ba781b4ef379f9a32"));
        assert!(!stats[0].is("fde9a446efcd") && !stats[0].is(""));
    }

    #[test]
    fn a_figure_the_engine_has_none_for_is_none_and_an_odd_line_is_skipped() {
        let out = "{\"ID\":\"aaa\",\"CPUPerc\":\"--\",\"MemUsage\":\"-- / --\"}\nnot json\n{\"CPUPerc\":\"1%\"}\n\n";
        assert_eq!(
            parse(out),
            [ContainerStats {
                id: "aaa".into(),
                cpu_percent: None,
                memory_bytes: None
            }]
        );
    }

    #[test]
    fn sizes_are_read_in_both_units_exactly() {
        assert_eq!(size("512B"), Some(512));
        assert_eq!(size("1.5GiB"), Some(3 * 512 * 1024 * 1024));
        assert_eq!(size(" 20MiB "), Some(20 * 1024 * 1024));
        assert_eq!((size(".5kB"), size("3.MB"), size("1TiB")), (Some(500), Some(3_000_000), Some(1 << 40)));
        for bad in ["", "--", ".", "MiB", "1.2.3MB", "-1MB", "1e3B", "7 parsecs"] {
            assert_eq!(size(bad), None, "{bad}");
        }
        assert_eq!((percent("250.5%"), percent("--"), percent("-1%")), (Some(250.5), None, None));
    }
}
