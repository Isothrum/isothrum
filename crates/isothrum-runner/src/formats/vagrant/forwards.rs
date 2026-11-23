//! The ports a VM forwards from the host, where `vagrant port` can't list them (vagrant-qemu):
//! asked of its QEMU's monitor (`info usernet`), the socket vagrant-qemu records. vagrant-qemu
//! keeps no list of them of its own.

#[cfg(unix)]
use super::screen::monitor_socket;
use crate::events::WarningCode;
use crate::formats::{SeenPort, ToolCx};
#[cfg(unix)]
use crate::port::{GuestPort, HostPort};

/// The ports `machine`'s QEMU forwards from the host; none, warned (`ports_unknown`), when its
/// monitor can't say: the machine runs on, its ports unknown.
#[cfg(unix)]
pub(super) async fn of_qemu(cx: &ToolCx<'_>, machine: &str) -> Vec<SeenPort> {
    let asked = match monitor_socket(cx, machine) {
        Ok(socket) => crate::cli::qemu::host_forwards(&socket)
            .await
            .map_err(|e| format!("{} didn't answer: {e}", socket.display())),
        Err(e) => Err(e.to_string()),
    };
    match asked {
        Ok(forwards) => forwards
            .into_iter()
            .map(|f| SeenPort {
                guest: GuestPort::new(f.guest),
                host: HostPort::new(f.host),
                protocol: Some(f.protocol),
            })
            .collect(),
        Err(why) => {
            (cx.warn)(
                WarningCode::PortsUnknown,
                &format!("`{machine}`'s forwarded ports are unknown: its QEMU monitor {why}"),
            );
            Vec::new()
        }
    }
}

/// No QEMU monitor is in reach on a host without Unix sockets (vagrant-qemu doesn't run there).
#[cfg(not(unix))]
pub(super) fn of_qemu(cx: &ToolCx<'_>, machine: &str) -> impl Future<Output = Vec<SeenPort>> {
    (cx.warn)(WarningCode::PortsUnknown, &format!("`{machine}`'s forwarded ports are unknown on this host"));
    std::future::ready(Vec::new())
}
