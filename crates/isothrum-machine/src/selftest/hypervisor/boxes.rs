//! The test box: a small public box that exists for the hypervisor and this computer's CPU, found
//! among those Vagrant holds ([`crate::boxes`]), else the first of them, which the caller adds.

use isothrum_runner::{Arch, Provider};

use crate::Context;
use crate::boxes::held;

/// Small public boxes that exist for `p` and `arch`, smallest first; none where no public box
/// does. Hyper-V on Arm runs only arm64 generation 2 VMs, and no public Vagrant box is built so
/// (bento's and generic's Hyper-V boxes are amd64 and generation 1).
#[must_use]
pub fn candidate_boxes(p: Provider, arch: Arch) -> &'static [&'static str] {
    match (p, arch) {
        (Provider::Hyperv, Arch::Arm64) => &[],
        (Provider::Virtualbox | Provider::VmwareDesktop | Provider::Parallels, Arch::Amd64) => &["generic/alpine319", "bento/debian-12"],
        (Provider::Virtualbox | Provider::VmwareDesktop | Provider::Parallels, Arch::Arm64) => &["bento/debian-12"],
        (Provider::Utm, _) => &["utm/bookworm"],
        _ => &["generic/alpine319"],
    }
}

/// The test box for `p` on this computer: a candidate Vagrant holds already, else the first
/// candidate; whether it is held. None: no box exists for `p` on this computer's CPU.
pub async fn box_for(cx: &Context, p: Provider) -> Option<(&'static str, bool)> {
    let arch = cx.host.arch;
    let candidates = candidate_boxes(p, arch);
    let first = *candidates.first()?;
    let list = cx.read_ok("vagrant", &["box", "list"]).await.unwrap_or_default();
    Some(match candidates.iter().find(|c| held(&list, c, p, arch).is_some()) {
        Some(c) => (c, true),
        None => (first, false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_provider_and_cpu_has_a_box_but_hyperv_on_arm() {
        for p in [
            Provider::Virtualbox,
            Provider::Libvirt,
            Provider::Qemu,
            Provider::Utm,
            Provider::Parallels,
            Provider::Hyperv,
        ] {
            for arch in [Arch::Amd64, Arch::Arm64] {
                let none = (p, arch) == (Provider::Hyperv, Arch::Arm64);
                assert_eq!(candidate_boxes(p, arch).is_empty(), none, "{p:?} on {arch:?}");
            }
        }
        assert_eq!(candidate_boxes(Provider::Virtualbox, Arch::Arm64), ["bento/debian-12"]);
    }
}
