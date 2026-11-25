//! What `ping` prints, read: whether the address answered. Its fixtures are in `fixtures/ping/`.
//!
//! Windows' `ping.exe` exits 0 when a router answers `Destination host unreachable` for the
//! address, so its exit code says nothing: a reply is a line with the reply's `TTL=` (`ttl=` on
//! Linux and macOS), which every language's `ping` prints and an unreachable answer never does.

use crate::Context;

/// Whether one `ping` that printed `out` got a reply.
pub(super) fn replied(out: &str) -> bool {
    out.lines().any(|l| l.to_ascii_lowercase().contains("ttl="))
}

/// One ping with a 3 s wait, in this OS's flags.
fn ping_args(cx: &Context, address: &str) -> Vec<String> {
    let flags: &[&str] = match cx.host.os {
        isothrum_runner::HostOs::Windows => &["-n", "1", "-w", "3000"],
        isothrum_runner::HostOs::Macos => &["-c", "1", "-t", "3"],
        isothrum_runner::HostOs::Linux => &["-c", "1", "-W", "3"],
    };
    flags.iter().copied().chain([address]).map(str::to_owned).collect()
}

/// What one ping of `address` printed; `Err` when `ping` couldn't run.
pub(super) async fn ping_once(cx: &Context, address: &str) -> Result<String, String> {
    let spec = isothrum_runner::ProcessSpec::new("ping", ping_args(cx, address));
    let captured = cx.probe_within(cx.probe_timeout, &spec).await.map_err(|e| e.to_string())?;
    Ok(crate::exec::all_lines(&captured))
}

/// Whether `address` answers one ping.
pub(super) async fn answers_ping(cx: &Context, address: &str) -> bool {
    ping_once(cx, address).await.is_ok_and(|out| replied(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_read_as_each_ping_prints_them() {
        for (fixture, reply) in [
            (include_str!("fixtures/ping/iputils-20240117-reply.txt"), true),
            (include_str!("fixtures/ping/macos-15-reply.txt"), true),
            (include_str!("fixtures/ping/windows-11-reply.txt"), true),
            (include_str!("fixtures/ping/windows-11-fr-reply.txt"), true),
            (include_str!("fixtures/ping/windows-11-unreachable.txt"), false),
            (include_str!("fixtures/ping/windows-11-fr-unreachable.txt"), false),
            (include_str!("fixtures/ping/windows-11-timeout.txt"), false),
            ("", false),
        ] {
            assert_eq!(replied(fixture), reply, "{fixture}");
        }
    }
}
