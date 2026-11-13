//! What QEMU says, read: its monitor's `info vnc`, and the monitor socket vagrant-qemu records in
//! its `options.yml`.

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
