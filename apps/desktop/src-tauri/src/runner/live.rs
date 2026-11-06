//! The lab as it is right now: each machine's state (and health, when its image has a
//! healthcheck), and each machine's port on each of its networks, up or down, as the kernel in
//! its network namespace reports it. A suspended link shows down here; so does a port a user
//! set down in a console.
//!
//! The ports are read in one go: a throwaway container in the host's PID namespace enters each
//! running machine's network namespace (`nsenter -n`) and lists its addresses and links. A lab
//! with VMs isn't probed (a `vagrant ssh` per VM is seconds, too slow to poll): its ports follow
//! the machines' states and the suspended links.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use isoloom_spec::model::Spec;

use super::capture::{CAPTURE_IMAGE, prefix_of};
use super::links::suspended_links;
use super::{RunError, Runtime, compose, run, runtime, status};

/// One machine, now.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct MachineLive {
    /// `running`, `exited`, `restarting`, `created`, `paused`... (Docker's words; a VM's are
    /// `running` or `exited`).
    pub state: String,
    /// `healthy`, `unhealthy`, `starting`, or empty when the machine has no healthcheck.
    pub health: String,
}

/// One machine's port on one network, now.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct PortLive {
    pub machine: String,
    pub network: String,
    /// Up (carries traffic), down, or `None` when unknown: the machine isn't running, or it is a
    /// Cisco appliance (its OS holds the address, out of sight).
    pub up: Option<bool>,
    /// The interface, when it was found (`eth1`).
    pub interface: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct LabLive {
    pub machines: BTreeMap<String, MachineLive>,
    pub ports: Vec<PortLive>,
    /// Whether the ports were read from the machines (`false`: inferred, in a lab with VMs or
    /// when the probe failed).
    pub probed: bool,
}

/// The machines' states from `docker compose ps --format json` (or the VMs' in the same shape):
/// the lab's own machines only, not the sidecars, the router or Isoloom's helpers.
pub(super) fn machine_states(spec: &Spec, ps: &str) -> BTreeMap<String, MachineLive> {
    let mut out = BTreeMap::new();
    for line in ps.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k).and_then(|s| s.as_str()).unwrap_or("").to_owned();
        let service = text("Service");
        if spec.machines.contains_key(&service) {
            out.insert(
                service,
                MachineLive {
                    state: text("State"),
                    health: text("Health"),
                },
            );
        }
    }
    // A machine Compose doesn't list hasn't been created (or was removed).
    for name in spec.machines.keys() {
        out.entry(name.clone()).or_insert_with(|| MachineLive {
            state: "exited".into(),
            health: String::new(),
        });
    }
    out
}

/// The shell the probe runs: for each `(machine, pid)`, a `== machine` line, then its IPv4
/// addresses and its links (`ip -o`).
pub(super) fn probe_script(pids: &[(String, u32)]) -> String {
    pids.iter()
        .map(|(m, pid)| {
            format!(
                "echo '== {m}'; nsenter -t {pid} -n ip -o -4 addr show; nsenter -t {pid} -n ip -o link show;"
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the probe saw in one machine: interface -> its IPv4 addresses, and interface -> up.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Seen {
    pub addresses: Vec<(String, String)>,
    pub up: BTreeMap<String, bool>,
}

/// An interface's name in `ip -o` output, without its peer (`eth0@if12` is `eth0`).
fn interface_name(field: &str) -> String {
    let name = field.trim_end_matches(':');
    name.split('@').next().unwrap_or(name).to_owned()
}

/// The probe's output, by machine.
pub(super) fn parse_probe(out: &str) -> BTreeMap<String, Seen> {
    let mut all: BTreeMap<String, Seen> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in out.lines() {
        if let Some(m) = line.strip_prefix("== ") {
            current = Some(m.trim().to_owned());
            all.entry(m.trim().to_owned()).or_default();
            continue;
        }
        let Some(seen) = current.as_ref().and_then(|m| all.get_mut(m)) else {
            continue;
        };
        let fields: Vec<&str> = line.split_whitespace().collect();
        match fields.as_slice() {
            [_, iface, "inet", addr, ..] => {
                seen.addresses
                    .push((interface_name(iface), (*addr).to_owned()));
            }
            [_, iface, flags, ..] if flags.starts_with('<') => {
                // Up: set up (`UP`) and with a carrier (`LOWER_UP`).
                let flags: Vec<&str> = flags
                    .trim_matches(|c| c == '<' || c == '>')
                    .split(',')
                    .collect();
                seen.up.insert(
                    interface_name(iface),
                    flags.contains(&"UP") && flags.contains(&"LOWER_UP"),
                );
            }
            _ => {}
        }
    }
    all
}

/// Each machine's port on each of its networks, from what the probe saw (`None`: not probed).
fn ports_from(
    spec: &Spec,
    machines: &BTreeMap<String, MachineLive>,
    seen: Option<&BTreeMap<String, Seen>>,
    cut: &[super::Link],
) -> Vec<PortLive> {
    let mut out = Vec::new();
    for (name, m) in &spec.machines {
        let running = machines.get(name).is_some_and(|l| l.state == "running");
        let appliance = m.docker.as_ref().is_some_and(|d| d.appliance.is_some());
        for network in m.networks.keys() {
            let is_cut = cut
                .iter()
                .any(|l| &l.machine == name && &l.network == network);
            let prefix = spec.networks.get(network).and_then(|n| prefix_of(&n.cidr));
            let found = seen.and_then(|s| s.get(name)).and_then(|s| {
                let prefix = prefix.as_deref()?;
                let (iface, _) = s.addresses.iter().find(|(_, a)| a.starts_with(prefix))?;
                Some((iface.clone(), s.up.get(iface).copied()))
            });
            let (up, interface) = match (running, appliance, found) {
                (false, _, _) | (true, true, _) => (None, None),
                (true, false, Some((iface, up))) => (up, Some(iface)),
                // Running, but not probed (or the address is gone): what the records say.
                (true, false, None) => (Some(!is_cut), None),
            };
            out.push(PortLive {
                machine: name.clone(),
                network: super::editor_network(spec, network),
                up,
                interface,
            });
        }
    }
    out
}

/// The running machines' containers and their PIDs (on the Docker host).
fn pids(dir: &Path, machines: &BTreeMap<String, MachineLive>) -> Vec<(String, u32)> {
    machines
        .iter()
        .filter(|(_, l)| l.state == "running")
        .filter_map(|(name, _)| {
            let id = run(compose(dir, &["ps", "-q", name]), "Finding the machine").ok()?;
            let id = id.lines().next()?.trim().to_owned();
            let mut c = Command::new("docker");
            c.args(["inspect", "-f", "{{.State.Pid}}", &id]);
            let pid = run(c, "Finding its process").ok()?.trim().parse().ok()?;
            Some((name.clone(), pid))
        })
        .filter(|(_, pid)| *pid > 0)
        .collect()
}

/// Reads every running machine's ports in one throwaway container.
fn probe(pids: &[(String, u32)]) -> Result<BTreeMap<String, Seen>, RunError> {
    if pids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut c = Command::new("docker");
    c.args([
        "run",
        "--rm",
        "--privileged",
        "--pid",
        "host",
        "--network",
        "none",
        CAPTURE_IMAGE,
        "sh",
        "-c",
        &probe_script(pids),
    ]);
    Ok(parse_probe(&run(c, "Reading the machines' ports")?))
}

/// The lab now: its machines' states and their ports.
pub(crate) fn lab_live(dir: &Path, spec: &Spec) -> Result<LabLive, RunError> {
    let machines = machine_states(spec, &status(dir)?);
    let cut = suspended_links(dir);
    let seen = match runtime(dir) {
        Runtime::Docker => probe(&pids(dir, &machines)).ok(),
        _ => None,
    };
    Ok(LabLive {
        ports: ports_from(spec, &machines, seen.as_ref(), &cut),
        probed: seen.is_some(),
        machines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAB: &str = "version: 1\nname: live\nnetworks:\n  office: { cidr: 10.10.1.0/24 }\n  wire: { cidr: 10.200.0.0/29 }\nmachines:\n  a: { networks: { office: 10, wire: 2 }, app: { image: \"alpine:3.20\" } }\n  b: { networks: { office: 11 }, app: { image: \"alpine:3.20\" } }\n";

    #[test]
    fn the_states_are_the_labs_machines_only() {
        let spec = crate::runner::spec_for(LAB, crate::runner::Runtime::Docker).unwrap();
        let ps = r#"{"Service":"a","State":"running","Health":"healthy"}
{"Service":"a-routes","State":"running","Health":"healthy"}
{"Service":"isoloom-router","State":"running","Health":""}"#;
        let m = machine_states(&spec, ps);
        assert_eq!(m.len(), 2);
        assert_eq!(
            m["a"],
            MachineLive {
                state: "running".into(),
                health: "healthy".into()
            }
        );
        assert_eq!(m["b"].state, "exited", "not listed: not created");
    }

    #[test]
    fn the_probe_reads_addresses_and_links() {
        let out = "== a
1: lo    inet 127.0.0.1/8 scope host lo\\       valid_lft forever
11: eth0    inet 10.10.1.10/24 brd 10.10.1.255 scope global eth0\\       valid_lft forever
12: eth1    inet 10.200.0.2/29 brd 10.200.0.7 scope global eth1\\       valid_lft forever
1: lo: <LOOPBACK,UP,LOWER_UP> mtu 65536 qdisc noqueue state UNKNOWN
11: eth0@if9747: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc noqueue state UP
12: eth1@if9748: <BROADCAST,MULTICAST> mtu 1500 qdisc noqueue state DOWN
== b
";
        let seen = parse_probe(out);
        assert_eq!(
            seen["a"].addresses[1],
            ("eth0".into(), "10.10.1.10/24".into())
        );
        assert!(seen["a"].up["eth0"]);
        assert!(!seen["a"].up["eth1"]);
        assert_eq!(seen["b"], Seen::default());
        let script = probe_script(&[("a".into(), 42)]);
        assert!(
            script.contains("nsenter -t 42 -n ip -o -4 addr show"),
            "{script}"
        );
    }

    #[test]
    fn each_port_is_up_down_or_unknown() {
        let spec = crate::runner::spec_for(LAB, crate::runner::Runtime::Docker).unwrap();
        let live = |state: &str| MachineLive {
            state: state.into(),
            health: String::new(),
        };
        let machines: BTreeMap<String, MachineLive> = [
            ("a".to_owned(), live("running")),
            ("b".to_owned(), live("exited")),
        ]
        .into();
        let mut seen = BTreeMap::new();
        seen.insert(
            "a".to_owned(),
            Seen {
                addresses: vec![
                    ("eth0".into(), "10.10.1.10/24".into()),
                    ("eth1".into(), "10.200.0.2/29".into()),
                ],
                up: [("eth0".to_owned(), true), ("eth1".to_owned(), false)].into(),
            },
        );
        let ports = ports_from(&spec, &machines, Some(&seen), &[]);
        let port = |m: &str, n: &str| {
            ports
                .iter()
                .find(|p| p.machine == m && p.network == n)
                .unwrap()
                .clone()
        };
        assert_eq!(port("a", "office").up, Some(true));
        assert_eq!(port("a", "office").interface.as_deref(), Some("eth0"));
        assert_eq!(port("a", "wire").up, Some(false));
        assert_eq!(port("b", "office").up, None, "stopped: unknown");
        // Not probed: the records say.
        let cut = [super::super::Link {
            network: "wire".into(),
            machine: "a".into(),
        }];
        let from_records = ports_from(&spec, &machines, None, &cut);
        assert_eq!(
            from_records
                .iter()
                .find(|p| p.network == "wire")
                .unwrap()
                .up,
            Some(false)
        );
        assert_eq!(
            from_records
                .iter()
                .find(|p| p.machine == "a" && p.network == "office")
                .unwrap()
                .up,
            Some(true)
        );
    }

    /// Needs Docker (run with `--ignored`): the probe sees a suspended link down, and up again.
    #[test]
    #[ignore = "needs Docker; run with --ignored"]
    fn the_probe_sees_a_suspended_link() {
        use crate::runner::{down, link_action, prepare, up};
        let dir = std::env::temp_dir().join(format!("isothrum-live-{}", std::process::id()));
        let yaml = "version: 1\nname: live\nnetworks:\n  a: { cidr: 10.64.1.0/24 }\nmachines:\n  pc: { networks: { a: 10 }, app: { image: \"alpine:3.20\" }, command: [sleep, infinity] }\n  srv: { networks: { a: 11 }, app: { image: \"alpine:3.20\" }, command: [sleep, infinity] }\n";
        let spec = prepare(&dir, yaml, &[]).unwrap();
        up(&dir).unwrap();
        let port = |l: &LabLive, m: &str| l.ports.iter().find(|p| p.machine == m).unwrap().clone();
        let live = lab_live(&dir, &spec).unwrap();
        assert!(live.probed);
        assert_eq!(live.machines["pc"].state, "running");
        assert_eq!(port(&live, "pc").up, Some(true), "{live:?}");
        link_action(&dir, &spec, "a", Some(&["pc".to_owned()]), "suspend").unwrap();
        let suspended = lab_live(&dir, &spec).unwrap();
        assert_eq!(port(&suspended, "pc").up, Some(false), "{suspended:?}");
        assert_eq!(port(&suspended, "srv").up, Some(true));
        link_action(&dir, &spec, "a", None, "resume").unwrap();
        assert_eq!(port(&lab_live(&dir, &spec).unwrap(), "pc").up, Some(true));
        down(&dir).unwrap();
        drop(std::fs::remove_dir_all(&dir));
    }
}
