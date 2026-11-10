//! Restarting, at an update, a running VM whose boot-time settings changed. Vagrant applies a VM's
//! synced folders, networks, forwarded ports, size and provider settings only when it boots it;
//! `up`'s steps on a running VM would leave them as they were (a host folder added: the VM never
//! mounts it). The manifest's `native.boot_digest` of each VM is compared with the one the run
//! recorded at its last `up` or update: a running VM whose two digests are both known and differ
//! is restarted (`vagrant reload`: only Vagrant's VMs carry a boot digest) before `up`'s steps
//! run again, which then provision it and start what runs in it (a `docker-vm`'s containers). A
//! VM with no recorded digest (a run recorded before the runner kept them) or none in the
//! manifest is left running: an unknown digest never reloads. Either way the update records the
//! manifest's digests ([`Running::runs_from_this`]).

use std::collections::BTreeMap;

use isoloom_manifest::{Machine, Manifest, Native};

use super::Running;
use crate::Result;
use crate::driver::Observed;
use crate::executor::Cx;
use crate::status::MachineState;

impl Running<'_> {
    /// Restarts each VM that runs (as `observed`) and whose boot digest in the manifest differs
    /// from the one `recorded`, both known, each as its own step (`reload-<machine>`); the first
    /// failure stops the rest. Only a Vagrant run's VMs carry a boot digest: another driver's run
    /// has nothing to restart.
    pub(super) async fn reload_changed(&self, cx: &Cx<'_>, recorded: &BTreeMap<String, String>, observed: &Observed) -> Result<()> {
        for vm in changed_at_boot(self.manifest(), recorded, observed) {
            let id = format!("reload-{}", vm.name);
            let note = format!("Restarting {} to apply its new settings…", vm.name);
            cx.events.stepped(&id, Some(&note), self.driver.restart(cx, vm, &id)).await?;
        }
        Ok(())
    }
}

/// Each VM's boot digest in `manifest` (its `native.boot_digest`), by machine name.
pub(super) fn digests(manifest: &Manifest) -> BTreeMap<String, String> {
    manifest
        .machines
        .iter()
        .filter_map(|m| match &m.native {
            Some(Native::Vagrant { boot_digest: Some(digest), .. }) => Some((m.name.to_string(), digest.clone())),
            _ => None,
        })
        .collect()
}

/// The machines of `manifest` that run (as `observed`) and whose boot digest differs from the
/// one `recorded`, both known.
fn changed_at_boot<'m>(manifest: &'m Manifest, recorded: &BTreeMap<String, String>, observed: &Observed) -> Vec<&'m Machine> {
    let now = digests(manifest);
    let running = |name: &str| observed.machines.iter().any(|m| m.name == name && m.state == MachineState::Running);
    manifest
        .machines
        .iter()
        .filter(|m| {
            let name = m.name.as_str();
            matches!((recorded.get(name), now.get(name)), (Some(was), Some(is)) if was != is) && running(name)
        })
        .collect()
}
