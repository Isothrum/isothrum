//! One machine of a running lab: its console, and starting or stopping it alone.

use std::path::Path;
use std::process::Command;

use isoloom_spec::model::Spec;

use super::{RunError, Runtime, compose, run, runtime};
use crate::console::Console;

/// The machine's container, when it runs.
pub(super) fn running_container(dir: &Path, machine: &str) -> Result<Option<String>, RunError> {
    let id = run(compose(dir, &["ps", "-q", machine]), "Finding the machine")?
        .trim().to_owned();
    Ok((!id.is_empty()).then_some(id))
}

/// The machine's container in the running lab.
pub(crate) fn container(dir: &Path, machine: &str) -> Result<String, RunError> {
    running_container(dir, machine)?.ok_or_else(|| RunError::Command {
        what: format!("`{machine}`"),
        output: "isn't running".into(),
    })
}

/// A container's shell: bash when it has it, else sh.
pub(crate) const SHELL: &str = "command -v bash >/dev/null && exec bash || exec sh";

/// The command a console on `machine` runs: a shell in its container, or SSH to a network
/// appliance's OS (its management address; `admin`/`admin`).
pub(crate) fn console_command(dir: &Path, spec: &Spec, machine: &str) -> Result<Console, RunError> {
    let m = spec
        .machines
        .get(machine)
        .ok_or_else(|| RunError::Spec(format!("no machine named `{machine}`")))?;
    if let rt @ (Runtime::Vagrant | Runtime::Hybrid) = runtime(dir) {
        return Ok(crate::vms::console_command(dir, spec, rt, machine));
    }
    let id = container(dir, machine)?;
    if m.docker.as_ref().is_some_and(|d| d.appliance.is_some()) {
        return appliance_console(&id, machine);
    }
    Ok(Console::new(&[
        "docker", "exec", "-it", &id, "sh", "-c", SHELL,
    ]))
}

/// SSH to a network appliance's OS, at its management address (`admin`/`admin`), from a helper
/// container on the management network: under Docker Desktop (macOS, Windows) the host can't
/// reach container addresses.
fn appliance_console(id: &str, machine: &str) -> Result<Console, RunError> {
    let (network, ip) = management_address(id)?.ok_or_else(|| RunError::Command {
        what: format!("`{machine}`"),
        output: "has no management address".into(),
    })?;
    Ok(Console::new(&[
        "docker",
        "run",
        "-it",
        "--rm",
        "--network",
        &network,
        super::CAPTURE_IMAGE,
        "ssh",
        "-o",
        "StrictHostKeyChecking=no",
        "-o",
        "UserKnownHostsFile=/dev/null",
        &format!("admin@{ip}"),
    ]))
}

/// A container's management network and its address there (Isoloom's management network).
fn management_address(id: &str) -> Result<Option<(String, String)>, RunError> {
    let mut c = Command::new("docker");
    c.args([
        "inspect",
        "-f",
        "{{range $n, $v := .NetworkSettings.Networks}}{{$n}} {{$v.IPAddress}}{{println}}{{end}}",
        id,
    ]);
    let nets = run(c, "Finding its management address")?;
    Ok(nets
        .lines()
        .find_map(|l| {
            l.split_once(' ')
                .filter(|(n, _)| n.ends_with("isoloom-mgmt"))
                .map(|(n, ip)| (n.to_owned(), ip.trim().to_owned()))
        })
        .filter(|(_, ip)| !ip.is_empty()))
}

/// Starts, stops or restarts one machine (and its network sidecar, which sets its routes).
pub(crate) fn machine_action(
    dir: &Path,
    spec: &Spec,
    machine: &str,
    action: &str,
) -> Result<String, RunError> {
    if !spec.machines.contains_key(machine) {
        return Err(RunError::Spec(format!("no machine named `{machine}`")));
    }
    let out = if let rt @ (Runtime::Vagrant | Runtime::Hybrid) = runtime(dir) {
        crate::vms::action(dir, spec, rt, machine, action)?
    } else {
        compose_action(dir, machine, action)?
    };
    // A machine comes back with its ports up: its suspended links are cut again.
    if action != "stop" {
        super::links::reapply_links(dir, spec, machine)?;
    }
    Ok(out)
}

/// Starts, stops or restarts one machine's Compose services (itself and its sidecar).
fn compose_action(dir: &Path, machine: &str, action: &str) -> Result<String, RunError> {
    let services = with_sidecar(dir, machine)?;
    let names: Vec<&str> = services.iter().map(String::as_str).collect();
    match action {
        "start" => run(
            compose(dir, &[&["up", "-d", "--no-deps"][..], &names].concat()),
            "Starting the machine",
        ),
        "stop" => run(
            compose(dir, &[&["stop"][..], &names].concat()),
            "Stopping the machine",
        ),
        "restart" => {
            run(
                compose(dir, &[&["stop"][..], &names].concat()),
                "Stopping the machine",
            )?;
            run(
                compose(dir, &[&["up", "-d", "--no-deps"][..], &names].concat()),
                "Starting the machine",
            )
        }
        other => Err(RunError::Spec(format!(
            "unknown action `{other}`: start, stop or restart"
        ))),
    }
}

/// The machine and its network sidecar, as Compose services.
pub(super) fn with_sidecar(dir: &Path, machine: &str) -> Result<Vec<String>, RunError> {
    let services = run(
        compose(dir, &["config", "--services"]),
        "Reading the lab's services",
    )?;
    let sidecar = format!("{machine}-routes");
    let mut out = vec![machine.to_owned()];
    if services.lines().any(|l| l.trim() == sidecar) {
        out.push(sidecar);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{down, prepare, up};

    /// Needs Docker (run with `--ignored`): a machine's console and starting/stopping it alone.
    #[test]
    #[ignore = "needs Docker; run with --ignored"]
    fn one_machine_stops_starts_and_has_a_console() {
        let dir = std::env::temp_dir().join(format!("isothrum-machine-{}", std::process::id()));
        let yaml = "version: 1\nname: onem\nnetworks:\n  a: { cidr: 10.61.1.0/24 }\n  b: { cidr: 10.61.2.0/24 }\nreach: [{ from: a, to: b }]\nmachines:\n  pc: { networks: { a: 10 }, app: { image: \"alpine:3.20\" }, command: [sleep, infinity] }\n  srv: { networks: { b: 10 }, app: { image: \"alpine:3.20\" }, command: [sleep, infinity] }\n";
        let spec = prepare(&dir, yaml, &[]).unwrap();
        up(&dir).unwrap();
        let running = |m: &str| {
            !run(compose(&dir, &["ps", "-q", m]), "ps")
                .unwrap()
                .trim()
                .is_empty()
        };
        let cmd = console_command(&dir, &spec, "pc").unwrap();
        assert_eq!(cmd.argv[..3], ["docker", "exec", "-it"]);
        assert_eq!(cmd.argv.last().map(String::as_str), Some(SHELL));
        machine_action(&dir, &spec, "pc", "stop").unwrap();
        assert!(
            !running("pc") && !running("pc-routes"),
            "the machine and its sidecar stop"
        );
        assert!(running("srv"), "the others keep running");
        machine_action(&dir, &spec, "pc", "start").unwrap();
        assert!(running("pc") && running("pc-routes"));
        // Its routes came back with the sidecar.
        let routes = run(
            compose(&dir, &["exec", "-T", "pc", "ip", "route"]),
            "routes",
        )
        .unwrap();
        assert!(routes.contains("10.61.2.0/24 via"), "{routes}");
        assert!(machine_action(&dir, &spec, "nobody", "stop").is_err());
        down(&dir).unwrap();
        drop(std::fs::remove_dir_all(&dir));
    }
}
