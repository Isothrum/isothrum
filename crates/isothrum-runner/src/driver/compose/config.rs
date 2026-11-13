//! Reading `docker compose config --format json`, and writing the pinned-ports override.

use indexmap::IndexMap;
use isoloom_manifest::Protocol;
use serde::Deserialize;
use serde_json::Value;

use crate::encoding::wire_name;
use crate::error::Error;

/// What this runner reads of `docker compose config --format json`: parsed once, at the boundary,
/// into the fields it uses, where four readers walked the JSON by hand. Unknown fields are
/// skipped.
#[derive(Debug, Deserialize)]
struct ComposeConfig {
    /// In the config's order, as Compose prints them.
    services: IndexMap<String, Service>,
    #[serde(default)]
    networks: IndexMap<String, Network>,
}

/// A service of the config: its published ports and whose network namespace it shares.
#[derive(Debug, Default, Deserialize)]
struct Service {
    #[serde(default)]
    ports: Vec<PortEntry>,
    network_mode: Option<String>,
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
    /// The port, when it is one.
    fn as_u16(&self) -> Option<u16> {
        let n = match self {
            PortNumber::Number(n) => *n,
            PortNumber::Text(s) => s.parse().ok()?,
        };
        u16::try_from(n).ok()
    }
}

/// A network of the config: its Docker name, when the config gives one.
#[derive(Debug, Deserialize)]
struct Network {
    name: Option<String>,
}

/// The config printed by `docker compose config --format json`, or why it isn't that JSON (not
/// JSON at all, no `services` object): what it holds is then unknown, never empty.
fn read(config: &str) -> Result<ComposeConfig, String> {
    let v: Value = serde_json::from_str(config).map_err(|e| format!("not JSON: {e}"))?;
    if !v.get("services").is_some_and(Value::is_object) {
        return Err("no `services` object".to_owned());
    }
    serde_json::from_value(v).map_err(|e| format!("not the config Compose prints: {e}"))
}

/// The failure of `step` whose config couldn't be read, `why` saying how ([`read`], [`ports`]):
/// `what` is what is then unknown. A caller that acted on an empty reading instead would act
/// blind (remove a live network, let a taken port through, start a machine without its routes).
pub(super) fn unreadable(step: &str, what: &str, why: &str) -> Error {
    Error::ToolFailed {
        step: step.to_owned(),
        reason: format!("`docker compose config --format json` printed no config this runner can read ({why}), so {what} unknown"),
    }
}

/// One published port of the config: the service, its container port, the protocol, the host
/// address and the host port (`None`: ephemeral, Docker picks one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConfigPort {
    pub(super) service: String,
    pub(super) target: u16,
    pub(super) protocol: Protocol,
    pub(super) host_ip: Option<String>,
    pub(super) published: Option<u16>,
}

/// The config's published ports, or why the config can't be read: it isn't the JSON Compose
/// prints, or a port has no readable target or protocol, which makes the whole config unreadable
/// (no half override).
pub(super) fn ports(config: &str) -> Result<Vec<ConfigPort>, String> {
    let read = read(config)?;
    let mut out = Vec::new();
    for (service, svc) in &read.services {
        for p in &svc.ports {
            let said = p.protocol.as_deref().unwrap_or_default();
            out.push(ConfigPort {
                service: service.clone(),
                target: p
                    .target
                    .as_ref()
                    .and_then(PortNumber::as_u16)
                    .filter(|t| *t > 0)
                    .ok_or_else(|| format!("a port of `{service}` has no target"))?,
                protocol: super::protocol(said).ok_or_else(|| format!("a port of `{service}` has the protocol `{said}`, neither tcp nor udp"))?,
                host_ip: p.host_ip.as_deref().filter(|i| !i.is_empty()).map(str::to_owned),
                published: p.published.as_ref().and_then(PortNumber::as_u16).filter(|n| *n > 0),
            });
        }
    }
    Ok(out)
}

/// The host ports the config publishes, each with its protocol, or why the config can't be read
/// ([`ports`]).
pub(super) fn host_ports(config: &str) -> Result<Vec<(u16, Protocol)>, String> {
    Ok(ports(config)?.iter().filter_map(|p| Some((p.published?, p.protocol))).collect())
}

/// The services that live in `service`'s network namespace (`network_mode: service:<service>`):
/// part of that machine (its routes, a sidecar), started and removed with it. In name order; or
/// why the config isn't the JSON Compose prints ([`read`]): unknown, not none.
pub(super) fn sharing_namespace(config: &str, service: &str) -> Result<Vec<String>, String> {
    let read = read(config)?;
    let mode = format!("service:{service}");
    let mut names: Vec<String> = read
        .services
        .iter()
        .filter(|(_, s)| s.network_mode.as_deref() == Some(mode.as_str()))
        .map(|(name, _)| name.clone())
        .collect();
    names.sort();
    Ok(names)
}

/// The networks the config defines, by their Docker names (`name:`, else `<project>_<key>`), or
/// why the config can't be read ([`read`]): unknown, not none.
pub(super) fn networks(config: &str, project: &str) -> Result<Vec<String>, String> {
    let read = read(config)?;
    Ok(read
        .networks
        .iter()
        .map(|(k, n)| n.name.clone().unwrap_or_else(|| format!("{project}_{k}")))
        .collect())
}

/// The pinned-ports override: every service with an ephemeral port gets its whole `ports` list
/// again (`!override`: Compose would otherwise add the entries), each ephemeral one with the port
/// `pick` gives (container port, protocol). `None` when nothing is ephemeral; the first failed
/// pick's error when one fails.
pub(super) fn pinned_ports<E>(ports: &[ConfigPort], mut pick: impl FnMut(&ConfigPort) -> Result<u16, E>) -> Result<Option<String>, E> {
    let mut services: Vec<&str> = Vec::new();
    for p in ports.iter().filter(|p| p.published.is_none()) {
        if !services.contains(&p.service.as_str()) {
            services.push(&p.service);
        }
    }
    if services.is_empty() {
        return Ok(None);
    }
    let mut lines = vec![
        "# Written by the Isothrum runner at the run's first start: its host ports, kept across resumes.".to_owned(),
        "services:".to_owned(),
    ];
    for service in services {
        lines.push(format!("  {}:", yaml_str(service)));
        lines.push("    ports: !override".to_owned());
        for p in ports.iter().filter(|p| p.service == service) {
            let host = match p.published {
                Some(n) => n,
                None => pick(p)?,
            };
            // An IPv6 address goes in brackets, or its colons read as the port separators.
            let ip = p
                .host_ip
                .as_deref()
                .map(|i| if i.contains(':') { format!("[{i}]:") } else { format!("{i}:") })
                .unwrap_or_default();
            let proto = wire_name(&p.protocol);
            lines.push(format!("      - {}", yaml_str(&format!("{ip}{host}:{}/{proto}", p.target))));
        }
    }
    Ok(Some(lines.join("\n") + "\n"))
}

/// A double-quoted YAML scalar. JSON's quoting does most of it, but YAML also reads NEL, LS and
/// PS (U+0085, U+2028, U+2029) as line breaks, so every non-ASCII character is escaped too.
fn yaml_str(s: &str) -> String {
    let mut out = String::new();
    for c in Value::from(s).to_string().chars() {
        let n = u32::from(c);
        if n < 0x80 {
            out.push(c);
            continue;
        }
        let (escape, digits) = if n <= 0xFFFF { ("\\u", 4) } else { ("\\U", 8) };
        out.push_str(escape);
        out.extend((0..digits).rev().filter_map(|i| char::from_digit((n >> (4 * i)) & 0xF, 16)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_without_a_target_makes_the_config_unreadable() {
        let zero = r#"{"services": {"web": {"ports": [{"target": 0, "published": "8080"}]}}}"#;
        assert_eq!(ports(zero), Err("a port of `web` has no target".to_owned()));
        let one = r#"{"services": {"web": {"ports": [{"target": 1, "published": "0"}]}}}"#;
        let read = ports(one).unwrap();
        assert_eq!((read[0].target, read[0].published), (1, None), "port 0 is no published port");
    }

    /// Regression: a protocol other than `udp` read as TCP.
    #[test]
    fn a_port_of_a_protocol_compose_has_no_tcp_or_udp_for_makes_the_config_unreadable() {
        let sctp = r#"{"services": {"web": {"ports": [{"target": 80, "protocol": "sctp"}]}}}"#;
        assert_eq!(ports(sctp), Err("a port of `web` has the protocol `sctp`, neither tcp nor udp".to_owned()));
        let said_none = r#"{"services": {"web": {"ports": [{"target": 80}]}}}"#;
        assert_eq!(ports(said_none).unwrap()[0].protocol, Protocol::Tcp);
    }

    #[test]
    fn pins_ephemeral_ports_and_keeps_fixed_ones() {
        let json = r#"{"services":{"web":{"ports":[{"host_ip":"127.0.0.1","target":80,"protocol":"tcp"},{"host_ip":"127.0.0.1","target":443,"published":"8443","protocol":"tcp"}]},"db":{},"dns":{"ports":[{"target":53,"published":"","protocol":"udp"}]}}}"#;
        let mut next = 40000;
        let yaml = pinned_ports(&ports(json).unwrap(), |_| {
            next += 1;
            Ok::<_, ()>(next)
        })
        .unwrap()
        .unwrap();
        let (web, dns) = if yaml.find("\"web\"") < yaml.find("\"dns\"") {
            (40001, 40002)
        } else {
            (40002, 40001)
        };
        assert!(
            yaml.contains(&format!("  \"dns\":\n    ports: !override\n      - \"{dns}:53/udp\"\n")),
            "{yaml}"
        );
        assert!(
            yaml.contains(&format!(
                "  \"web\":\n    ports: !override\n      - \"127.0.0.1:{web}:80/tcp\"\n      - \"127.0.0.1:8443:443/tcp\"\n"
            )),
            "{yaml}"
        );
        assert!(!yaml.contains("db"));
        assert!(
            pinned_ports(&ports(r#"{"services":{"web":{"ports":[{"target":80,"published":"8080"}]}}}"#).unwrap(), |_| {
                Ok::<_, ()>(1)
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(
            pinned_ports(&ports(r#"{"services":{"web":{"ports":[{"target":80}]}}}"#).unwrap(), |_| Err("none free")),
            Err("none free")
        );
    }

    #[test]
    fn unicode_line_breaks_in_names_stay_inside_their_scalar() {
        for name in ["\u{85}", "a\u{2028}b", "\u{2029}", "na\u{ef}ve \u{1f600}"] {
            let json = serde_json::json!({"services": {name: {"ports": [{"target": 80, "protocol": "tcp"}]}}}).to_string();
            let yaml = pinned_ports(&ports(&json).unwrap(), |_| Ok::<_, ()>(40000)).unwrap().unwrap();
            let line = yaml.lines().nth(2).unwrap();
            let quoted = line.trim().trim_end_matches(':');
            assert_eq!(
                serde_json::from_str::<String>(&quoted.replace("\\U0001f600", "\u{1f600}")).unwrap(),
                name,
                "{yaml}"
            );
        }
    }

    #[test]
    fn ipv6_host_addresses_are_bracketed() {
        let json = r#"{"services":{"a":{"ports":[{"target":80,"host_ip":"::1","protocol":"udp"},{"target":443,"published":"8443","host_ip":""}]}}}"#;
        let yaml = pinned_ports(&ports(json).unwrap(), |_| Ok::<_, ()>(40000)).unwrap().unwrap();
        assert!(yaml.contains("- \"[::1]:40000:80/udp\"\n") && yaml.contains("- \"8443:443/tcp\"\n"), "{yaml}");
    }

    #[test]
    fn host_ports_take_strings_and_numbers_and_drop_zero_or_out_of_range() {
        let json = r#"{"services":{"a":{"ports":[{"published":"80","target":80}]},"b":{"ports":[{"published":8443,"target":8443}]},"c":{"ports":[{"published":0,"target":5000},{"published":"0","target":5001}]},"d":{"ports":[{"published":70000,"target":9}]},"e":{"ports":[{"published":"53","target":53,"protocol":"udp"}]},"init":{}}}"#;
        let mut found = host_ports(json).unwrap();
        found.sort();
        assert_eq!(found, [(53, Protocol::Udp), (80, Protocol::Tcp), (8443, Protocol::Tcp)]);
        assert_eq!(host_ports(r#"{"services":{}}"#), Ok(Vec::new()), "a config that publishes nothing");
        assert!(ports(r#"{"services":{"a":{"ports":[{"published":""}]}}}"#).is_err(), "a port without a target");
    }

    #[test]
    fn networks_are_named_by_their_name_else_the_project_and_key() {
        let config = r#"{"services":{},"networks":{"lab":{"name":"app-x_lab"},"back":{}}}"#;
        assert_eq!(networks(config, "app-x").unwrap(), ["app-x_lab", "app-x_back"]);
        assert_eq!(networks(r#"{"services":{}}"#, "p"), Ok(Vec::new()), "a config that defines none");
    }

    /// A config that can't be read is unknown, never empty: read as empty, it would let every port
    /// through the free-ports check and make every live network of the project stale. Why it
    /// can't be read is said (regression: serde's error was dropped, and the step's failure said
    /// only that the config couldn't be read).
    #[test]
    fn a_config_it_cant_read_has_unknown_ports_and_networks_and_says_why() {
        // serde's own wording follows `not JSON: ` (its exact text is serde_json's to change).
        for (unreadable, why) in [
            ("not json", "not JSON: "),
            ("", "not JSON: "),
            ("{}", "no `services` object"),
            (r#"{"name":"p"}"#, "no `services` object"),
            (r#"{"services":[]}"#, "no `services` object"),
        ] {
            assert!(
                host_ports(unreadable).is_err_and(|e| e.starts_with(why)),
                "{unreadable:?}: {:?}",
                host_ports(unreadable)
            );
            assert!(networks(unreadable, "p").is_err_and(|e| e.starts_with(why)), "{unreadable:?}");
        }
        let said = unreadable("compose.up", "the host ports it publishes are", "not JSON: expected value at line 1 column 1").to_string();
        assert!(
            said.contains("(not JSON: expected value at line 1 column 1), so the host ports it publishes are unknown"),
            "{said}"
        );
    }
}

#[cfg(test)]
mod proptests {
    use proptest::prelude::*;

    use super::*;

    /// A double-quoted YAML scalar read back: JSON's escapes, and YAML's `\uXXXX` and `\UXXXXXXXX`.
    fn unquote(quoted: &str) -> Option<String> {
        let mut chars = quoted.strip_prefix('"')?.strip_suffix('"')?.chars();
        let mut out = String::new();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            let escaped = chars.next()?;
            let hex = |n: usize, rest: &mut std::str::Chars<'_>| u32::from_str_radix(&rest.by_ref().take(n).collect::<String>(), 16).ok();
            out.push(match escaped {
                'u' => char::from_u32(hex(4, &mut chars)?)?,
                'U' => char::from_u32(hex(8, &mut chars)?)?,
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                'b' => '\u{8}',
                'f' => '\u{c}',
                other => other,
            });
        }
        Some(out)
    }

    proptest! {
        #[test]
        fn any_service_name_reads_back_from_the_override(name in "\\PC{1,24}", target in 1u16..) {
            let json = serde_json::json!({"services": {&name: {"ports": [{"target": target}]}}}).to_string();
            let yaml = pinned_ports(&ports(&json).unwrap(), |_| Ok::<_, ()>(40000)).unwrap().unwrap();
            prop_assert_eq!(yaml.lines().count(), 5, "{}", yaml);
            let expected = format!("      - \"40000:{target}/tcp\"\n");
            prop_assert!(yaml.ends_with(&expected));
        }

        #[test]
        fn a_quoted_scalar_is_ascii_and_reads_back_as_itself(text in "(?s).{0,40}") {
            let quoted = yaml_str(&text);
            prop_assert!(quoted.is_ascii(), "{}", quoted);
            prop_assert_eq!(unquote(&quoted), Some(text));
        }

        #[test]
        fn reading_any_text_never_panics(text in "\\PC{0,200}") {
            // Read or refused with why, never a panic.
            let (_, _, _) = (ports(&text).is_ok(), host_ports(&text).is_ok(), networks(&text, "p").is_ok());
        }
    }
}

#[cfg(test)]
mod namespace_tests {
    use super::sharing_namespace;

    #[test]
    fn a_service_in_another_ones_namespace_is_part_of_it() {
        let config = r#"{"services":{"user":{"image":"debian"},"isoloom-routes-user":{"network_mode":"service:user"},"web":{},"isoloom-routes-web":{"network_mode":"service:web"}}}"#;
        assert_eq!(sharing_namespace(config, "user").unwrap(), ["isoloom-routes-user"]);
        assert_eq!(sharing_namespace(config, "nobody").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_config_it_cant_read_is_unknown_not_empty() {
        assert!(sharing_namespace("not json", "user").is_err_and(|why| why.starts_with("not JSON: ")));
        assert_eq!(
            sharing_namespace(r#"{"name":"p"}"#, "user"),
            Err("no `services` object".to_owned()),
            "no services"
        );
        assert_eq!(sharing_namespace(r#"{"services":[]}"#, "user"), Err("no `services` object".to_owned()));
    }
}
