//! What `ping` prints, read: whether the address answered. Its fixtures are in `fixtures/ping/`.
//!
//! Windows' `ping.exe` exits 0 when a router answers `Destination host unreachable` for the
//! address, so its exit code says nothing: a reply is a line with the reply's `TTL=` (`ttl=` on
//! Linux and macOS), which every language's `ping` prints and an unreachable answer never does.

/// Whether one `ping` that printed `out` got a reply.
pub(super) fn replied(out: &str) -> bool {
    out.lines().any(|l| l.to_ascii_lowercase().contains("ttl="))
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
