//! Three kinds of port number, kept apart by type so one is never stored or reported where
//! another is meant:
//!
//! - [`GuestPort`]: a port inside a machine (a container's target port, a VM's own port, the
//!   port Compose publishes inside the run's VM). Nothing on this computer answers at it as it is.
//! - [`HostPort`]: the port a published port is reached at, at the machine's host address
//!   (`${host.<machine>}`: this computer's loopback where the engine forwards it here, the VM's
//!   own address where it doesn't). What status, the outputs, `${published.…}`, the port
//!   reservations and the free-port check carry.
//! - `PreferredPort` (crate-private): the host port a manifest asks for (`publish.preferred`). A
//!   wish: it becomes a [`HostPort`] only through a pick that found it free, or as the number a
//!   forward takes when nothing was picked.
//!
//! A guest port turns into the host port it is reached at through its driver's one function
//! (`AnyDriver::reached_at`), never by reading its number. Each type serializes as the bare number,
//! so the JSON and the schemas read as they did.

use std::fmt;

use isoloom_manifest::Port;
use serde::{Deserialize, Serialize};

/// A port inside a machine: a container's target port, a VM's own port. Reached from this
/// computer only through the host port its driver gives it ([`HostPort`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct GuestPort(u16);

/// The port a published port is reached at, at its machine's host address: on this computer's
/// loopback where the engine forwards it here (Compose, a local VM), at the VM's own address
/// where it doesn't (ESXi, a server). Only a host port is reserved, checked free or reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct HostPort(u16);

/// The host port a manifest's publish asks for (`preferred`): taken only when free, so not a
/// [`HostPort`] until a pick says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct PreferredPort(u16);

impl GuestPort {
    /// The port `number` inside a machine (an engine's report, a test's value).
    #[must_use]
    pub const fn new(number: u16) -> GuestPort {
        GuestPort(number)
    }

    /// The manifest's port `p`: the machine's own.
    pub(crate) fn of(p: &Port) -> GuestPort {
        GuestPort(p.port.get())
    }

    /// Its number.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// The host port it is reached at where its machine answers at its own address (an ESXi VM,
    /// a server's VM): the same number, at the machine's address. Only a driver whose machines
    /// answer there calls this; a driver that forwards ports gives the forward's.
    pub(crate) const fn at_own_address(self) -> HostPort {
        HostPort(self.0)
    }

    /// The host port a forward takes it from when nothing named one (no pick, no preferred
    /// number): a Vagrantfile forwards the port from its own number.
    pub(crate) const fn forwarded_from_its_own_number(self) -> HostPort {
        HostPort(self.0)
    }
}

impl HostPort {
    /// The port `number` on the host (a socket bound, an engine's report, a recorded pick).
    #[must_use]
    pub const fn new(number: u16) -> HostPort {
        HostPort(number)
    }

    /// Its number.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl PreferredPort {
    /// What the manifest's port `p` asks for, when it is published and names a number.
    pub(crate) fn of(p: &Port) -> Option<PreferredPort> {
        p.publish.as_ref()?.preferred.map(|n| PreferredPort(n.get()))
    }

    /// The host port it is, once a pick found it free and reserved it for the run.
    pub(crate) const fn picked(self) -> HostPort {
        HostPort(self.0)
    }

    /// The host port a forward takes when the run recorded no pick for it (the Vagrantfile's
    /// fallback): the number as asked.
    pub(crate) const fn unpicked(self) -> HostPort {
        HostPort(self.0)
    }
}

impl fmt::Display for GuestPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for HostPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for PreferredPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_reads_and_writes_as_the_bare_number() {
        assert_eq!(serde_json::to_string(&HostPort::new(41873)).unwrap(), "41873");
        assert_eq!(serde_json::to_string(&GuestPort::new(80)).unwrap(), "80");
        assert_eq!(serde_json::from_str::<HostPort>("8080").unwrap(), HostPort::new(8080));
    }

    #[test]
    fn each_kind_has_the_bare_numbers_schema() {
        let number = serde_json::to_value(schemars::schema_for!(u16)).unwrap();
        for schema in [schemars::schema_for!(HostPort), schemars::schema_for!(GuestPort)] {
            let mut schema = serde_json::to_value(schema).unwrap();
            let mut number = number.clone();
            for s in [&mut schema, &mut number] {
                let fields = s.as_object_mut().unwrap();
                fields.remove("title");
                fields.remove("description");
            }
            assert_eq!(schema, number);
        }
    }
}
