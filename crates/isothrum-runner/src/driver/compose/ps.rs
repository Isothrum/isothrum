//! Reading `docker compose ps --all --format json`, and matching what it lists to the
//! manifest's machines by their Compose service. Nothing is guessed from names or images.

use std::collections::BTreeMap;

use isoloom_manifest::{Manifest, Native, Protocol};
use serde::Deserialize;

use crate::error::Error;
use crate::status::{MachineState, MachineStatus, PublishedPort};
use crate::vars::PortKey;

/// One container, as `ps` prints it.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct PsEntry {
    #[serde(rename = "Service")]
    pub(super) service: String,
    #[serde(rename = "State", default)]
    pub(super) state: String,
    /// `healthy`, `unhealthy`, `starting`, or empty (no health check).
    #[serde(rename = "Health", default)]
    health: String,
    #[serde(rename = "ExitCode", default)]
    exit_code: i32,
    /// `null` from some Compose versions for a container without ports: as none.
    #[serde(rename = "Publishers", default, deserialize_with = "null_as_empty")]
    publishers: Vec<Publisher>,
    /// Compose's labels, comma-joined.
    #[serde(rename = "Labels", default)]
    labels: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Publisher {
    #[serde(rename = "PublishedPort", default)]
    published_port: u16,
    #[serde(rename = "TargetPort", default)]
    target_port: u16,
    #[serde(rename = "Protocol", default)]
    protocol: String,
}

fn null_as_empty<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Vec<T>, D::Error> {
    Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
}

impl PsEntry {
    /// A `docker compose run` container (a check runner): not one of the run's machines.
    pub(super) fn one_off(&self) -> bool {
        self.labels.split(',').any(|l| l.trim() == "com.docker.compose.oneoff=True")
    }

    fn machine_state(&self) -> MachineState {
        match (self.state.as_str(), self.health.as_str()) {
            ("running", "unhealthy") => MachineState::Unhealthy,
            ("running", _) => MachineState::Running,
            ("exited", _) => MachineState::Exited(self.exit_code),
            ("created" | "paused", _) => MachineState::Stopped,
            (other, _) => MachineState::Unknown(other.to_owned()),
        }
    }

    /// Its published ports, once each (Docker lists one per address family).
    fn published(&self) -> Vec<PublishedPort> {
        let mut out: Vec<PublishedPort> = Vec::new();
        for p in self.publishers.iter().filter(|p| p.published_port > 0 && p.target_port > 0) {
            let protocol = if p.protocol == "udp" { Protocol::Udp } else { Protocol::Tcp };
            let port = PublishedPort {
                port: p.target_port,
                protocol,
                host_port: p.published_port,
            };
            if !out.contains(&port) {
                out.push(port);
            }
        }
        out
    }
}

/// `ps --format json`'s containers: a JSON array (older Compose) or one object per line (2.21 on).
/// A line that doesn't parse is skipped; `None` when there is output but no container in it can be
/// read: taken as no container, it would make every machine of a run that is there read absent.
pub(super) fn parse_ps(out: &str) -> Option<Vec<PsEntry>> {
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return Some(Vec::new());
    }
    if trimmed.starts_with('[') {
        return serde_json::from_str(trimmed).ok();
    }
    let entries: Vec<PsEntry> = trimmed.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    (!entries.is_empty()).then_some(entries)
}

/// The words for `ps` output [`parse_ps`] can't read.
pub(crate) const UNREADABLE: &str = "`docker compose ps --format json` printed no container this runner can read";

/// The failure of step `step`, whose `ps` output [`parse_ps`] can't read.
pub(super) fn unreadable(step: &str) -> Error {
    Error::ToolFailed {
        step: step.to_owned(),
        reason: UNREADABLE.to_owned(),
    }
}

/// Every machine of `manifest`, as `entries` show it (absent when none matches its service), and
/// the host port each published machine port got.
pub(super) fn machines(manifest: &Manifest, entries: &[PsEntry]) -> (Vec<MachineStatus>, BTreeMap<PortKey, u16>) {
    let mut published = BTreeMap::new();
    let mut out = Vec::new();
    for m in &manifest.machines {
        let service = match &m.native {
            Some(Native::Compose { service }) => service.as_str(),
            _ => m.name.as_str(),
        };
        let entry = entries.iter().find(|e| e.service == service && !e.one_off());
        let ports = entry.map(PsEntry::published).unwrap_or_default();
        // Each by its name in the manifest (`${published.<machine>.<name>}`).
        for p in &ports {
            if let Some(port) = m.ports.iter().find(|q| q.port.get() == p.port && q.kind.transport() == p.protocol) {
                published.insert((m.name.to_string(), port.name.to_string()), p.host_port);
            }
        }
        out.push(MachineStatus {
            name: m.name.to_string(),
            role: m.role,
            state: entry.map_or(MachineState::Absent, PsEntry::machine_state),
            addresses: crate::driver::static_addresses(m),
            published: ports,
        });
    }
    (out, published)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::docker_manifest;

    #[test]
    fn parses_both_compose_output_formats() {
        let lines = "{\"Service\":\"web\",\"State\":\"running\"}\n\n{bogus}\n{\"Service\":\"db\",\"State\":\"exited\",\"ExitCode\":3}\n";
        assert_eq!(read(lines).len(), 2);
        assert_eq!(read("[{\"Service\":\"web\",\"State\":\"running\"}]")[0].service, "web");
        assert_eq!(read(lines)[1].machine_state(), MachineState::Exited(3));
    }

    #[test]
    fn no_output_is_no_container_but_output_with_none_readable_is_unreadable() {
        assert!(read("").is_empty() && read(" \n").is_empty() && read("[]").is_empty());
        for out in ["[not valid", "not json", "{bogus}\n{\"State\":\"running\"}", "[{\"State\":\"running\"}]"] {
            assert!(parse_ps(out).is_none(), "{out}");
        }
    }

    /// The containers `out` lists, which must be readable.
    fn read(out: &str) -> Vec<PsEntry> {
        parse_ps(out).unwrap()
    }

    #[test]
    fn compose_run_containers_are_one_off() {
        let out = "{\"Service\":\"isoloom-check\",\"State\":\"running\",\"Labels\":\"com.docker.compose.project=p,com.docker.compose.oneoff=True\"}\n{\"Service\":\"web\",\"State\":\"running\",\"Labels\":\"com.docker.compose.oneoff=False\"}\n";
        let flags: Vec<bool> = read(out).iter().map(PsEntry::one_off).collect();
        assert_eq!(flags, [true, false]);
    }

    #[test]
    fn machines_come_from_the_manifest_and_ports_from_ps() {
        let ps = r#"[{"Service":"web","State":"running","Health":"healthy","Publishers":[{"URL":"127.0.0.1","PublishedPort":41873,"TargetPort":80,"Protocol":"tcp"},{"URL":"::1","PublishedPort":41873,"TargetPort":80,"Protocol":"tcp"}]},
            {"Service":"db","State":"running","Health":"unhealthy","Publishers":null},
            {"Service":"web-init-1","State":"exited","ExitCode":0},
            {"Service":"isoloom-check","State":"running","Labels":"com.docker.compose.oneoff=True"},
            {"Service":"stranger","State":"running"}]"#;
        let (machines, published) = machines(&docker_manifest(), &read(ps));
        let state = |n: &str| machines.iter().find(|m| m.name == n).unwrap().state.clone();
        assert_eq!(state("web"), MachineState::Running);
        assert_eq!(state("db"), MachineState::Unhealthy);
        assert_eq!(state("web-init-1"), MachineState::Exited(0));
        assert_eq!(state("isoloom-check"), MachineState::Absent, "a one-off isn't the machine");
        assert!(machines.iter().all(|m| m.name != "stranger"));
        assert_eq!(published, BTreeMap::from([(("web".to_owned(), "portal".to_owned()), 41873)]));
        let web = machines.iter().find(|m| m.name == "web").unwrap();
        assert_eq!((web.published.len(), web.addresses["lab"].as_str()), (1, "10.30.0.10"));
    }

    #[test]
    fn a_port_published_to_or_from_zero_is_not_published_and_one_listed_twice_is_kept_once() {
        let ps = r#"[{"Service":"web","State":"running","Publishers":[
            {"PublishedPort":0,"TargetPort":80,"Protocol":"tcp"},
            {"PublishedPort":41873,"TargetPort":0,"Protocol":"tcp"},
            {"PublishedPort":41874,"TargetPort":80,"Protocol":"udp"},
            {"PublishedPort":41874,"TargetPort":80,"Protocol":"udp"}]}]"#;
        let udp = PublishedPort {
            port: 80,
            protocol: Protocol::Udp,
            host_port: 41874,
        };
        assert_eq!(read(ps)[0].published(), [udp]);
    }

    #[test]
    fn a_published_port_is_named_by_its_port_and_its_transport_both() {
        let ps = r#"[{"Service":"web","State":"running","Publishers":[
            {"PublishedPort":41873,"TargetPort":80,"Protocol":"tcp"},
            {"PublishedPort":41874,"TargetPort":80,"Protocol":"udp"},
            {"PublishedPort":41875,"TargetPort":8443,"Protocol":"tcp"}]}]"#;
        let (machines, published) = machines(&docker_manifest(), &read(ps));
        assert_eq!(published, BTreeMap::from([(("web".to_owned(), "portal".to_owned()), 41873)]));
        let web = machines.iter().find(|m| m.name == "web").unwrap();
        assert_eq!(web.published.len(), 3, "every published port is shown, named or not");
    }

    #[test]
    fn a_machine_is_found_by_its_compose_service_not_its_name() {
        let manifest = isoloom_manifest::parse(&crate::testing::WEB_PORTAL.replace(r#""service": "web""#, r#""service": "portal""#)).unwrap();
        let ps = r#"[{"Service":"portal","State":"running"},{"Service":"web","State":"exited","ExitCode":1}]"#;
        let (machines, _) = machines(&manifest, &read(ps));
        let web = machines.iter().find(|m| m.name == "web").unwrap();
        assert_eq!(web.state, MachineState::Running);
    }
}

#[cfg(test)]
mod proptests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn any_ps_output_is_read_without_panicking(text in "\\PC{0,300}") {
            if let Some(entries) = parse_ps(&text) {
                let _ = machines(&crate::testing::docker_manifest(), &entries);
            }
        }

        #[test]
        fn both_formats_read_the_same_entries(services in proptest::collection::vec("[a-z][a-z0-9-]{0,12}", 0..6)) {
            let objects: Vec<String> = services.iter().map(|s| format!(r#"{{"Service":"{s}","State":"running"}}"#)).collect();
            let lines = parse_ps(&objects.join("\n")).unwrap();
            let array = parse_ps(&format!("[{}]", objects.join(","))).unwrap();
            let names = |e: &[PsEntry]| e.iter().map(|p| p.service.clone()).collect::<Vec<_>>();
            prop_assert_eq!(names(&lines), services.clone());
            prop_assert_eq!(names(&array), services);
        }
    }
}
