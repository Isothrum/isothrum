//! Reading `docker compose ps --all --format json`, and matching what it lists to the
//! manifest's machines by their Compose service. Nothing is guessed from names or images.

use std::collections::BTreeMap;

use isoloom_manifest::Manifest;
use serde::Deserialize;

use super::EngineAt;
use crate::error::Error;
use crate::port::{GuestPort, HostPort};
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
        self.labels.split(',').any(|l| l.trim() == super::ONE_OFF_LABEL)
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
    fn published(&self) -> Vec<EnginePort> {
        let mut out: Vec<EnginePort> = Vec::new();
        for p in self.publishers.iter().filter(|p| p.published_port > 0 && p.target_port > 0) {
            let Some(protocol) = super::protocol(&p.protocol) else {
                tracing::warn!(
                    service = self.service,
                    protocol = p.protocol,
                    "a published port's protocol isn't TCP or UDP: it is left out"
                );
                continue;
            };
            let port = EnginePort {
                target: GuestPort::new(p.target_port),
                protocol,
                on_engine: p.published_port,
            };
            if !out.contains(&port) {
                out.push(port);
            }
        }
        out
    }
}

/// `ps --format json`'s containers: a JSON array (older Compose) or one object per line (2.21 on).
/// A blank line is no container. `None` when the output holds a line that doesn't parse, traced
/// by its number and why (never the line: a container's line may hold its command): a container
/// left out would read as a machine absent, and the operations would act on that (a start made
/// again, a stop skipped).
pub(super) fn parse_ps(out: &str) -> Option<Vec<PsEntry>> {
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return Some(Vec::new());
    }
    if trimmed.starts_with('[') {
        return serde_json::from_str(trimmed)
            .inspect_err(|e| tracing::warn!(error = %e, "`docker compose ps`'s list couldn't be read"))
            .ok();
    }
    let read = |(n, line): (usize, &str)| {
        serde_json::from_str(line)
            .inspect_err(|e| tracing::warn!(line = n + 1, error = %e, "a line of `docker compose ps` couldn't be read: the listing is unreadable"))
            .ok()
    };
    let lines = trimmed.lines().enumerate().filter(|(_, l)| !l.trim().is_empty());
    lines.map(read).collect()
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

/// A container port as the engine publishes it: on the engine's host, which is this computer
/// or the run's VM ([`EngineAt`]), so not yet a host port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnginePort {
    /// The container's port.
    target: GuestPort,
    protocol: isoloom_manifest::Protocol,
    /// The port on the engine's host.
    on_engine: u16,
}

/// Every machine of `manifest`, as `entries` show it (absent when none matches its service), and
/// the host port each published machine port is reached at, the engine running `at`.
pub(super) fn machines(manifest: &Manifest, entries: &[PsEntry], at: &EngineAt<'_>) -> (Vec<MachineStatus>, BTreeMap<PortKey, HostPort>) {
    let mut published = BTreeMap::new();
    let mut out = Vec::new();
    for m in &manifest.machines {
        let service = crate::driver::native_name(m);
        let entry = entries.iter().find(|e| e.service == service && !e.one_off());
        let mut ports = Vec::new();
        for p in entry.map(PsEntry::published).unwrap_or_default() {
            let declared = super::machine_port(m, p.target, p.protocol);
            let Some(host_port) = at.reached_at(m, declared, p.on_engine) else { continue };
            // Each by its name in the manifest (`${published.<machine>.<name>}`).
            if let Some(declared) = declared {
                published.insert(PortKey::new(m.name.as_str(), declared.name.as_str()), host_port);
            }
            ports.push(PublishedPort {
                port: p.target,
                protocol: p.protocol,
                host_port,
            });
        }
        out.push(MachineStatus {
            name: m.name.to_string(),
            role: m.role,
            state: entry.map_or(MachineState::Absent, PsEntry::machine_state),
            addresses: crate::driver::static_addresses(m),
            published: ports,
            console: None,
        });
    }
    (out, published)
}

#[cfg(test)]
mod tests {
    use isoloom_manifest::Protocol;

    use super::*;
    use crate::testing::docker_manifest;

    #[tokio::test]
    async fn a_line_that_doesnt_parse_makes_the_listing_unreadable_traced_by_its_number() {
        let lines = "{\"Service\":\"web\",\"State\":\"running\"}\n\n{\"Service\":\"db\",\"Command\":\"run --token s3cret\" cut\n";
        let (entries, traced) = crate::testing::traced(async { parse_ps(lines) }).await;
        assert!(entries.is_none(), "a container left out would read as a machine absent: {entries:?}");
        assert!(
            traced.contains("WARN") && traced.contains("line=3") && traced.contains("couldn't be read"),
            "{traced}"
        );
        assert!(!traced.contains("s3cret"), "the line itself isn't traced: {traced}");
        let (_, quiet) = crate::testing::traced(async { parse_ps("{\"Service\":\"web\"}\n\n") }).await;
        assert!(quiet.is_empty(), "a blank line is no container: {quiet}");
    }

    /// Regression: the list form's parse error was dropped, unlike a line's.
    #[tokio::test]
    async fn a_list_that_doesnt_parse_is_unreadable_and_traced() {
        let (entries, traced) = crate::testing::traced(async { parse_ps("[{\"Service\":\"web\"},") }).await;
        assert!(entries.is_none());
        assert!(
            traced.contains("WARN") && traced.contains("list couldn't be read") && traced.contains("EOF"),
            "{traced}"
        );
    }

    #[test]
    fn parses_both_compose_output_formats() {
        let lines = "{\"Service\":\"web\",\"State\":\"running\"}\n\n{\"Service\":\"db\",\"State\":\"exited\",\"ExitCode\":3}\n";
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
            {"Service":"isoloom-init-web-1","State":"exited","ExitCode":0},
            {"Service":"isoloom-check","State":"running","Labels":"com.docker.compose.oneoff=True"},
            {"Service":"stranger","State":"running"}]"#;
        let (machines, published) = machines(&docker_manifest(), &read(ps), &EngineAt::ThisComputer);
        let state = |n: &str| machines.iter().find(|m| m.name == n).unwrap().state.clone();
        assert_eq!(state("web"), MachineState::Running);
        assert_eq!(state("db"), MachineState::Unhealthy);
        assert_eq!(state("isoloom-init-web-1"), MachineState::Exited(0));
        assert_eq!(state("isoloom-check"), MachineState::Absent, "a one-off isn't the machine");
        assert!(machines.iter().all(|m| m.name != "stranger"));
        assert_eq!(published, BTreeMap::from([(PortKey::new("web", "portal"), HostPort::new(41873))]));
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
        let udp = EnginePort {
            target: GuestPort::new(80),
            protocol: Protocol::Udp,
            on_engine: 41874,
        };
        assert_eq!(read(ps)[0].published(), [udp]);
    }

    /// Regression: a protocol other than `udp` read as TCP.
    #[tokio::test]
    async fn a_port_published_over_another_protocol_is_left_out_and_traced() {
        let ps = r#"[{"Service":"web","State":"running","Publishers":[
            {"PublishedPort":41873,"TargetPort":80,"Protocol":"sctp"},
            {"PublishedPort":41874,"TargetPort":81}]}]"#;
        let (published, traced) = crate::testing::traced(async { read(ps)[0].published() }).await;
        assert_eq!(
            published.iter().map(|p| (p.target, p.protocol)).collect::<Vec<_>>(),
            [(GuestPort::new(81), Protocol::Tcp)]
        );
        assert!(traced.contains("WARN") && traced.contains("sctp"), "{traced}");
    }

    #[test]
    fn a_published_port_is_named_by_its_port_and_its_transport_both() {
        let ps = r#"[{"Service":"web","State":"running","Publishers":[
            {"PublishedPort":41873,"TargetPort":80,"Protocol":"tcp"},
            {"PublishedPort":41874,"TargetPort":80,"Protocol":"udp"},
            {"PublishedPort":41875,"TargetPort":8443,"Protocol":"tcp"}]}]"#;
        let (machines, published) = machines(&docker_manifest(), &read(ps), &EngineAt::ThisComputer);
        assert_eq!(published, BTreeMap::from([(PortKey::new("web", "portal"), HostPort::new(41873))]));
        let web = machines.iter().find(|m| m.name == "web").unwrap();
        assert_eq!(web.published.len(), 3, "every published port is shown, named or not");
    }

    /// Regression (the docker-vm read): a port Compose publishes inside the run's VM is the VM's,
    /// not this computer's. It is reported only at the host port the VM's driver reaches it at,
    /// and one the driver doesn't reach (undeclared, so nothing forwards it) is left out instead
    /// of being reported at the VM's number.
    #[test]
    fn a_port_published_in_a_vm_is_reported_at_the_host_port_its_driver_gives() {
        let ps = r#"[{"Service":"web","State":"running","Publishers":[
            {"PublishedPort":80,"TargetPort":80,"Protocol":"tcp"},
            {"PublishedPort":8443,"TargetPort":8443,"Protocol":"tcp"}]}]"#;
        // The VM forwards its declared port 80 from 50123; nothing forwards the rest.
        let reach = |_: &isoloom_manifest::Machine, p: Option<&isoloom_manifest::Port>, on_vm: GuestPort| {
            p.map(|_| {
                assert_eq!(on_vm, GuestPort::new(80));
                HostPort::new(50123)
            })
        };
        let (machines, published) = machines(&docker_manifest(), &read(ps), &EngineAt::Vm(&reach));
        assert_eq!(published, BTreeMap::from([(PortKey::new("web", "portal"), HostPort::new(50123))]));
        let web = machines.iter().find(|m| m.name == "web").unwrap();
        let shown: Vec<_> = web.published.iter().map(|p| (p.port, p.host_port)).collect();
        assert_eq!(shown, [(GuestPort::new(80), HostPort::new(50123))], "never the VM's 8443 as a host port");
    }

    #[test]
    fn a_machine_is_found_by_its_compose_service_not_its_name() {
        let manifest = isoloom_manifest::parse(&crate::testing::WEB_PORTAL.replace(r#""service": "web""#, r#""service": "portal""#)).unwrap();
        let ps = r#"[{"Service":"portal","State":"running"},{"Service":"web","State":"exited","ExitCode":1}]"#;
        let (machines, _) = machines(&manifest, &read(ps), &EngineAt::ThisComputer);
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
                let _ = machines(&crate::testing::docker_manifest(), &entries, &EngineAt::ThisComputer);
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
