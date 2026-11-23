//! What `virsh` prints, read: the display `domdisplay` gives a libvirt domain.

use std::net::SocketAddr;

/// The TCP port of VNC display 0: display `n` listens on this plus `n`.
const VNC_DISPLAY_0: u16 = 5900;

/// The port VNC display `display` listens on; none past the last port.
pub(crate) fn vnc_port(display: u16) -> Option<u16> {
    VNC_DISPLAY_0.checked_add(display)
}

/// The VNC display listening on `port`; none for a port below display 0's.
pub(crate) fn vnc_display_number(port: u16) -> Option<u16> {
    port.checked_sub(VNC_DISPLAY_0)
}

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
    Some(SocketAddr::new(ip, vnc_port(display)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_display_and_its_port_convert_both_ways_within_the_ports() {
        assert_eq!((vnc_port(0), vnc_port(2)), (Some(5900), Some(5902)));
        assert_eq!(vnc_port(u16::MAX - 5899), None);
        assert_eq!((vnc_display_number(5900), vnc_display_number(5902)), (Some(0), Some(2)));
        // A port below display 0's is no display, not display 0.
        assert_eq!(vnc_display_number(5899), None);
    }

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
        assert_eq!(
            vnc_display(include_str!("fixtures/virsh/10.0.0-domdisplay-vnc.txt")),
            Some("127.0.0.1:5900".parse().unwrap()),
            "as virsh 10.0 prints it"
        );
        assert_eq!(vnc_display(include_str!("fixtures/virsh/10.0.0-domdisplay-spice.txt")), None, "no VNC display");
    }
}
