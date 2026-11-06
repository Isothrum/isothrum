//! Labs with VMs: run through Isoloom's Vagrant output (only VMs) or its hybrid output (VMs, and
//! the containers in a Docker host VM), with `vagrant` instead of `docker compose`. Machines are
//! Vagrant's (`config.vm.define "<name>"`): a console is `vagrant ssh`, starting and stopping
//! one is `vagrant up` and `halt`, a snapshot is `vagrant snapshot`, a capture is `tcpdump` over
//! `vagrant ssh`. A hybrid lab's containers are reached through their host VM.

use std::path::{Path, PathBuf};
use std::process::Command;

use isoloom_spec::model::Spec;

use crate::console::Console;
use crate::runner::{RunError, Runtime, SHELL, run};

/// The hybrid output's Docker host VM, and where it keeps the containers' Compose file.
pub(crate) const DOCKER_HOST: &str = "isoloom-docker";
const HOST_COMPOSE: &str = "/opt/isoloom/.isoloom/hybrid/compose.yml";

/// The folder Isoloom wrote the Vagrantfile into.
pub(crate) fn dir(lab: &Path, runtime: Runtime) -> PathBuf {
    lab.join(".isoloom").join(match runtime {
        Runtime::Hybrid => "hybrid",
        _ => "vagrant",
    })
}

/// `vagrant` in the lab's Vagrant folder.
pub(crate) fn vagrant(lab: &Path, runtime: Runtime, args: &[&str]) -> Command {
    let mut c = Command::new("vagrant");
    c.current_dir(dir(lab, runtime)).args(args);
    c
}

/// Whether `machine` is one of a hybrid lab's containers (those run in the Docker host VM).
pub(crate) fn in_host(spec: &Spec, runtime: Runtime, machine: &str) -> bool {
    runtime == Runtime::Hybrid
        && spec
            .machines
            .get(machine)
            .is_some_and(|m| m.docker.is_some())
}

/// A shell command run as root in a VM, base64-encoded so that its quotes survive `ssh -c`.
pub(crate) fn ssh_root(lab: &Path, runtime: Runtime, vm: &str, script: &str) -> Command {
    vagrant(
        lab,
        runtime,
        &[
            "ssh",
            vm,
            "-c",
            &format!("echo {} | base64 -d | sudo sh", base64(script.as_bytes())),
        ],
    )
}

pub(crate) fn up(lab: &Path, runtime: Runtime) -> Result<String, RunError> {
    run(vagrant(lab, runtime, &["up"]), "Starting the lab's VMs")
}

pub(crate) fn down(lab: &Path, runtime: Runtime) -> Result<String, RunError> {
    run(
        vagrant(lab, runtime, &["destroy", "-f"]),
        "Removing the lab's VMs",
    )
}

/// The machines' states, in the shape the editor reads (`docker compose ps` JSON lines):
/// `{"Service": <machine>, "State": "running" | "exited"}`. A hybrid lab adds its containers'.
pub(crate) fn status(lab: &Path, runtime: Runtime) -> Result<String, RunError> {
    let out = run(
        vagrant(lab, runtime, &["status", "--machine-readable"]),
        "Reading the lab's state",
    )?;
    let mut lines = states(&out);
    if runtime == Runtime::Hybrid
        && lines.contains(&format!(
            "\"Service\":\"{DOCKER_HOST}\",\"State\":\"running\""
        ))
    {
        let script = format!("docker compose -f {HOST_COMPOSE} ps --all --format json");
        if let Ok(containers) = run(
            ssh_root(lab, runtime, DOCKER_HOST, &script),
            "Reading the containers' state",
        ) {
            lines.push('\n');
            lines.push_str(containers.trim());
        }
    }
    Ok(lines)
}

/// `vagrant status --machine-readable` (`time,machine,state,<state>` lines) as JSON lines.
pub(crate) fn states(machine_readable: &str) -> String {
    machine_readable
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(',').collect();
            match f.as_slice() {
                [_, name, "state", state, ..] if !name.is_empty() => {
                    let state = if *state == "running" {
                        "running"
                    } else {
                        "exited"
                    };
                    Some(serde_json::json!({ "Service": *name, "State": state }).to_string())
                }
                _ => None,
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A console: `vagrant ssh` to a VM, or a shell in a hybrid lab's container through its host.
pub(crate) fn console_command(lab: &Path, spec: &Spec, runtime: Runtime, machine: &str) -> Console {
    let console = if in_host(spec, runtime, machine) {
        let shell = format!("sudo docker compose -f {HOST_COMPOSE} exec {machine} sh -c '{SHELL}'");
        Console::new(&["vagrant", "ssh", DOCKER_HOST, "-c", &shell])
    } else {
        Console::new(&["vagrant", "ssh", machine])
    };
    console.in_dir(&dir(lab, runtime))
}

/// Starts, stops or restarts one machine.
pub(crate) fn action(
    lab: &Path,
    spec: &Spec,
    runtime: Runtime,
    machine: &str,
    action: &str,
) -> Result<String, RunError> {
    if in_host(spec, runtime, machine) {
        let verb = match action {
            "start" => "start",
            "stop" => "stop",
            "restart" => "restart",
            other => {
                return Err(RunError::Spec(format!(
                    "unknown action `{other}`: start, stop or restart"
                )));
            }
        };
        let script = format!("docker compose -f {HOST_COMPOSE} {verb} {machine}");
        return run(
            ssh_root(lab, runtime, DOCKER_HOST, &script),
            "Changing the machine's state",
        );
    }
    let (args, what): (&[&str], &str) = match action {
        "start" => (&["up", machine], "Starting the machine"),
        "stop" => (&["halt", machine], "Stopping the machine"),
        "restart" => (&["reload", machine], "Restarting the machine"),
        other => {
            return Err(RunError::Spec(format!(
                "unknown action `{other}`: start, stop or restart"
            )));
        }
    };
    run(vagrant(lab, runtime, args), what)
}

// Snapshots of a VM: Vagrant's (the provider's own: VirtualBox, libvirt...), named by the id.

pub(crate) fn snapshot_save(
    lab: &Path,
    runtime: Runtime,
    machine: &str,
    id: &str,
) -> Result<String, RunError> {
    run(
        vagrant(lab, runtime, &["snapshot", "save", machine, id]),
        "Saving the machine",
    )
}

pub(crate) fn snapshot_restore(
    lab: &Path,
    runtime: Runtime,
    machine: &str,
    id: &str,
) -> Result<String, RunError> {
    run(
        vagrant(
            lab,
            runtime,
            &["snapshot", "restore", machine, id, "--no-provision"],
        ),
        "Restoring the machine",
    )
}

pub(crate) fn snapshot_delete(
    lab: &Path,
    runtime: Runtime,
    machine: &str,
    id: &str,
) -> Result<String, RunError> {
    run(
        vagrant(lab, runtime, &["snapshot", "delete", machine, id]),
        "Removing the snapshot",
    )
}

/// `script`, as root in `machine`'s network namespace: in the VM over `vagrant ssh`, or for a
/// hybrid lab's container, in the capture image on its host joined to the container's namespace.
pub(crate) fn in_namespace(
    lab: &Path,
    runtime: Runtime,
    machine: &str,
    in_host: bool,
    script: &str,
) -> Command {
    if !in_host {
        return ssh_root(lab, runtime, machine, script);
    }
    let inner = script.replace('\'', "'\\''");
    let host = format!(
        "docker run --rm -i --net container:$(docker compose -f {HOST_COMPOSE} ps -q {machine}) --cap-add NET_RAW --cap-add NET_ADMIN {} sh -c '{inner}'",
        crate::runner::CAPTURE_IMAGE
    );
    ssh_root(lab, runtime, DOCKER_HOST, &host)
}

/// A capture from `machine`: the container capture's script (find the interface on the network,
/// pcap on stdout) in its namespace, with tcpdump installed first in a VM that lacks it.
pub(crate) fn capture_command(
    lab: &Path,
    runtime: Runtime,
    machine: &str,
    in_host: bool,
    script: &str,
) -> Command {
    let full = if in_host {
        script.to_owned()
    } else {
        format!(
            "command -v tcpdump >/dev/null || {{ (apt-get install -y -qq tcpdump || dnf install -y -q tcpdump) >/dev/null 2>&1; }}; {script}"
        )
    };
    in_namespace(lab, runtime, machine, in_host, &full)
}

/// Sets `machine`'s routes again after a link of its came back (the kernel dropped those
/// through it): Isoloom's `routes` provisioners for a VM, a hybrid lab's container's sidecar
/// restarted on its host. Best effort: a machine without routes has nothing to set.
pub(crate) fn restore_routes(lab: &Path, runtime: Runtime, machine: &str, in_host: bool) {
    let cmd = if in_host {
        let script = format!(
            "cd $(dirname {HOST_COMPOSE}) && docker compose -f {HOST_COMPOSE} config --services | grep -qx {machine}-routes && docker compose -f {HOST_COMPOSE} restart {machine}-routes || true"
        );
        ssh_root(lab, runtime, DOCKER_HOST, &script)
    } else {
        vagrant(
            lab,
            runtime,
            &[
                "provision",
                machine,
                "--provision-with",
                "routes,default route",
            ],
        )
    };
    drop(run(cmd, "Setting the routes again"));
}

/// Standard, padded base64 (one small use: no dependency for it).
fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            *chunk.first().unwrap_or(&0),
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let v = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                // `& 63` keeps the sextet in 0..64, so the conversion and the lookup always hold.
                let sextet = u8::try_from((v >> (18 - 6 * i)) & 63).unwrap_or(0);
                char::from(*A.get(usize::from(sextet)).unwrap_or(&b'A'))
            } else {
                '='
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vagrant_states_become_the_editors_json_lines() {
        let out = "1700000000,,metadata,provider,virtualbox\n1700000000,web,state,running\n1700000000,db,state,poweroff\n1700000000,web,state-human-short,running\n";
        assert_eq!(
            states(out),
            "{\"Service\":\"web\",\"State\":\"running\"}\n{\"Service\":\"db\",\"State\":\"exited\"}"
        );
    }

    /// GNU `base64` (`-w0`): Linux only.
    #[test]
    #[cfg(target_os = "linux")]
    fn base64_matches_the_systems() {
        for s in ["", "a", "ab", "abc", "echo 'a b' && awk '{print $2}'\n"] {
            let sys = Command::new("sh")
                .args(["-c", "printf %s \"$0\" | base64 -w0", s])
                .output()
                .unwrap()
                .stdout;
            assert_eq!(
                base64(s.as_bytes()),
                String::from_utf8(sys).unwrap(),
                "{s:?}"
            );
        }
    }

    #[test]
    fn a_hybrid_labs_containers_go_through_their_host() {
        // A machine set up by a playbook has no container form: the lab mixes a VM and a container.
        let spec = crate::runner::spec_for(
            "version: 1\nname: mixed\nnetworks:\n  lan: {cidr: 10.0.1.0/24}\nmachines:\n  web:\n    networks: {lan: 10}\n    app: {image: nginx}\n  pc:\n    networks: {lan: 20}\n    os: debian@12\n    app: {steps: [{playbook: provision/pc.yml}]}\n",
            Runtime::Hybrid,
        )
        .unwrap();
        assert!(in_host(&spec, Runtime::Hybrid, "web"));
        assert!(!in_host(&spec, Runtime::Hybrid, "pc"));
        let c = console_command(Path::new("/lab"), &spec, Runtime::Hybrid, "web").sh_line();
        assert!(
            c.contains("vagrant ssh isoloom-docker -c") && c.contains("exec web"),
            "{c}"
        );
        assert!(
            console_command(Path::new("/lab"), &spec, Runtime::Hybrid, "pc")
                .sh_line()
                .ends_with("vagrant ssh pc")
        );
        let cap = capture_command(
            Path::new("/lab"),
            Runtime::Vagrant,
            "pc",
            false,
            "tcpdump -w -",
        );
        let args: Vec<String> = cap
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&args[..3], ["ssh", "pc", "-c"]);
    }
}

#[cfg(test)]
mod vagrant_tests {
    use crate::runner::{Runtime, prepare, runtime};

    /// Vagrant accepts the Vagrantfiles the app prepares (needs `vagrant`; `ISOTHRUM_VM_LAB`
    /// keeps the folder).
    #[test]
    #[ignore = "needs Vagrant; run with --ignored"]
    fn vagrant_validates_the_prepared_labs() {
        let base = std::env::var("ISOTHRUM_VM_LAB")
            .map_or_else(|_| std::env::temp_dir(), std::path::PathBuf::from);
        // Playbook steps: VMs only (a script's steps would build containers too, run on Docker).
        let mixed = "version: 1\nname: mixed\nnetworks:\n  office: {cidr: 10.10.1.0/24}\nmachines:\n  web:\n    networks: {office: 10}\n    app: {image: \"nginx:1.27\"}\n  pc:\n    networks: {office: 20}\n    os: debian@12\n    app: {steps: [{playbook: provision/pc.yml}]}\n";
        let vms = "version: 1\nname: vms\nnetworks:\n  office: {cidr: 10.10.1.0/24}\nmachines:\n  a:\n    networks: {office: 10}\n    os: debian@12\n    app: {steps: [{playbook: provision/a.yml}]}\n  b:\n    networks: {office: 11}\n    os: ubuntu@24.04\n    app: {steps: [{playbook: provision/b.yml}]}\n";
        let script = || "- hosts: all\n  tasks: []\n".to_owned();
        for (name, yaml, rt) in [
            ("mixed", mixed, Runtime::Hybrid),
            ("vms", vms, Runtime::Vagrant),
        ] {
            let dir = base.join(name);
            let own: Vec<(String, String)> = ["pc", "a", "b"]
                .iter()
                .map(|m| (format!("provision/{m}.yml"), script()))
                .collect();
            prepare(&dir, yaml, &own).unwrap();
            assert_eq!(runtime(&dir), rt);
            let out = super::vagrant(&dir, rt, &["validate"]).output().unwrap();
            assert!(
                out.status.success(),
                "{name}: {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let st = super::status(&dir, rt).unwrap();
            assert!(st.contains("\"State\":\"exited\""), "{name}: {st}");
        }
    }
}
