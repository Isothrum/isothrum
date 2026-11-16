//! The host ports a run keeps: what each machine published at the run's first start, kept in the
//! run's state folder until `down`. A start, resume or update that finds one moved says so
//! (`ports_moved`). A Vagrant run's tools get them back as variables
//! (`HOST_PORT_<MACHINE>_<GUEST>_<PROTO>`, [`var_name`]): Vagrant's providers re-pick a
//! port its `auto_correct` moved at each boot (VirtualBox excepted), and no provider takes a
//! forwarded port's host side from outside the Vagrantfile, so a Vagrantfile that wants stable
//! ports reads them (`host: ENV.fetch("HOST_PORT_WEB_80_TCP", 8080).to_i`). Nothing
//! requires it: a Vagrantfile that ignores them runs as it would, and is warned when they move.

use std::path::Path;

use crate::Result;
use crate::events::WarningCode;
use crate::formats::Seen;
use crate::project::platform::Protocol;

/// The run's kept ports, in its state folder.
const FILE: &str = "host-ports.json";

/// One kept port: the machine, its guest port and protocol, and the host port it is reached at.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Kept {
    pub(crate) machine: String,
    pub(crate) guest: u16,
    pub(crate) protocol: Protocol,
    pub(crate) host: u16,
}

/// The ports the run in `state_dir` keeps; none before its first start.
pub(super) fn kept(state_dir: &Path) -> Result<Vec<Kept>> {
    crate::store::read_json(&state_dir.join(FILE))
}

/// Keeps what `seen` publishes: a port not kept yet is added; one kept whose host port moved is
/// said (`warn`, `ports_moved`) and kept at its new place, where the run is now reached. A
/// machine that publishes nothing now (parked) changes nothing.
pub(super) fn keep(state_dir: &Path, seen: &Seen, warn: &(dyn Fn(WarningCode, &str) + Send + Sync)) -> Result<()> {
    let mut kept = kept(state_dir)?;
    let before = kept.clone();
    for machine in &seen.machines {
        for p in &machine.published {
            let (guest, protocol, host) = (p.guest.get(), p.protocol.unwrap_or(Protocol::Tcp), p.host.get());
            match kept
                .iter_mut()
                .find(|k| k.machine == machine.name && k.guest == guest && k.protocol == protocol)
            {
                Some(k) if k.host != host => {
                    let proto = crate::encoding::wire_name(&protocol);
                    warn(
                        WarningCode::PortsMoved,
                        &format!("`{}`'s port {guest}/{proto} moved from host port {} to {host}", machine.name, k.host),
                    );
                    k.host = host;
                }
                Some(_) => {}
                None => kept.push(Kept {
                    machine: machine.name.clone(),
                    guest,
                    protocol,
                    host,
                }),
            }
        }
    }
    if kept == before {
        return Ok(());
    }
    crate::store::write_json(&state_dir.join(FILE), &kept)
}

/// Drops the run's kept ports: they go with it at `down`.
pub(super) fn forget(state_dir: &Path) -> Result<()> {
    crate::store::remove(&state_dir.join(FILE))
}

/// The variable a kept port of `machine` is given as: `HOST_PORT_`, the machine's name
/// upper-cased with every character but `A`–`Z` and `0`–`9` written `_`, `_`, the guest port in
/// decimal, `_`, and `TCP` or `UDP` (`web-1`'s 80/tcp: `HOST_PORT_WEB_1_80_TCP`).
pub(crate) fn var_name(machine: &str, guest: u16, protocol: Protocol) -> String {
    let name: String = machine
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' })
        .collect();
    let proto = match protocol {
        Protocol::Tcp => "TCP",
        Protocol::Udp => "UDP",
    };
    format!("HOST_PORT_{name}_{guest}_{proto}")
}

/// The name a kept port's variable had before [`var_name`]'s, set beside it until the
/// Vagrantfiles that read it read the new one.
const LEGACY_PREFIX: &str = "ISOTHRUM_";

/// The variables `kept` ports are given as ([`var_name`]), each to its host port. Two machines
/// whose names give one variable (`web-1` and `web_1`) give none: which machine it meant would be
/// a guess.
pub(super) fn env(kept: &[Kept]) -> Vec<(String, String)> {
    let named: Vec<(String, &Kept)> = kept.iter().map(|k| (var_name(&k.machine, k.guest, k.protocol), k)).collect();
    named
        .iter()
        .filter(|(name, k)| !named.iter().any(|(other, o)| other == name && o.machine != k.machine))
        .flat_map(|(name, k)| {
            let host = k.host.to_string();
            [(name.clone(), host.clone()), (format!("{LEGACY_PREFIX}{name}"), host)]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(machine: &str, guest: u16, host: u16) -> Kept {
        Kept {
            machine: machine.to_owned(),
            guest,
            protocol: Protocol::Tcp,
            host,
        }
    }

    #[test]
    fn a_kept_ports_variable_names_its_machine_guest_port_and_protocol() {
        assert_eq!(var_name("web", 80, Protocol::Tcp), "HOST_PORT_WEB_80_TCP");
        assert_eq!(var_name("web-1.stack", 53, Protocol::Udp), "HOST_PORT_WEB_1_STACK_53_UDP");
        assert_eq!(var_name("web·1", 8080, Protocol::Tcp), "HOST_PORT_WEB_1_8080_TCP", "one `_` per character");
        let vars = env(&[k("web", 80, 2200), k("web-1", 22, 2222), k("web_1", 22, 2223)]);
        assert_eq!(
            vars,
            [
                ("HOST_PORT_WEB_80_TCP".to_owned(), "2200".to_owned()),
                ("ISOTHRUM_HOST_PORT_WEB_80_TCP".to_owned(), "2200".to_owned())
            ],
            "web-1 and web_1 collide: neither"
        );
    }
}
