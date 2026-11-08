//! Capturing a LAN's or a cable's traffic: tcpdump in the namespace of a machine on it, into
//! Wireshark or a `.pcap` file.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use isoloom_spec::model::Spec;

use super::{RunError, Runtime, compose, run, runtime};
use crate::tools::has;

/// The image the capture runs in: tcpdump, ip and a shell, nothing to install.
pub(crate) const CAPTURE_IMAGE: &str = "nicolaka/netshoot:v0.13";

/// The first three octets of a /24 to /29 block, with the trailing dot: what every address on
/// that network starts with (`10.10.1.0/24` -> `10.10.1.`).
pub(super) fn prefix_of(cidr: &str) -> Option<String> {
    let addr = cidr.split('/').next()?;
    let (head, _) = addr.rsplit_once('.')?;
    Some(format!("{head}."))
}

/// The shell that finds the interface whose address starts with `prefix`, in `$IF` (a VLAN's
/// `eth1.10@eth1` without its parent), or fails. The prefix is checked by the caller to be
/// digits and dots only, so it can't break out of the quotes.
pub(super) fn interface_script(prefix: &str) -> String {
    format!(
        "IF=$(ip -o -4 addr show | awk -v p='{prefix}' 'index($4, p) == 1 {{ print $2; exit }}'); \
         IF=${{IF%%@*}}; \
         if [ -z \"$IF\" ]; then echo 'no interface on {prefix}0' >&2; exit 1; fi;"
    )
}

/// The shell run inside the capture container: find the interface on `prefix`'s network, then
/// stream its packets as pcap on stdout.
pub(super) fn capture_script(prefix: &str) -> String {
    format!(
        "{} exec tcpdump -i \"$IF\" -U -w -",
        interface_script(prefix)
    )
}

/// Where to capture: a machine on the network, by its container, and the network's block.
pub(crate) struct CapturePlan {
    pub container: String,
    pub prefix: String,
    /// In a lab with VMs: the lab's folder, its runtime, the machine, and whether it's a
    /// container in the Docker host VM.
    pub vm: Option<(PathBuf, Runtime, String, bool)>,
}

/// Picks the machine to capture from: on `network`, the first one Isoloom attached to it.
/// Every machine on a LAN or cable sees its traffic on that segment, so any member works.
pub(crate) fn plan_capture(
    dir: &Path,
    spec: &Spec,
    network: &str,
) -> Result<CapturePlan, RunError> {
    let network = &super::spec_network(spec, network)?;
    let net = spec
        .networks
        .get(network)
        .ok_or_else(|| RunError::Spec(format!("the lab has no network `{network}`")))?;
    let prefix = prefix_of(&net.cidr)
        .filter(|p| p.chars().all(|c| c.is_ascii_digit() || c == '.'))
        .ok_or_else(|| RunError::Spec(format!("`{}` isn't an IPv4 block", net.cidr)))?;
    match runtime(dir) {
        Runtime::Docker => docker_plan(dir, spec, network, prefix),
        rt => vm_plan(dir, spec, rt, network, prefix),
    }
}

/// In a lab with VMs: the first machine on `network`, reached through Vagrant.
fn vm_plan(
    dir: &Path,
    spec: &Spec,
    rt: Runtime,
    network: &str,
    prefix: String,
) -> Result<CapturePlan, RunError> {
    let machine = spec
        .machines
        .iter()
        .find(|(_, m)| m.networks.contains_key(network))
        .map(|(name, _)| name.clone())
        .ok_or_else(|| RunError::Spec(format!("no machine is on `{network}`")))?;
    let in_host = crate::vms::in_host(spec, rt, &machine);
    Ok(CapturePlan {
        container: String::new(),
        prefix,
        vm: Some((dir.to_path_buf(), rt, machine, in_host)),
    })
}

/// In a lab of containers: the running container of the first container on `network`.
fn docker_plan(
    dir: &Path,
    spec: &Spec,
    network: &str,
    prefix: String,
) -> Result<CapturePlan, RunError> {
    let machine = spec
        .machines
        .iter()
        .find(|(_, m)| m.networks.contains_key(network) && m.docker.is_some())
        .map(|(name, _)| name.clone())
        .ok_or_else(|| RunError::Spec(format!("no container is on `{network}`")))?;
    let id = run(compose(dir, &["ps", "-q", &machine]), "Finding the machine")?;
    let container = id.lines().next().unwrap_or("").trim().to_owned();
    if container.is_empty() {
        return Err(RunError::Spec(format!(
            "`{machine}` isn't running: start the lab first"
        )));
    }
    Ok(CapturePlan {
        container,
        prefix,
        vm: None,
    })
}

/// The capture process: tcpdump in the machine's network namespace, pcap on stdout. On Docker,
/// its container is called `name`, so that stopping the capture can remove it.
pub(super) fn capture_command(plan: &CapturePlan, name: &str) -> Command {
    if let Some((dir, rt, machine, in_host)) = &plan.vm {
        return crate::vms::capture_command(
            dir,
            *rt,
            machine,
            *in_host,
            &capture_script(&plan.prefix),
        );
    }
    let mut c = Command::new("docker");
    c.args([
        "run",
        "--rm",
        "-i",
        "--name",
        name,
        "--net",
        &format!("container:{}", plan.container),
        "--cap-add",
        "NET_RAW",
        "--cap-add",
        "NET_ADMIN",
        CAPTURE_IMAGE,
        "sh",
        "-c",
        &capture_script(&plan.prefix),
    ]);
    c
}

/// A running capture: the tcpdump container, and Wireshark when it reads from it.
pub(crate) struct Capture {
    pub children: Vec<Child>,
    /// Where the packets go when Wireshark isn't there.
    pub file: Option<PathBuf>,
    /// The capture's container (Docker labs): killing its `docker run` client may leave it.
    pub container: Option<String>,
}

impl Capture {
    pub(crate) fn stop(&mut self) {
        for c in &mut self.children {
            stop_child(c);
        }
        if let Some(name) = &self.container {
            drop(remove_container(name).output());
        }
    }
}

fn stop_child(c: &mut Child) {
    drop(c.kill());
    drop(c.wait());
}

/// `docker rm -f <name>`.
fn remove_container(name: &str) -> Command {
    let mut c = Command::new("docker");
    c.args(["rm", "-f", name]);
    c
}

/// A name no other capture has: `isothrum-capture-<pid>-<n>`.
fn capture_name() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("isothrum-capture-{}-{n}", std::process::id())
}

/// Whether Wireshark can show a capture live: it hands even a piped capture to `dumpcap`, which
/// Linux distributions restrict to the `wireshark` group. Without it, Wireshark opens only to
/// fail with "Couldn't run dumpcap in child process: Permission denied".
pub(crate) fn live_capture_possible() -> bool {
    has("wireshark")
        && Command::new("dumpcap")
            .arg("-v")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

/// Starts a capture: live into Wireshark when it can show it, otherwise into `file`.
pub(crate) fn start_capture(plan: &CapturePlan, file: &Path) -> Result<Capture, RunError> {
    let name = capture_name();
    let dump = capture_command(plan, &name);
    let container = plan.vm.is_none().then_some(name);
    let (children, file) = if live_capture_possible() {
        (into_wireshark(dump)?, None)
    } else {
        (vec![into_file(dump, file)?], Some(file.to_path_buf()))
    };
    Ok(Capture {
        children,
        file,
        container,
    })
}

/// Runs the capture into Wireshark; stops the capture when Wireshark doesn't start.
fn into_wireshark(mut dump: Command) -> Result<Vec<Child>, RunError> {
    let mut dump = dump
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| RunError::Io(e.to_string()))?;
    let wireshark = dump
        .stdout
        .take()
        .ok_or_else(|| RunError::Io("no capture output".into()))
        .and_then(|out| {
            Command::new("wireshark")
                .args(["-k", "-i", "-"])
                .stdin(Stdio::from(out))
                .spawn()
                .map_err(|e| RunError::Io(format!("couldn't start Wireshark: {e}")))
        });
    match wireshark {
        Ok(w) => Ok(vec![dump, w]),
        Err(e) => {
            stop_child(&mut dump);
            Err(e)
        }
    }
}

/// Runs the capture into a `.pcap` file.
fn into_file(mut dump: Command, file: &Path) -> Result<Child, RunError> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| RunError::Io(e.to_string()))?;
    }
    let out = std::fs::File::create(file).map_err(|e| RunError::Io(e.to_string()))?;
    dump.stdout(out)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| RunError::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capture_finds_the_interface_by_its_network() {
        assert_eq!(prefix_of("10.10.1.0/24").as_deref(), Some("10.10.1."));
        assert_eq!(prefix_of("10.200.0.8/29").as_deref(), Some("10.200.0."));
        let script = capture_script("10.10.1.");
        assert!(script.contains("p='10.10.1.'"));
        assert!(script.contains("exec tcpdump -i \"$IF\" -U -w -"));
    }

    #[test]
    fn the_capture_runs_in_the_machine_namespace() {
        let plan = CapturePlan {
            container: "abc123".into(),
            prefix: "10.10.1.".into(),
            vm: None,
        };
        let cmd = capture_command(&plan, "isothrum-capture-1-0");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.windows(2).any(|w| w == ["--net", "container:abc123"]));
        assert!(args.contains(&CAPTURE_IMAGE.to_owned()));
        assert!(
            args.windows(2)
                .any(|w| w == ["--name", "isothrum-capture-1-0"])
        );
    }

    /// Needs Docker (run with `--ignored`): a capture on a switch's VLAN, named as the editor
    /// names it (`vlan10`), records that VLAN's traffic.
    #[test]
    #[ignore = "needs Docker; run with --ignored"]
    fn a_capture_on_a_vlan_records_its_traffic() {
        use crate::runner::{down, prepare, up};
        let dir = std::env::temp_dir().join(format!("isothrum-vcap-{}", std::process::id()));
        let yaml = "version: 1\nname: vcap\nnetworks:\n  sw1:\n    cidr: 10.65.0.0/19\n    vlans:\n      10: { cidr: 10.65.10.0/24 }\nmachines:\n  pc: { networks: { sw1.vlan10: 10 }, app: { image: \"alpine:3.20\" }, command: [sleep, infinity] }\n  srv: { networks: { sw1.vlan10: 11 }, app: { image: \"alpine:3.20\" }, command: [sleep, infinity] }\n";
        let spec = prepare(&dir, yaml, &[]).unwrap();
        up(&dir).unwrap();
        let plan = plan_capture(&dir, &spec, "vlan10").unwrap();
        let file = dir.join("vlan10.pcap");
        let name = capture_name();
        let mut capture = Capture {
            children: vec![into_file(capture_command(&plan, &name), &file).unwrap()],
            file: Some(file.clone()),
            container: Some(name),
        };
        // tcpdump writes the pcap header when it starts: after the capture image's download,
        // the first time.
        let started = (0..240).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(500));
            std::fs::metadata(&file).is_ok_and(|m| m.len() >= 24)
        });
        assert!(started, "tcpdump didn't start");
        run(
            compose(
                &dir,
                &["exec", "-T", "pc", "ping", "-c", "3", "10.65.10.11"],
            ),
            "ping",
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        capture.stop();
        // A pcap header is 24 bytes; three pings and their replies are well over 300.
        let size = std::fs::metadata(&file).unwrap().len();
        down(&dir).unwrap();
        drop(std::fs::remove_dir_all(&dir));
        assert!(size > 300, "the capture has {size} bytes");
    }

    #[test]
    fn captures_have_their_own_names() {
        assert_ne!(capture_name(), capture_name());
        let rm = remove_container("isothrum-capture-1-0");
        let args: Vec<&std::ffi::OsStr> = rm.get_args().collect();
        assert_eq!(args, ["rm", "-f", "isothrum-capture-1-0"]);
    }
}
