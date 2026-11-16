//! The claims whose witness is owed, apart from the table of those shown ([`super::claims`]).

/// The claims whose witness is owed: each a test to write (a scripted process seeing the call),
/// and the list only shrinks. A claim added to a driver never goes here: it comes with its test.
pub(super) const OWED: [(&str, &str, &str); 3] = [
    ("vagrant", "native_console", "a VM's own window opened through its provider's app"),
    (
        "terraform",
        "reach_graphical",
        "a Proxmox VE VM's VNC websocket asked of its API (curl's vncproxy) from the module's state",
    ),
    ("terraform", "native_console", "a Proxmox VE VM's web console address from the module's state"),
];
