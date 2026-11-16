//! Which of a module's resources are machines, and what each is called: a VM of one of the
//! providers' VM types ([`VM_TYPES`]), named by its address without its type
//! ([`machine_name`]): `esxi_guest.web` is `web`, `aws_instance.pool[0]` is `pool[0]`, and
//! `module.lab.proxmox_virtual_environment_vm.vm["a"]` is `module.lab.vm["a"]`. A machine's
//! address is the one its provider writes in the state, else the one the module's `machines`
//! output gives it ([`with_named_addresses`]).

use crate::cli::terraform as tf;
use crate::formats::SeenMachine;
use crate::state::RunState;
use crate::status::MachineState;

/// The providers' resource types that are a virtual machine: the clouds', the hypervisors' and
/// the servers'. Anything else a module makes (networks, firewalls, keys, waits) is one of its
/// resources, never a machine.
const VM_TYPES: [&str; 18] = [
    "aws_instance",
    "azurerm_linux_virtual_machine",
    "azurerm_virtual_machine",
    "azurerm_windows_virtual_machine",
    "digitalocean_droplet",
    "esxi_guest",
    "google_compute_instance",
    "hcloud_server",
    "libvirt_domain",
    "linode_instance",
    "oci_core_instance",
    "openstack_compute_instance_v2",
    "proxmox_virtual_environment_vm",
    "proxmox_vm_qemu",
    "scaleway_instance_server",
    "vsphere_virtual_machine",
    "vultr_instance",
    "xenorchestra_vm",
];

/// Whether a resource of type `kind` is a machine.
pub(super) fn is_vm(kind: &str) -> bool {
    VM_TYPES.contains(&kind)
}

/// The machine name of the instance at `address`, of type `kind`: the address without its
/// `<kind>.` segment, its modules and its key kept; the address as it is when it isn't a
/// resource of `kind` (an address Terraform wouldn't write).
pub(super) fn machine_name(address: &str, kind: &str) -> String {
    let segments = segments(address);
    // `module.<name>[<key>]` pairs, then `<kind>.<name>[<key>]`.
    let mut at = 0;
    while segments.get(at).map(String::as_str) == Some("module") {
        at += 2;
    }
    if segments.get(at).map(String::as_str) != Some(kind) || at + 1 >= segments.len() {
        return address.to_owned();
    }
    let mut kept = segments;
    kept.remove(at);
    kept.join(".")
}

/// `address` split at each `.` outside a key (`[...]`, whose strings may hold dots).
fn segments(address: &str) -> Vec<String> {
    let (mut out, mut current) = (Vec::new(), String::new());
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    for c in address.chars() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' if depth > 0 => quoted = !quoted,
            '[' if !quoted => depth += 1,
            ']' if !quoted => depth = depth.saturating_sub(1),
            '.' if depth == 0 && !quoted => {
                out.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    out.push(current);
    out
}

/// The machines among `managed`: each VM by its machine name ([`machine_name`]), with
/// its state (a tainted one is unhealthy: a create or a provisioner of it failed).
pub(super) fn machines_of(managed: &[tf::Resource]) -> Vec<SeenMachine> {
    managed
        .iter()
        .filter(|r| is_vm(&r.kind))
        .map(|r| SeenMachine {
            name: machine_name(&r.address, &r.kind),
            state: if r.tainted { MachineState::Unhealthy } else { MachineState::Running },
            published: Vec::new(),
            kind: Some(r.kind.clone()),
            address: r.vm.address.clone(),
            windows: r.vm.windows,
        })
        .collect()
}

/// The output naming each machine's address: `{ web = "10.60.0.10" }`.
const MACHINES_OUTPUT: &str = "machines";

/// Gives each of `machines` the state gave no address the one the module's `machines` output
/// names (`outputs`, as `output -json` read them), when it is text. A server's VMs on the
/// module's own networks have none in the state: their provider reads addresses from a guest
/// agent they don't run (Proxmox VE's), and the module knows them, as it gave them.
pub(super) fn with_named_addresses(machines: &mut [SeenMachine], outputs: &std::collections::BTreeMap<String, tf::Output>) {
    let Some(named) = outputs.get(MACHINES_OUTPUT).and_then(|o| o.value.as_object()) else {
        return;
    };
    for machine in machines.iter_mut().filter(|m| m.address.is_none()) {
        machine.address = named
            .get(&machine.name)
            .and_then(serde_json::Value::as_str)
            .filter(|a| !a.is_empty())
            .map(str::to_owned);
    }
}

/// Where a run stands, from its state as `show` read it: absent when it holds nothing; ready
/// when every managed instance is sound, or when it holds data sources only (a module that reads
/// and gives back, making nothing, is up once applied); failed with a tainted instance.
pub(super) fn run_state(held: &tf::Shown) -> RunState {
    let managed = &held.managed;
    if managed.is_empty() && held.data.is_empty() {
        RunState::Absent
    } else if managed.is_empty() {
        RunState::Ready
    } else if managed.iter().any(|r| r.tainted) {
        RunState::Failed
    } else {
        RunState::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_is_named_by_its_address_without_its_type() {
        for (address, kind, name) in [
            ("esxi_guest.web", "esxi_guest", "web"),
            ("aws_instance.pool[0]", "aws_instance", "pool[0]"),
            (
                "module.lab.proxmox_virtual_environment_vm.vm[\"a\"]",
                "proxmox_virtual_environment_vm",
                "module.lab.vm[\"a\"]",
            ),
            // A module named like the type.
            ("module.esxi_guest.esxi_guest.web", "esxi_guest", "module.esxi_guest.web"),
            // A key holding the type.
            ("module.m[\"esxi_guest.x\"].esxi_guest.web", "esxi_guest", "module.m[\"esxi_guest.x\"].web"),
            // Nested modules, one named like the type.
            ("module.esxi_guest.module.lab.esxi_guest.web", "esxi_guest", "module.esxi_guest.module.lab.web"),
            // Another type.
            ("aws_instance.vm", "esxi_guest", "aws_instance.vm"),
        ] {
            assert_eq!(machine_name(address, kind), name, "{address}");
        }
    }

    fn seen(name: &str, address: Option<&str>) -> SeenMachine {
        SeenMachine {
            name: name.to_owned(),
            state: MachineState::Running,
            published: Vec::new(),
            kind: Some("proxmox_virtual_environment_vm".to_owned()),
            address: address.map(str::to_owned),
            windows: false,
        }
    }

    #[test]
    fn a_machine_the_state_gives_no_address_takes_the_one_the_machines_output_names() {
        let outputs: std::collections::BTreeMap<String, tf::Output> = serde_json::from_value(serde_json::json!({
            "machines": { "value": { "web": "10.60.0.10", "cache": "10.60.0.20", "db": "", "odd": 7 } }
        }))
        .expect("outputs");
        let mut machines = vec![
            seen("web", None),
            seen("router", Some("192.0.2.10")),
            seen("cache", Some("198.51.100.4")),
            seen("db", None),
            seen("odd", None),
            seen("nameless", None),
        ];
        with_named_addresses(&mut machines, &outputs);
        let addresses: Vec<(&str, Option<&str>)> = machines.iter().map(|m| (m.name.as_str(), m.address.as_deref())).collect();
        assert_eq!(
            addresses,
            [
                ("web", Some("10.60.0.10")),
                ("router", Some("192.0.2.10")),
                ("cache", Some("198.51.100.4")),
                ("db", None),
                ("odd", None),
                ("nameless", None),
            ],
            "the state's address wins; an empty or non-text one is none"
        );
        let mut untouched = vec![seen("web", None)];
        with_named_addresses(&mut untouched, &std::collections::BTreeMap::new());
        assert_eq!(untouched[0].address, None, "no output: nothing given");
    }

    #[test]
    fn only_vm_types_are_machines() {
        for (kind, vm) in [
            ("esxi_guest", true),
            ("aws_instance", true),
            ("proxmox_virtual_environment_vm", true),
            ("terraform_data", false),
            ("aws_security_group", false),
            ("esxi_vswitch", false),
        ] {
            assert_eq!(is_vm(kind), vm, "{kind}");
        }
        assert!(VM_TYPES.is_sorted(), "kept sorted, for its search");
    }
}
