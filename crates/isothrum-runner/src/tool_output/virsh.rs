//! What `virsh` prints, read: the display `domdisplay` gives a libvirt domain.

use std::net::SocketAddr;

/// The address of a `vnc://<host>:<display>` line `virsh domdisplay` prints (port 5900 plus the
/// display number), a bare IPv6 host in brackets.
pub(crate) fn vnc_display(said: &str) -> Option<SocketAddr> {
    let rest = said.lines().map(str::trim).find_map(|l| l.strip_prefix("vnc://"))?;
    let rest = rest.split(['/', '?']).next()?;
    let (host, display) = rest.rsplit_once(':')?;
    let display: u16 = display.parse().ok()?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let ip = if host.eq_ignore_ascii_case("localhost") {
        std::net::IpAddr::from([127, 0, 0, 1])
    } else {
        host.parse().ok()?
    };
    Some(SocketAddr::new(ip, 5900u16.checked_add(display)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virsh_says_a_vnc_display_by_its_number() {
        assert_eq!(vnc_display("vnc://127.0.0.1:0\n"), Some("127.0.0.1:5900".parse().unwrap()));
        assert_eq!(vnc_display("vnc://localhost:3"), Some("127.0.0.1:5903".parse().unwrap()));
        assert_eq!(vnc_display("vnc://[::1]:1"), Some("[::1]:5901".parse().unwrap()));
        assert_eq!(
            vnc_display("vnc://0.0.0.0:2"),
            Some("0.0.0.0:5902".parse().unwrap()),
            "read as said; refused later"
        );
        assert_eq!(vnc_display("spice://127.0.0.1:5930"), None);
        assert_eq!(vnc_display(""), None);
    }

    #[test]
    fn fixtures_read_as_virsh_prints_them() {
        assert_eq!(
            vnc_display(include_str!("fixtures/virsh/10.0.0-domdisplay-vnc.txt")),
            Some("127.0.0.1:5900".parse().unwrap())
        );
        assert_eq!(vnc_display(include_str!("fixtures/virsh/10.0.0-domdisplay-spice.txt")), None, "no VNC display");
    }
}
