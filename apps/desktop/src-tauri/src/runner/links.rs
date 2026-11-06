//! Suspending a link, as GNS3 does: a machine's port on a LAN or a cable is set down in its
//! network namespace (`ip link set down`), as pulling the cable would; resuming sets it up again
//! and gives the machine its routes back (the kernel dropped those through the port). The lab's
//! folder records the suspended links (`links.json`), so a machine started again, or the whole
//! lab, finds them cut as before. Stopping the lab forgets them.

use std::path::Path;
use std::process::Command;

use isoloom_spec::model::Spec;

use super::capture::{CAPTURE_IMAGE, interface_script, prefix_of};
use super::machines::{running_container, with_sidecar};
use super::{RunError, Runtime, compose, run, runtime};

/// The record of the suspended links, in the lab's folder.
const LINKS: &str = "links.json";

/// One machine's port on one network, cut.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Link {
    pub network: String,
    pub machine: String,
}

/// The lab's suspended links.
pub(crate) fn suspended_links(dir: &Path) -> Vec<Link> {
    std::fs::read_to_string(dir.join(LINKS))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save(dir: &Path, links: &[Link]) -> Result<(), RunError> {
    if links.is_empty() {
        drop(std::fs::remove_file(dir.join(LINKS)));
        return Ok(());
    }
    let text = serde_json::to_string_pretty(links).map_err(|e| RunError::Io(e.to_string()))?;
    std::fs::write(dir.join(LINKS), text).map_err(|e| RunError::Io(e.to_string()))
}

/// The links with their networks as the editor names them (a VLAN's is `vlan10`).
pub(crate) fn editor_links(spec: &Spec, links: Vec<Link>) -> Vec<Link> {
    links
        .into_iter()
        .map(|l| Link {
            network: super::editor_network(spec, &l.network),
            machine: l.machine,
        })
        .collect()
}

/// Forgets the suspended links (the lab is stopped: its machines are gone).
pub(super) fn clear_links(dir: &Path) {
    drop(std::fs::remove_file(dir.join(LINKS)));
}

/// `links` with `cut` added (suspended) or removed (resumed), each once.
fn with(links: &[Link], cut: &[Link], suspend: bool) -> Vec<Link> {
    let mut out: Vec<Link> = links.iter().filter(|l| !cut.contains(l)).cloned().collect();
    if suspend {
        out.extend(cut.iter().cloned());
    }
    out
}

/// The shell run in the machine's namespace: find its port on the network (by the block's
/// prefix, digits and dots only) and set it down or up.
pub(super) fn link_script(prefix: &str, up: bool) -> String {
    let state = if up { "up" } else { "down" };
    format!("{} ip link set \"$IF\" {state}", interface_script(prefix))
}

/// The ports a link action touches: the block's prefix, and the machines whose port on
/// `network` it is: those named (each must be on the network), else every machine on it.
fn ports(
    spec: &Spec,
    network: &str,
    machines: Option<&[String]>,
) -> Result<(String, Vec<String>), RunError> {
    let net = spec
        .networks
        .get(network)
        .ok_or_else(|| RunError::Spec(format!("the lab has no network `{network}`")))?;
    let prefix = prefix_of(&net.cidr)
        .filter(|p| p.chars().all(|c| c.is_ascii_digit() || c == '.'))
        .ok_or_else(|| RunError::Spec(format!("`{}` isn't an IPv4 block", net.cidr)))?;
    let on_network = |m: &str| {
        spec.machines
            .get(m)
            .is_some_and(|m| m.networks.contains_key(network))
    };
    let names: Vec<String> = match machines {
        Some(named) => {
            if let Some(m) = named.iter().find(|m| !on_network(m)) {
                return Err(RunError::Spec(format!("`{m}` isn't on `{network}`")));
            }
            named.to_vec()
        }
        None => spec
            .machines
            .iter()
            .filter(|(_, m)| m.networks.contains_key(network))
            .map(|(n, _)| n.clone())
            .collect(),
    };
    if names.is_empty() {
        return Err(RunError::Spec(format!("no machine is on `{network}`")));
    }
    Ok((prefix, names))
}

/// Suspends or resumes the links of `machines` (or of every machine) on `network`; returns the
/// lab's suspended links afterwards. A machine that isn't running is only recorded: its port
/// is cut when it starts. A Cisco appliance's port can't be (its OS owns the address), so a
/// cable to one is cut at its other end.
pub(crate) fn link_action(
    dir: &Path,
    spec: &Spec,
    network: &str,
    machines: Option<&[String]>,
    action: &str,
) -> Result<Vec<Link>, RunError> {
    let suspend = match action {
        "suspend" => true,
        "resume" => false,
        other => {
            return Err(RunError::Spec(format!(
                "unknown action `{other}`: suspend or resume"
            )));
        }
    };
    // Recorded by the spec's name (what the machines are on); reported by the editor's.
    let network = super::spec_network(spec, network)?;
    let network = network.as_str();
    let (prefix, names) = ports(spec, network, machines)?;
    let mut failed = Vec::new();
    let mut done = Vec::new();
    for machine in &names {
        match set_link(dir, spec, machine, &prefix, !suspend) {
            Ok(()) => done.push(Link {
                network: network.into(),
                machine: machine.clone(),
            }),
            Err(e) => failed.push(format!("{machine}: {e}")),
        }
    }
    let links = with(&suspended_links(dir), &done, suspend);
    save(dir, &links)?;
    let links = editor_links(spec, links);
    if !failed.is_empty() {
        return Err(RunError::Command {
            what: if suspend {
                "Suspending the link".into()
            } else {
                "Resuming the link".into()
            },
            output: failed.join("\n"),
        });
    }
    Ok(links)
}

/// Sets `machine`'s port on the block down or up, and after up, its routes back.
fn set_link(
    dir: &Path,
    spec: &Spec,
    machine: &str,
    prefix: &str,
    up: bool,
) -> Result<(), RunError> {
    let Some(m) = spec.machines.get(machine) else {
        return Ok(());
    };
    if m.docker.as_ref().is_some_and(|d| d.appliance.is_some()) {
        return Ok(());
    }
    let script = link_script(prefix, up);
    match runtime(dir) {
        Runtime::Docker => {
            let Some(id) = running_container(dir, machine)? else {
                return Ok(());
            };
            run(in_container(&id, &script), "Setting the port")?;
            if up && with_sidecar(dir, machine)?.len() > 1 {
                run(
                    compose(dir, &["restart", &format!("{machine}-routes")]),
                    "Setting the routes again",
                )?;
            }
        }
        rt => {
            let in_host = crate::vms::in_host(spec, rt, machine);
            run(
                crate::vms::in_namespace(dir, rt, machine, in_host, &script),
                "Setting the port",
            )?;
            if up {
                crate::vms::restore_routes(dir, rt, machine, in_host);
            }
        }
    }
    Ok(())
}

/// A throwaway container in the machine's network namespace, running `script` as root with
/// the right to change its links.
fn in_container(id: &str, script: &str) -> Command {
    let mut c = Command::new("docker");
    c.args([
        "run",
        "--rm",
        "--net",
        &format!("container:{id}"),
        "--cap-add",
        "NET_ADMIN",
        CAPTURE_IMAGE,
        "sh",
        "-c",
        script,
    ]);
    c
}

/// Cuts again the recorded links of `machine` (it was started, or recreated, with its ports up).
pub(super) fn reapply_links(dir: &Path, spec: &Spec, machine: &str) -> Result<(), RunError> {
    for l in suspended_links(dir).iter().filter(|l| l.machine == machine) {
        let (prefix, _) = ports(spec, &l.network, Some(std::slice::from_ref(&l.machine)))?;
        set_link(dir, spec, machine, &prefix, false)?;
    }
    Ok(())
}

/// Cuts again every recorded link (the lab was started again); what couldn't be, as text.
pub(super) fn reapply_all_links(dir: &Path, spec: &Spec) -> String {
    let mut notes = Vec::new();
    for l in suspended_links(dir) {
        if let Err(e) = reapply_links(dir, spec, &l.machine) {
            notes.push(format!(
                "couldn't suspend `{}`'s link on `{}` again: {e}",
                l.machine, l.network
            ));
        }
    }
    notes.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAB: &str = "version: 1\nname: links\nnetworks:\n  office: { cidr: 10.10.1.0/24 }\n  wire: { cidr: 10.200.0.0/29 }\nmachines:\n  a: { networks: { office: 10, wire: 2 }, app: { image: \"alpine:3.20\" } }\n  b: { networks: { office: 11 }, app: { image: \"alpine:3.20\" } }\n  r: { networks: { wire: 3 }, app: { image: \"alpine:3.20\" } }\n";

    fn link(network: &str, machine: &str) -> Link {
        Link {
            network: network.into(),
            machine: machine.into(),
        }
    }

    #[test]
    fn the_script_sets_the_port_on_the_block() {
        let down = link_script("10.10.1.", false);
        assert!(down.contains("index($4, p) == 1") && down.contains("p='10.10.1.'"));
        assert!(down.ends_with("ip link set \"$IF\" down"), "{down}");
        assert!(link_script("10.10.1.", true).ends_with("ip link set \"$IF\" up"));
    }

    #[test]
    fn the_ports_of_a_link_are_the_machines_on_its_network() {
        let spec = crate::runner::spec_for(LAB, crate::runner::Runtime::Docker).unwrap();
        let (prefix, all) = ports(&spec, "office", None).unwrap();
        assert_eq!(
            (prefix.as_str(), all),
            ("10.10.1.", vec!["a".to_owned(), "b".into()])
        );
        let (_, one) = ports(&spec, "office", Some(&["b".to_owned()])).unwrap();
        assert_eq!(one, ["b"]);
        assert!(ports(&spec, "office", Some(&["r".to_owned()])).is_err());
        assert!(ports(&spec, "nowhere", None).is_err());
    }

    #[test]
    fn the_record_adds_and_removes_links_once() {
        let had = [link("office", "a"), link("wire", "a")];
        let cut = [link("office", "a"), link("office", "b")];
        assert_eq!(
            with(&had, &cut, true),
            [link("wire", "a"), link("office", "a"), link("office", "b")]
        );
        assert_eq!(with(&had, &cut, false), [link("wire", "a")]);
        let dir = std::env::temp_dir().join(format!("isothrum-links-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        save(&dir, &had).unwrap();
        assert_eq!(suspended_links(&dir), had);
        save(&dir, &[]).unwrap();
        assert!(!dir.join(LINKS).exists() && suspended_links(&dir).is_empty());
        clear_links(&dir);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A switch's VLAN is `vlan10` in the editor, `sw1.vlan10` in isoloom.yml and `sw1-vlan10`
    /// once Isoloom has flattened it: every command takes the editor's name.
    #[test]
    fn a_vlan_goes_by_the_editors_name() {
        use crate::runner::{editor_network, spec_network};
        let spec = crate::runner::spec_for("version: 1\nname: v\nnetworks:\n  sw1:\n    cidr: 10.10.0.0/19\n    vlans:\n      10: { cidr: 10.10.10.0/24 }\n      20: { cidr: 10.10.20.0/24 }\n  office: { cidr: 10.20.0.0/24 }\nmachines:\n  pc: { networks: { sw1.vlan10: 10, office: 10 }, app: { image: \"alpine:3.20\" } }\n  srv: { networks: { sw1.vlan20: 10 }, app: { image: \"alpine:3.20\" } }\n", crate::runner::Runtime::Docker).unwrap();
        for name in ["vlan10", "sw1.vlan10", "sw1-vlan10"] {
            assert_eq!(spec_network(&spec, name).unwrap(), "sw1-vlan10", "{name}");
        }
        assert_eq!(spec_network(&spec, "office").unwrap(), "office");
        assert!(spec_network(&spec, "vlan30").is_err());
        assert!(spec_network(&spec, "sw2.vlan10").is_err());
        assert_eq!(editor_network(&spec, "sw1-vlan20"), "vlan20");
        assert_eq!(editor_network(&spec, "office"), "office");
        let (prefix, on) = ports(&spec, &spec_network(&spec, "vlan10").unwrap(), None).unwrap();
        assert_eq!((prefix.as_str(), on), ("10.10.10.", vec!["pc".to_owned()]));
    }

    #[test]
    fn a_link_action_is_suspend_or_resume() {
        let spec = crate::runner::spec_for(LAB, crate::runner::Runtime::Docker).unwrap();
        let dir = std::env::temp_dir().join(format!("isothrum-links-act-{}", std::process::id()));
        let err = link_action(&dir, &spec, "office", None, "cut").unwrap_err();
        assert!(err.to_string().contains("suspend or resume"), "{err}");
    }

    /// Needs Docker (run with `--ignored`): a suspended link carries nothing, and the machine
    /// has its routes back once it's resumed; a restarted machine finds its link cut as before.
    #[test]
    #[ignore = "needs Docker; run with --ignored"]
    fn a_suspended_link_carries_nothing_until_resumed() {
        use crate::runner::{down, machine_action, prepare, up};
        let dir = std::env::temp_dir().join(format!("isothrum-link-{}", std::process::id()));
        let yaml = "version: 1\nname: link\nnetworks:\n  a: { cidr: 10.63.1.0/24 }\n  b: { cidr: 10.63.2.0/24 }\nreach: [{ from: a, to: b }]\nmachines:\n  pc: { networks: { a: 10 }, app: { image: \"alpine:3.20\" }, command: [sleep, infinity] }\n  srv: { networks: { b: 10 }, app: { image: \"alpine:3.20\" }, command: [sleep, infinity] }\n";
        let spec = prepare(&dir, yaml, &[]).unwrap();
        up(&dir).unwrap();
        let pc = |cmd: &str| {
            run(
                compose(&dir, &["exec", "-T", "pc", "sh", "-c", cmd]),
                "exec",
            )
        };
        let reaches = || pc("ping -c 1 -W 2 10.63.2.10").is_ok();
        let port_down = || pc("ip -o link show eth0").unwrap().contains("state DOWN");
        assert!(reaches(), "before: pc reaches srv");
        let links = link_action(&dir, &spec, "a", Some(&["pc".to_owned()]), "suspend").unwrap();
        assert_eq!(links, [link("a", "pc")]);
        assert!(
            port_down() && !reaches(),
            "suspended: the port is down, nothing gets through"
        );
        assert_eq!(link_action(&dir, &spec, "a", None, "resume").unwrap(), []);
        assert!(!port_down() && reaches(), "resumed: pc reaches srv again");
        let routes = pc("ip route").unwrap();
        assert!(
            routes.contains("10.63.2.0/24 via"),
            "its routes are back: {routes}"
        );
        // The link stays cut through a restart of the machine, and the lab forgets it when stopped.
        link_action(&dir, &spec, "a", None, "suspend").unwrap();
        machine_action(&dir, &spec, "pc", "restart").unwrap();
        assert!(port_down(), "restarted: still cut");
        assert_eq!(suspended_links(&dir).len(), 1);
        down(&dir).unwrap();
        assert_eq!(suspended_links(&dir), Vec::<Link>::new());
        drop(std::fs::remove_dir_all(&dir));
    }
}
