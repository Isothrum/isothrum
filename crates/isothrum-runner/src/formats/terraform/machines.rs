//! Which of a module's resources are machines, and what each is called: a VM of one of the
//! providers' VM types ([`VM_TYPES`]), named by its address without its type
//! ([`machine_name`]): `esxi_guest.web` is `web`, `aws_instance.pool[0]` is `pool[0]`, and
//! `module.lab.proxmox_virtual_environment_vm.vm["a"]` is `module.lab.vm["a"]`.

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
        assert_eq!(machine_name("esxi_guest.web", "esxi_guest"), "web");
        assert_eq!(machine_name("aws_instance.pool[0]", "aws_instance"), "pool[0]");
        assert_eq!(
            machine_name("module.lab.proxmox_virtual_environment_vm.vm[\"a\"]", "proxmox_virtual_environment_vm"),
            "module.lab.vm[\"a\"]"
        );
        assert_eq!(
            machine_name("module.esxi_guest.esxi_guest.web", "esxi_guest"),
            "module.esxi_guest.web",
            "a module named like the type"
        );
        assert_eq!(
            machine_name("module.m[\"esxi_guest.x\"].esxi_guest.web", "esxi_guest"),
            "module.m[\"esxi_guest.x\"].web",
            "a key holding the type"
        );
        assert_eq!(
            machine_name("module.esxi_guest.module.lab.esxi_guest.web", "esxi_guest"),
            "module.esxi_guest.module.lab.web",
            "nested modules, one named like the type"
        );
        assert_eq!(machine_name("aws_instance.vm", "esxi_guest"), "aws_instance.vm", "another type");
    }

    #[test]
    fn only_vm_types_are_machines() {
        assert!(is_vm("esxi_guest") && is_vm("aws_instance") && is_vm("proxmox_virtual_environment_vm"));
        assert!(!is_vm("terraform_data") && !is_vm("aws_security_group") && !is_vm("esxi_vswitch"));
        assert!(VM_TYPES.windows(2).all(|w| w[0] < w[1]), "kept sorted");
    }
}
