//! `docker compose config --format json`, read: the ports each service publishes, parsed once at
//! the boundary into typed fields (unknown fields skipped), and the override that pins those
//! Docker would give an ephemeral host port.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::{Failure, Project, TOOL};
use crate::cli::{Call, CliError, argv};
use crate::project::platform::Protocol;

/// What this adapter reads of the config.
#[derive(Debug, Deserialize)]
struct ComposeConfig {
    services: BTreeMap<String, Service>,
}

/// A service of the config: its published ports.
#[derive(Debug, Default, Deserialize)]
struct Service {
    #[serde(default)]
    ports: Vec<PortEntry>,
}

/// A published port as the config writes it (numbers, or strings holding them).
#[derive(Debug, Deserialize)]
struct PortEntry {
    target: Option<PortNumber>,
    protocol: Option<String>,
    host_ip: Option<String>,
    published: Option<PortNumber>,
}

/// A port number the config writes as a number or as a string.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PortNumber {
    Number(u64),
    Text(String),
}

impl PortNumber {
    fn as_u16(&self) -> Option<u16> {
        let n = match self {
            PortNumber::Number(n) => *n,
            PortNumber::Text(s) => s.parse().ok()?,
        };
        u16::try_from(n).ok().filter(|n| *n > 0)
    }
}

/// One port a service publishes: its container port, protocol, host address, and host port
/// (`None`: ephemeral, Docker picks one each time the container starts).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, Deserialize)]
pub(crate) struct ConfigPort {
    pub(crate) service: String,
    pub(crate) target: u16,
    pub(crate) protocol: Protocol,
    pub(crate) host_ip: Option<String>,
    pub(crate) published: Option<u16>,
}

/// The ports `config` (what `config --format json` printed) publishes, or why it can't be read:
/// not the JSON Compose prints, or a port with no target or another protocol than tcp and udp
/// (the whole config is then unreadable: no half pinning).
pub(crate) fn ports_of(config: &str) -> Result<Vec<ConfigPort>, String> {
    let read: ComposeConfig = serde_json::from_str(config).map_err(|e| format!("not the config Compose prints: {e}"))?;
    let mut out = Vec::new();
    for (service, svc) in read.services {
        for p in svc.ports {
            let protocol = match p.protocol.as_deref().unwrap_or("tcp") {
                "tcp" => Protocol::Tcp,
                "udp" => Protocol::Udp,
                other => return Err(format!("a port of `{service}` has the protocol `{other}`, neither tcp nor udp")),
            };
            out.push(ConfigPort {
                target: p
                    .target
                    .as_ref()
                    .and_then(PortNumber::as_u16)
                    .ok_or_else(|| format!("a port of `{service}` has no target"))?,
                protocol,
                host_ip: p.host_ip.filter(|i| !i.is_empty()),
                published: p.published.as_ref().and_then(PortNumber::as_u16),
                service: service.clone(),
            });
        }
    }
    Ok(out)
}

/// The ports `project` publishes, as `config --format json` says: of the services it starts, or
/// of `services` alone when given (a service under a profile is in it when named).
pub(crate) async fn ports(call: &Call<'_>, project: &Project<'_>, services: &[String]) -> Result<Vec<ConfigPort>, CliError<Failure>> {
    let mut args = argv(["config", "--format", "json"]);
    args.extend(services.iter().cloned());
    let out = call.ok(TOOL, project.args(args), Failure::of).await?;
    ports_of(&out.stdout.join("\n")).map_err(|what| CliError::Unexpected { tool: TOOL, what })
}

/// The override that pins `ports`' ephemeral ones to `pinned` (each one's host port, by its
/// place in `ports`): each service with one gets its whole `ports` list again (`!override`, or
/// Compose would add the entries). `None` when no port is ephemeral.
pub(crate) fn pins_override(ports: &[ConfigPort], host_of: impl Fn(&ConfigPort) -> Option<u16>) -> Option<String> {
    let mut services: Vec<&str> = Vec::new();
    for p in ports.iter().filter(|p| p.published.is_none()) {
        if !services.contains(&p.service.as_str()) {
            services.push(&p.service);
        }
    }
    if services.is_empty() {
        return None;
    }
    let mut lines = vec![
        "# Written by the Isothrum runner: the run's host ports, kept until it is brought down.".to_owned(),
        "services:".to_owned(),
    ];
    for service in services {
        lines.push(format!("  {}:", yaml_str(service)));
        lines.push("    ports: !override".to_owned());
        for p in ports.iter().filter(|p| p.service == service) {
            let Some(host) = p.published.or_else(|| host_of(p)) else { continue };
            // An IPv6 address goes in brackets, or its colons read as the port separators.
            let ip = p
                .host_ip
                .as_deref()
                .map(|i| if i.contains(':') { format!("[{i}]:") } else { format!("{i}:") })
                .unwrap_or_default();
            let protocol = crate::encoding::wire_name(&p.protocol);
            lines.push(format!("      - {}", yaml_str(&format!("{ip}{host}:{}/{protocol}", p.target))));
        }
    }
    Some(lines.join("\n") + "\n")
}

/// A double-quoted YAML scalar: JSON's quoting, every non-ASCII character escaped too (YAML reads
/// NEL, LS and PS as line breaks).
fn yaml_str(s: &str) -> String {
    let mut out = String::new();
    for c in serde_json::Value::from(s).to_string().chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            for unit in c.encode_utf16(&mut [0; 2]) {
                out.extend(format!("\\u{unit:04x}").chars());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What Docker Compose 5.6 printed of a service publishing `"80"`, `"127.0.0.1::443"`,
    /// `"8080:8080"` and `"[::1]::53/udp"`.
    const CONFIG: &str = include_str!("../../tool_output/fixtures/compose/5.6-config-ports.json");

    #[test]
    fn the_configs_ports_are_read_and_the_ephemeral_ones_pinned_with_the_rest_kept() {
        let ports = ports_of(CONFIG).expect("read");
        let ephemeral: Vec<(u16, Protocol, Option<&str>)> = ports
            .iter()
            .filter(|p| p.published.is_none())
            .map(|p| (p.target, p.protocol, p.host_ip.as_deref()))
            .collect();
        assert_eq!(
            ephemeral,
            [
                (80, Protocol::Tcp, None),
                (443, Protocol::Tcp, Some("127.0.0.1")),
                (53, Protocol::Udp, Some("::1"))
            ]
        );
        let pinned = pins_override(&ports, |p| Some(41000 + p.target)).expect("an override");
        assert_eq!(
            pinned,
            "# Written by the Isothrum runner: the run's host ports, kept until it is brought down.\nservices:\n  \"web\":\n    ports: !override\n      - \"41080:80/tcp\"\n      - \"127.0.0.1:41443:443/tcp\"\n      - \"8080:8080/tcp\"\n      - \"[::1]:41053:53/udp\"\n"
        );
        assert_eq!(pins_override(&[], |_| None), None, "nothing ephemeral");
        assert!(ports_of("{\"services\": 3}").is_err() && ports_of("not json").is_err());
    }
}
