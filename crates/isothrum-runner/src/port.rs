//! Two kinds of port number, kept apart by type so one is never stored or reported where the
//! other is meant:
//!
//! - [`GuestPort`]: a port inside a machine (a container's target port, a VM's own port). Nothing
//!   on this computer answers at it as it is.
//! - [`HostPort`]: the port a published port is reached at, at the machine's host address. What
//!   status, the port reservations and the free-port check carry.
//!
//! A guest port turns into the host port it is reached at only through its driver: the host port
//! is the one its tool reports for it (Compose's `PublishedPort`, a Vagrant forward's host side),
//! read by that driver's adapter, never its own number reused. Each type serializes as the bare
//! number, and its schema is the bare number's, inlined, so the JSON and the schemas read as
//! they did.

use std::borrow::Cow;
use std::fmt;

use serde::{Deserialize, Serialize};

/// A port inside a machine: a container's target port, a VM's own port. Reached from this
/// computer only through the host port its driver gives it ([`HostPort`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GuestPort(u16);

/// The port a published port is reached at, at its machine's host address: on this computer's
/// loopback where the engine forwards it here (Compose, a local VM). Only a host port is
/// reserved, checked free or reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HostPort(u16);

impl GuestPort {
    /// The port `number` inside a machine (an engine's report, a test's value).
    #[must_use]
    pub const fn new(number: u16) -> GuestPort {
        GuestPort(number)
    }

    /// Its number.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
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

/// The bare number's schema, inlined where the port appears (no definition of its own), so a
/// schema that carries a port reads as it did with a `u16`.
macro_rules! number_schema {
    ($port:ident) => {
        impl schemars::JsonSchema for $port {
            fn inline_schema() -> bool {
                true
            }

            fn schema_name() -> Cow<'static, str> {
                Cow::Borrowed(stringify!($port))
            }

            fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
                u16::json_schema(generator)
            }
        }
    };
}

number_schema!(GuestPort);
number_schema!(HostPort);

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
