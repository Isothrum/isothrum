//! Starter set-up scripts, so a downloaded project with provisioning steps runs as is.
//!
//! The editor lists a machine's provisioning steps (`provision/db.sh`), but nobody writes the
//! files. Each shell step gets a script that installs what the machine's ports call for and makes
//! it listen on the network (PostgreSQL, MariaDB and Redis listen on localhost by default, so the
//! project's checks couldn't reach them). The steps are portable, so a script only installs and
//! configures: Isoloom runs it when it builds the machine's container image, or on the VM, and the
//! services are started only where an init system runs them. A `.ps1` step gets a commented
//! template, an Ansible playbook a minimal one (on Windows, one the controller runs over WinRM).
//! They are starting points to edit, not a configuration management system.

mod packages;

use isoloom_spec::Spec;
use isoloom_spec::syntax::{Machine, Octets, OsRef, Step};
use isoloom_spec::validate::Cidr;

use crate::ProjectFile;
use crate::controller::ControllerPlaybook;
use packages::{Family, PACKAGES, install, start, wanted};

/// The folder Isoloom writes its outputs to, at the project's root: `isoloom_render::OUTPUT_DIR`
/// (re-exported by `isoloom-targets`, which this crate doesn't depend on; a test holds the two
/// equal).
const OUTPUT_DIR: &str = ".isoloom";

/// Adds to `files` one starter per provisioning step, the machines' own then the controller's:
/// at paths inside the project, which no file has yet (Isoloom's own aside).
pub(crate) fn add_starters(files: &mut Vec<ProjectFile>, spec: &Spec, controller: &[ControllerPlaybook]) {
    for (name, m) in &spec.machines {
        let (Some(os), Some(app)) = (m.os, m.app()) else {
            continue;
        };
        for step in &app.steps {
            let (path, contents) = match step {
                Step::Run { script, .. } if is_ps1(script.as_str()) => (script.as_str(), powershell(spec, name, m, os)),
                Step::Run { script, .. } => (script.as_str(), shell(spec, name, m, os)),
                Step::Playbook(file) => (file.as_str(), playbook(name, PlaybookOn::Machine)),
                _ => continue,
            };
            add(files, path, contents);
        }
    }
    for p in controller {
        add(files, &p.path, playbook(&p.machine, PlaybookOn::Controller));
    }
}

/// Adds the starter at `path`, unless the path leaves the project, is Isoloom's (a name it reads
/// the spec from, [`isoloom_spec::SPEC_FILES`], or its outputs' folder), or already has a file.
fn add(files: &mut Vec<ProjectFile>, path: &str, contents: String) {
    let in_outputs = path.strip_prefix(OUTPUT_DIR).is_some_and(|rest| rest.starts_with('/'));
    let isoloom_s = isoloom_spec::SPEC_FILES.contains(&path) || in_outputs;
    if !path.starts_with('/') && !path.contains("..") && !isoloom_s && !files.iter().any(|s| s.path == path) {
        files.push(ProjectFile {
            path: path.to_owned(),
            contents,
        });
    }
}

fn is_ps1(path: &str) -> bool {
    std::path::Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("ps1"))
}

/// The machine's ports, by number, each once (a TCP and a UDP port may share one).
fn ports(m: &Machine) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    for (port, _) in transports(m) {
        if !out.contains(&port) {
            out.push(port);
        }
    }
    out
}

/// The machine's ports, by number and transport (`TCP`, `UDP`), each once.
fn transports(m: &Machine) -> Vec<(u16, &'static str)> {
    let mut out: Vec<(u16, &'static str)> = Vec::new();
    for p in m.ports.values() {
        let Some(port) = p.port else { continue };
        let found = (port.get(), if p.protocol.is_tcp() { "TCP" } else { "UDP" });
        if !out.contains(&found) {
            out.push(found);
        }
    }
    out
}

/// The machine's addresses, as `10.10.1.12 (lan)`.
fn addresses(spec: &Spec, m: &Machine) -> Vec<String> {
    m.networks
        .iter()
        .filter_map(|(net, octets)| {
            let octet = match octets {
                Octets::One(o) => *o,
                Octets::Several(list) => *list.first()?,
            };
            let address = Cidr::parse(cidr_of(spec, net)?)?.host(octet)?;
            Some(format!("{address} ({net})"))
        })
        .collect()
}

/// A network's block: a LAN's own, or a VLAN's under its LAN (`sw1.vlan10`).
fn cidr_of<'s>(spec: &'s Spec, net: &str) -> Option<&'s str> {
    if let Some(n) = spec.networks.get(net) {
        return Some(&n.cidr);
    }
    let (lan, id) = crate::vlans::parse_ref(net)?;
    spec.networks.get(lan)?.vlans.get(&id).map(|v| v.cidr.as_str())
}

/// A Linux step's script: what it is, then the install, the configuration and the start.
fn shell(spec: &Spec, name: &str, m: &Machine, os: OsRef) -> String {
    let family = Family::of(os.os().family);
    let ports = ports(m);
    let wanted = wanted(&ports, family);
    let mut lines = header(spec, name, m, os);
    lines.insert(0, "#!/bin/sh".to_owned());
    if !ports.is_empty() {
        let list: Vec<String> = ports.iter().map(ToString::to_string).collect();
        lines.push(format!("# Services the project expects to answer: {}", list.join(", ")));
    }
    lines.push("set -eu".to_owned());
    lines.push(String::new());
    if wanted.is_empty() {
        lines.push("# Nothing to install for this machine's ports yet: add your own steps here.".to_owned());
    } else {
        lines.push(install(&wanted, family));
        for p in wanted.iter().filter(|p| !p.configure.is_empty()) {
            lines.push(format!(
                "\n# Port {}: listen on the project's networks, not only on localhost.\n{}",
                p.port, p.configure
            ));
        }
        lines.push(String::new());
        lines.push(start(&wanted, family));
    }
    let unknown: Vec<String> = ports
        .iter()
        .filter(|port| !PACKAGES.iter().any(|p| p.port == **port))
        .map(ToString::to_string)
        .collect();
    if !unknown.is_empty() {
        lines.push(format!("\n# To do: install what answers on port(s) {}.", unknown.join(", ")));
    }
    text(&lines)
}

/// Lines as a file's text, each ending with a line break.
fn text(lines: &[String]) -> String {
    let mut text = lines.iter().map(|l| l.trim_end_matches('\n')).collect::<Vec<_>>().join("\n");
    text.push('\n');
    text
}

/// A starter's first lines: what it is and the machine's addresses.
fn header(spec: &Spec, name: &str, m: &Machine, os: OsRef) -> Vec<String> {
    let mut lines = vec![format!(
        "# Starter set-up for `{name}` ({os}), written by Isothrum: edit it to build your project."
    )];
    lines.extend(addresses(spec, m).iter().map(|a| format!("# Address: {a}")));
    lines
}

/// The TCP ports IIS answers on, once installed (443 once a certificate is bound).
const WEB_PORTS: [u16; 2] = [80, 443];

/// A Windows step's script: IIS for its web ports, a firewall rule for each other port, on its
/// own transport.
fn powershell(spec: &Spec, name: &str, m: &Machine, os: OsRef) -> String {
    let mut lines = header(spec, name, m, os);
    lines.push("$ErrorActionPreference = 'Stop'".to_owned());
    lines.push(String::new());
    let ports = transports(m);
    let is_web = |(port, transport): &(u16, &str)| *transport == "TCP" && WEB_PORTS.contains(port);
    let web: Vec<String> = ports.iter().filter(|p| is_web(p)).map(|(port, _)| port.to_string()).collect();
    if !web.is_empty() {
        lines.push(format!("# A web server (IIS) for port {}.", web.join(" and ")));
        lines.push("Install-WindowsFeature -Name Web-Server -IncludeManagementTools".to_owned());
    }
    if web.iter().any(|p| p == "443") {
        lines.push("# Port 443: IIS answers HTTPS once a certificate is bound to its site.".to_owned());
    }
    for (p, transport) in ports.iter().filter(|p| !is_web(p)) {
        let lower = transport.to_lowercase();
        lines.push(format!("# To do: install what answers on port {p}/{lower}, then open it:"));
        lines.push(format!(
            "New-NetFirewallRule -DisplayName 'project-{p}-{lower}' -Direction Inbound -Protocol {transport} -LocalPort {p} -Action Allow"
        ));
    }
    if ports.is_empty() {
        lines.push("# Nothing to install for this machine's ports yet: add your own steps here.".to_owned());
    }
    text(&lines)
}

/// Where an Ansible step's playbook runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaybookOn {
    /// On its machine, as root (`hosts: all`, `become`).
    Machine,
    /// From Isoloom's controller, over WinRM (`hosts: <machine>`; WinRM has no `become`).
    Controller,
}

/// The starter playbook that sets up `machine`, run `on` its machine or from the controller.
fn playbook(machine: &str, on: PlaybookOn) -> String {
    let (from, hosts, become_, ping) = match on {
        PlaybookOn::Machine => ("", "all", "  become: true\n", "ansible.builtin.ping"),
        PlaybookOn::Controller => (", run from Isoloom's controller over WinRM", machine, "", "ansible.windows.win_ping"),
    };
    format!(
        "# Starter playbook for `{machine}`{from}, written by Isothrum: edit it to build your project.\n- hosts: {hosts}\n{become_}  tasks:\n    - name: The machine answers\n      {ping}:\n"
    )
}

#[cfg(test)]
mod tests;
