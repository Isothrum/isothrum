//! What QEMU says, read: its monitor's `info vnc` and `info usernet`, and the monitor socket
//! vagrant-qemu records in its `options.yml`.

#[cfg(any(unix, test))]
use std::net::SocketAddr;
use std::path::PathBuf;

/// The `:monitor_socket:` vagrant-qemu's `options.yml` records.
pub(crate) fn recorded_socket(options: &str) -> Option<PathBuf> {
    options.lines().find_map(|l| {
        let path = l.trim().strip_prefix(":monitor_socket:")?.trim().trim_matches(['"', '\'']);
        (!path.is_empty() && path != "~").then(|| PathBuf::from(path))
    })
}

/// The address `info vnc` names its server at (`Server: 127.0.0.1:5900 (ipv4)`, or an older
/// QEMU's `address: 127.0.0.1:5900`).
#[cfg(any(unix, test))]
pub(crate) fn vnc_server(said: &str) -> Option<SocketAddr> {
    said.lines().find_map(|l| {
        let l = l.trim();
        let rest = l.strip_prefix("Server:").or_else(|| l.strip_prefix("address:"))?.trim();
        rest.split_whitespace().next()?.parse().ok()
    })
}

/// One port QEMU's user network forwards from the host to the guest (`hostfwd`).
#[cfg(any(unix, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostForward {
    /// `tcp` or `udp`.
    pub(crate) protocol: crate::project::platform::Protocol,
    /// The host's port.
    pub(crate) host: u16,
    /// The guest's.
    pub(crate) guest: u16,
}

/// The host forwards `info usernet` lists (its `TCP[HOST_FORWARD]` and `UDP[HOST_FORWARD]` rows:
/// protocol, descriptor, source address and port, destination address and port).
#[cfg(any(unix, test))]
pub(crate) fn host_forwards(said: &str) -> Vec<HostForward> {
    use crate::project::platform::Protocol;
    said.lines()
        .filter_map(|l| {
            let words: Vec<&str> = l.split_whitespace().collect();
            let protocol = match *words.first()? {
                "TCP[HOST_FORWARD]" => Protocol::Tcp,
                "UDP[HOST_FORWARD]" => Protocol::Udp,
                _ => return None,
            };
            Some(HostForward {
                protocol,
                host: words.get(3)?.parse().ok()?,
                guest: words.get(5)?.parse().ok()?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qemu_says_its_vnc_server_in_either_format() {
        let new = "info vnc\r\ndefault:\r\n  Server: 127.0.0.1:5901 (ipv4)\r\n    Auth: none (Sub: none)\r\n(qemu) ";
        assert_eq!(vnc_server(new), Some("127.0.0.1:5901".parse().unwrap()));
        let old = "Server:\n     address: 127.0.0.1:5900\n        auth: none\n";
        assert_eq!(vnc_server(old), Some("127.0.0.1:5900".parse().unwrap()));
        assert_eq!(vnc_server("default:\n  Server: none\n"), None);
    }

    #[test]
    fn vagrant_qemu_records_its_monitor_socket() {
        let options = "---\n:ssh_port: 50022\n:control_port:\n:monitor_socket: \"/Users/u/.vagrant.d/tmp/vagrant-qemu/abc/qemu_socket\"\n:serial_socket: x\n";
        assert_eq!(
            recorded_socket(options),
            Some(PathBuf::from("/Users/u/.vagrant.d/tmp/vagrant-qemu/abc/qemu_socket"))
        );
        assert_eq!(recorded_socket(":monitor_socket:\n"), None, "a control port instead");
        assert_eq!(recorded_socket(""), None);
    }

    #[test]
    fn the_user_networks_host_forwards_are_read_and_its_connections_left_out() {
        use crate::project::platform::Protocol;
        let fw = |protocol, host, guest| HostForward { protocol, host, guest };
        assert_eq!(
            host_forwards(include_str!("fixtures/qemu/8.2-info-usernet.txt")),
            [fw(Protocol::Tcp, 8080, 80), fw(Protocol::Udp, 5353, 53), fw(Protocol::Tcp, 50022, 22)]
        );
        assert_eq!(host_forwards("info usernet\r\n(qemu) "), []);
    }

    #[test]
    fn fixtures_read_as_qemu_prints_them() {
        assert_eq!(
            vnc_server(include_str!("fixtures/qemu/8.2-info-vnc.txt")),
            Some("127.0.0.1:5901".parse().unwrap())
        );
        assert_eq!(
            vnc_server(include_str!("fixtures/qemu/6.2-info-vnc.txt")),
            Some("127.0.0.1:5900".parse().unwrap())
        );
    }
}
