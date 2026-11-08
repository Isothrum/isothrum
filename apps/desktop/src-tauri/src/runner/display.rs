//! A machine's screen, as GNS3's VNC console: what the machine shows, in a viewer on this
//! computer.
//!
//! - A container whose image runs a VNC server (it exposes a port in 5900–5999): a forwarder
//!   container joins one of its networks and publishes that port on 127.0.0.1, then the system
//!   opens `vnc://127.0.0.1:<port>` (Screen Sharing on macOS, the default VNC viewer elsewhere).
//!   Container addresses aren't reachable from the host under Docker Desktop: the forwarder is.
//! - A VirtualBox VM: its own screen window (`VBoxManage startvm --type separate`).
//! - A libvirt VM: its VNC or SPICE display, as `virsh domdisplay` gives it.
//!
//! Stopping the lab removes its forwarders.

use std::path::Path;
use std::process::Command;

use isoloom_spec::model::Spec;

use super::{CAPTURE_IMAGE, RunError, Runtime, container, run, runtime};

/// The label that marks a lab's VNC forwarders (its value: the lab's folder name).
const LABEL: &str = "isothrum.vnc";

/// Opens `machine`'s screen; returns what was opened (a `vnc://` address, or the VM's window).
pub(crate) fn open_display(dir: &Path, spec: &Spec, machine: &str) -> Result<String, RunError> {
    if !spec.machines.contains_key(machine) {
        return Err(RunError::Spec(format!("no machine named `{machine}`")));
    }
    match runtime(dir) {
        Runtime::Docker => container_display(dir, machine),
        rt if crate::vms::in_host(spec, rt, machine) => Err(RunError::Spec(format!(
            "`{machine}` is a container in a lab with VMs: open its console instead"
        ))),
        rt => vm_display(dir, rt, machine),
    }
}

/// The first VNC port (5900–5999) among a container's exposed ports, from
/// `{{json .Config.ExposedPorts}}` (`{"5900/tcp":{},"80/tcp":{}}`).
pub(super) fn vnc_port(exposed: &str) -> Option<u16> {
    let ports: serde_json::Value = serde_json::from_str(exposed.trim()).ok()?;
    let mut found: Vec<u16> = ports
        .as_object()?
        .keys()
        .filter_map(|k| k.strip_suffix("/tcp")?.parse().ok())
        .filter(|p| (5900..=5999).contains(p))
        .collect();
    found.sort_unstable();
    found.first().copied()
}

/// The host side of `docker port <c> 5900/tcp` (`127.0.0.1:49153`, possibly one per line).
pub(super) fn published(out: &str) -> Option<String> {
    out.lines()
        .map(str::trim)
        .find(|l| l.starts_with("127.0.0.1:"))
        .map(str::to_owned)
}

/// A container's networks and its address on each (`docker inspect`), Isoloom's management
/// network last.
fn networks(id: &str) -> Result<Vec<(String, String)>, RunError> {
    let mut c = Command::new("docker");
    c.args([
        "inspect",
        "-f",
        "{{range $n, $v := .NetworkSettings.Networks}}{{$n}} {{$v.IPAddress}}{{println}}{{end}}",
        id,
    ]);
    let mut nets: Vec<(String, String)> = run(c, "Finding its networks")?
        .lines()
        .filter_map(|l| {
            let (n, ip) = l.split_once(' ')?;
            (!ip.trim().is_empty()).then(|| (n.to_owned(), ip.trim().to_owned()))
        })
        .collect();
    nets.sort_by_key(|(n, _)| n.ends_with("isoloom-mgmt"));
    Ok(nets)
}

/// The forwarder's name for `machine` of the lab in `dir`.
fn forwarder(dir: &Path, machine: &str) -> String {
    format!("isothrum-vnc-{}-{machine}", lab_of(dir))
}

fn lab_of(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Publishes `ip:port` on 127.0.0.1 through a forwarder on `network`; returns `127.0.0.1:<port>`.
pub(super) fn forward(
    dir: &Path,
    machine: &str,
    network: &str,
    ip: &str,
    port: u16,
) -> Result<String, RunError> {
    let name = forwarder(dir, machine);
    drop(Command::new("docker").args(["rm", "-f", &name]).output());
    let mut c = Command::new("docker");
    c.args([
        "run",
        "-d",
        "--name",
        &name,
        "--label",
        &format!("{LABEL}={}", lab_of(dir)),
        "--network",
        network,
        "-p",
        "127.0.0.1::5900",
        CAPTURE_IMAGE,
        "socat",
        "TCP-LISTEN:5900,fork,reuseaddr",
        &format!("TCP:{ip}:{port}"),
    ]);
    run(c, "Starting the VNC forwarder")?;
    let mut p = Command::new("docker");
    p.args(["port", &name, "5900/tcp"]);
    published(&run(p, "Finding the VNC port")?).ok_or_else(|| RunError::Command {
        what: "Finding the VNC port".into(),
        output: "the forwarder publishes nothing".into(),
    })
}

/// A container's screen, when its image runs a VNC server.
fn container_display(dir: &Path, machine: &str) -> Result<String, RunError> {
    let id = container(dir, machine)?;
    let mut c = Command::new("docker");
    c.args(["inspect", "-f", "{{json .Config.ExposedPorts}}", &id]);
    let port = vnc_port(&run(c, "Reading its ports")?).ok_or_else(|| {
        RunError::Spec(format!(
            "`{machine}` has no screen: its image runs no VNC server (none of its ports is in \
             5900–5999). Open its console instead."
        ))
    })?;
    let (network, ip) = networks(&id)?
        .into_iter()
        .next()
        .ok_or_else(|| RunError::Spec(format!("`{machine}` is on no network")))?;
    let url = format!("vnc://{}", forward(dir, machine, &network, &ip, port)?);
    crate::terminal::open_url(&url)?;
    Ok(url)
}

/// A VM's screen: VirtualBox's window, or libvirt's VNC/SPICE display.
fn vm_display(dir: &Path, rt: Runtime, machine: &str) -> Result<String, RunError> {
    let state = crate::vms::dir(dir, rt)
        .join(".vagrant/machines")
        .join(machine);
    let id = |provider: &str| {
        std::fs::read_to_string(state.join(provider).join("id"))
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    if let Some(id) = id("virtualbox") {
        let mut c = Command::new("VBoxManage");
        c.args(["startvm", &id, "--type", "separate"]);
        run(c, "Opening the VM's screen")?;
        return Ok(format!("{machine}: VirtualBox"));
    }
    if let Some(id) = id("libvirt") {
        let mut c = Command::new("virsh");
        c.args(["-c", "qemu:///system", "domdisplay", &id]);
        let url = run(c, "Finding the VM's display")?.trim().to_owned();
        if url.is_empty() {
            return Err(RunError::Spec(format!("`{machine}` has no display")));
        }
        crate::terminal::open_url(&url)?;
        return Ok(url);
    }
    Err(RunError::Spec(format!(
        "`{machine}` isn't running, or its provider shows no screen from here (VirtualBox and \
         libvirt do): open its console instead"
    )))
}

/// Removes the lab's VNC forwarders (the lab is stopping).
pub(super) fn remove_forwarders(dir: &Path) {
    let filter = format!("label={LABEL}={}", lab_of(dir));
    let Ok(ids) = run(
        {
            let mut c = Command::new("docker");
            c.args(["ps", "-aq", "--filter", &filter]);
            c
        },
        "Finding the VNC forwarders",
    ) else {
        return;
    };
    let ids: Vec<&str> = ids.split_whitespace().collect();
    if !ids.is_empty() {
        drop(
            Command::new("docker")
                .args([&["rm", "-f"][..], &ids].concat())
                .output(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vnc_port_is_the_first_in_5900_5999() {
        assert_eq!(
            vnc_port(r#"{"80/tcp":{},"5901/tcp":{},"5900/tcp":{}}"#),
            Some(5900)
        );
        assert_eq!(vnc_port(r#"{"80/tcp":{}}"#), None);
        assert_eq!(vnc_port("null"), None);
        assert_eq!(vnc_port(r#"{"5900/udp":{}}"#), None);
    }

    #[test]
    fn the_published_port_is_on_localhost() {
        assert_eq!(
            published("127.0.0.1:49153\n").as_deref(),
            Some("127.0.0.1:49153")
        );
        assert_eq!(
            published("[::1]:49153\n127.0.0.1:49154"),
            Some("127.0.0.1:49154".into())
        );
        assert_eq!(published(""), None);
    }

    /// Needs Docker (run with `--ignored`): the forwarder reaches a machine's port 5900 from
    /// 127.0.0.1, and stopping the lab removes it.
    #[test]
    #[ignore = "needs Docker; run with --ignored"]
    fn a_machines_vnc_port_is_reachable_from_this_computer() {
        use crate::runner::{compose, down, prepare, up};
        use std::io::Read;
        let dir = std::env::temp_dir().join(format!("isothrum-vnc-{}", std::process::id()));
        let yaml = "version: 1\nname: vnc\nnetworks:\n  a: { cidr: 10.66.1.0/24 }\nmachines:\n  desk: { networks: { a: 10 }, app: { image: \"alpine:3.20\" }, command: [sleep, infinity] }\n";
        prepare(&dir, yaml, &[]).unwrap();
        up(&dir, None).unwrap();
        // A stand-in VNC server: it greets with the RFB banner, as one does.
        run(
            compose(
                &dir,
                &[
                    "exec",
                    "-d",
                    "desk",
                    "sh",
                    "-c",
                    "while true; do echo 'RFB 003.008' | nc -l -p 5900; done",
                ],
            ),
            "server",
        )
        .unwrap();
        let id = container(&dir, "desk").unwrap();
        let (network, ip) = networks(&id).unwrap().remove(0);
        assert_eq!(ip, "10.66.1.10", "the lab's network comes first");
        let at = forward(&dir, "desk", &network, &ip, 5900).unwrap();
        let mut banner = String::new();
        for _ in 0..30 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if let Ok(mut s) = std::net::TcpStream::connect(&at) {
                s.set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                drop(s.read_to_string(&mut banner));
                if !banner.is_empty() {
                    break;
                }
            }
        }
        down(&dir).unwrap();
        let left = Command::new("docker")
            .args([
                "ps",
                "-aq",
                "--filter",
                &format!("name={}", forwarder(&dir, "desk")),
            ])
            .output()
            .unwrap();
        drop(std::fs::remove_dir_all(&dir));
        assert!(banner.starts_with("RFB 003.008"), "{banner:?} from {at}");
        assert!(
            left.stdout.is_empty(),
            "the forwarder is removed with the lab"
        );
    }
}
