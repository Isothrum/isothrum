//! When a process started, as the host stored it: what tells a process from another that took its id.

use super::Start;
#[cfg(windows)]
use super::headless;

/// The start to record for the tool `pid` that was just started.
pub(super) fn recorded_start(pid: u32) -> Option<Start> {
    if cfg!(target_os = "linux") {
        return proc_start(pid);
    }
    now_secs().map(Start::At)
}

/// This computer's clock now, in seconds since the epoch.
pub(super) fn now_secs() -> Option<u64> {
    Some(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs())
}

/// Process `pid`'s start on Linux: field 22 of `/proc/<pid>/stat` (after its name, which may
/// hold spaces and parentheses), and this boot's id.
pub(super) fn proc_start(pid: u32) -> Option<Start> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    Some(Start::Boot {
        boot: boot.trim().to_owned(),
        ticks: stat_ticks(&stat)?,
    })
}

/// The start in a `/proc/<pid>/stat` line, in clock ticks since boot.
pub(super) fn stat_ticks(stat: &str) -> Option<u64> {
    /// `starttime` is field 22 (from 1); the first field after the name is field 3.
    const STARTTIME_AFTER_NAME: usize = 22 - 3;
    stat.rsplit_once(')')?.1.split_whitespace().nth(STARTTIME_AFTER_NAME)?.parse().ok()
}

/// Process `pid`'s start as the OS has it now; none when there is no such process.
pub(super) async fn os_start(pid: u32) -> Option<Start> {
    if cfg!(target_os = "linux") {
        return proc_start(pid);
    }
    os_started_at(pid).await.map(Start::At)
}

/// `ps` with `args`, in UTC and the C locale's words (how `lstart` is read back), its error
/// output dropped.
#[cfg(unix)]
#[expect(
    clippy::disallowed_methods,
    reason = "the process module's own reading of processes and their starts, under the runner it serves"
)]
pub(super) async fn ps(args: &[&str]) -> std::io::Result<std::process::Output> {
    tokio::process::Command::new("ps")
        .args(args)
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .await
}

/// When process `pid` started, in seconds since the epoch, as `ps` reads the start the kernel
/// stored (`lstart`, in UTC and the C locale's words).
#[cfg(unix)]
pub(crate) async fn os_started_at(pid: u32) -> Option<u64> {
    let out = ps(&["-o", "lstart=", "-p", &pid.to_string()]).await.ok()?;
    out.status.success().then(|| lstart(&String::from_utf8_lossy(&out.stdout))).flatten()
}

/// When process `pid` started, as Windows stored its creation time (seconds since the epoch).
#[cfg(windows)]
#[expect(
    clippy::disallowed_methods,
    reason = "the process module's own reading of a process's start time, under the runner it serves"
)]
pub(crate) async fn os_started_at(pid: u32) -> Option<u64> {
    let script = format!("[DateTimeOffset]::new((Get-Process -Id {pid} -ErrorAction Stop).StartTime).ToUnixTimeSeconds()");
    let mut cmd = tokio::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(std::process::Stdio::null());
    headless(&mut cmd);
    let out = cmd.output().await.ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok()).flatten()
}

/// `ps`'s `lstart` (`Wed Nov 11 16:43:35 2026`) as seconds since the epoch.
#[cfg(any(unix, test))]
pub(super) fn lstart(text: &str) -> Option<u64> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    const SECS_PER_MINUTE: u64 = 60;
    const SECS_PER_HOUR: u64 = 60 * SECS_PER_MINUTE;
    const SECS_PER_DAY: u64 = 24 * SECS_PER_HOUR;
    let words: Vec<&str> = text.split_whitespace().collect();
    let [_, month, day, time, year] = words.as_slice() else { return None };
    let month = i64::try_from(MONTHS.iter().position(|m| m == month)? + 1).ok()?;
    let (day, year): (i64, i64) = (day.parse().ok()?, year.parse().ok()?);
    let hms: Vec<u64> = time.split(':').map(str::parse).collect::<Result<_, _>>().ok()?;
    // A leap second reads as 60.
    let [h @ 0..24, m @ 0..60, s @ 0..=60] = hms.as_slice() else { return None };
    Some(crate::clock::days_from_civil(year, month, day)? * SECS_PER_DAY + h * SECS_PER_HOUR + m * SECS_PER_MINUTE + s)
}
