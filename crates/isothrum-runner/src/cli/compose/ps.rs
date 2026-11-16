//! Reading `docker compose ps --all --format json`: the project's containers, each with its
//! service, its state and health, and the host ports it publishes. Nothing is guessed: a listing
//! that can't be read whole is unreadable, and a port whose protocol Compose doesn't say is
//! published over an unknown protocol.

use serde::Deserialize;

use crate::project::platform::Protocol;
use crate::status::MachineState;

/// The label, with its value, Compose puts on a `docker compose run` container (a one-off).
const ONE_OFF_LABEL: &str = "com.docker.compose.oneoff=True";

/// One container of the project, as `ps` prints it.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Container {
    #[serde(rename = "Service")]
    service: String,
    #[serde(rename = "State", default)]
    state: String,
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

/// A port a container publishes on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContainerPort {
    /// The container's port.
    pub(crate) guest: crate::port::GuestPort,
    /// The host port.
    pub(crate) host: crate::port::HostPort,
    /// `tcp` or `udp`; `None` when Compose says no protocol, or one that is neither.
    pub(crate) protocol: Option<Protocol>,
}

fn null_as_empty<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Vec<T>, D::Error> {
    Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
}

impl Container {
    /// Its service.
    pub(crate) fn service(&self) -> &str {
        &self.service
    }

    /// Whether it is paused (`compose pause`): what a resume unpauses.
    pub(crate) fn is_paused(&self) -> bool {
        self.state == "paused"
    }

    /// A `docker compose run` container: not one of the project's machines.
    pub(crate) fn one_off(&self) -> bool {
        self.labels.split(',').any(|l| l.trim() == ONE_OFF_LABEL)
    }

    /// Its state, its health check's verdict included.
    pub(crate) fn machine_state(&self) -> MachineState {
        match (self.state.as_str(), self.health.as_str()) {
            ("running", "unhealthy") => MachineState::Unhealthy,
            ("running", _) => MachineState::Running,
            ("exited", _) => MachineState::Exited(self.exit_code),
            ("created" | "paused", _) => MachineState::Stopped,
            (other, _) => MachineState::Unknown(other.to_owned()),
        }
    }

    /// Its published ports, once each (Docker lists one per address family).
    pub(crate) fn published(&self) -> Vec<ContainerPort> {
        let mut out: Vec<ContainerPort> = Vec::new();
        for p in self.publishers.iter().filter(|p| p.published_port > 0 && p.target_port > 0) {
            let port = ContainerPort {
                guest: crate::port::GuestPort::new(p.target_port),
                host: crate::port::HostPort::new(p.published_port),
                protocol: Protocol::parse(&p.protocol),
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
/// left out would read as a machine absent, and the operations would act on that.
pub(crate) fn parse(out: &str) -> Option<Vec<Container>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_line_that_doesnt_parse_makes_the_listing_unreadable_traced_by_its_number() {
        let lines = "{\"Service\":\"web\",\"State\":\"running\"}\n\n{\"Service\":\"db\",\"Command\":\"run --token s3cret\" cut\n";
        let (entries, traced) = crate::testing::traced(async { parse(lines) }).await;
        assert!(entries.is_none(), "a container left out would read as a machine absent: {entries:?}");
        assert!(
            traced.contains("WARN") && traced.contains("line=3") && traced.contains("couldn't be read"),
            "{traced}"
        );
        assert!(!traced.contains("s3cret"), "the line itself isn't traced: {traced}");
    }

    #[test]
    fn both_listing_formats_are_read_one_object_per_line_or_an_array() {
        let lines = "{\"Service\":\"web\",\"State\":\"running\"}\n\n{\"Service\":\"db\",\"State\":\"exited\",\"ExitCode\":3}\n";
        assert_eq!(read(lines).len(), 2);
        assert_eq!(read("[{\"Service\":\"web\",\"State\":\"running\"}]")[0].service, "web");
        assert_eq!(read(lines)[1].machine_state(), MachineState::Exited(3));
    }

    #[test]
    fn no_output_is_no_container_but_output_with_none_readable_is_unreadable() {
        assert!(read("").is_empty() && read(" \n").is_empty() && read("[]").is_empty());
        for out in ["[not valid", "not json", "{bogus}\n{\"State\":\"running\"}", "[{\"State\":\"running\"}]"] {
            assert!(parse(out).is_none(), "{out}");
        }
    }

    fn read(out: &str) -> Vec<Container> {
        parse(out).unwrap()
    }

    #[test]
    fn a_compose_run_container_is_a_one_off_and_a_services_isnt() {
        let out = "{\"Service\":\"check\",\"State\":\"running\",\"Labels\":\"com.docker.compose.project=p,com.docker.compose.oneoff=True\"}\n{\"Service\":\"web\",\"State\":\"running\",\"Labels\":\"com.docker.compose.oneoff=False\"}\n";
        let flags: Vec<bool> = read(out).iter().map(Container::one_off).collect();
        assert_eq!(flags, [true, false]);
    }

    #[test]
    fn a_port_published_to_or_from_zero_is_not_published_and_one_listed_twice_is_kept_once() {
        let ps = r#"[{"Service":"web","State":"running","Publishers":[
            {"PublishedPort":0,"TargetPort":80,"Protocol":"tcp"},
            {"PublishedPort":41873,"TargetPort":0,"Protocol":"tcp"},
            {"PublishedPort":41874,"TargetPort":80,"Protocol":"udp"},
            {"PublishedPort":41874,"TargetPort":80,"Protocol":"udp"}]}]"#;
        let udp = ContainerPort {
            guest: crate::port::GuestPort::new(80),
            host: crate::port::HostPort::new(41874),
            protocol: Some(Protocol::Udp),
        };
        assert_eq!(read(ps)[0].published(), [udp]);
    }

    /// Regression: an empty protocol, or one that isn't TCP or UDP, read as TCP.
    #[test]
    fn a_port_whose_protocol_isnt_said_is_published_over_an_unknown_one() {
        let ps = r#"[{"Service":"web","State":"running","Publishers":[
            {"PublishedPort":41873,"TargetPort":80,"Protocol":"sctp"},
            {"PublishedPort":41874,"TargetPort":81}]}]"#;
        let published: Vec<_> = read(ps)[0].published().iter().map(|p| (p.guest.get(), p.protocol)).collect();
        assert_eq!(published, [(80, None), (81, None)]);
    }
}
