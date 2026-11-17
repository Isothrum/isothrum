//! The test box: a small public box that exists for the hypervisor and this computer's CPU, found
//! among those Vagrant holds, else added (`vagrant box add`), its download handed back for a
//! caller that keeps a record of its downloads.

use std::time::Duration;

use isothrum_runner::{Arch, DownloadToAdopt, ProcessSpec, Provider};

use crate::Context;

/// A box is hundreds of MB: past this the download is stalled, not just on a slow link.
const ADD_LIMIT: Duration = Duration::from_secs(1800);

/// Small public boxes that exist for `p` and `arch`, smallest first.
#[must_use]
pub fn candidate_boxes(p: Provider, arch: Arch) -> &'static [&'static str] {
    match (p, arch) {
        (Provider::Virtualbox | Provider::VmwareDesktop | Provider::Parallels, Arch::Amd64) => &["generic/alpine319", "bento/debian-12"],
        (Provider::Virtualbox | Provider::VmwareDesktop | Provider::Parallels, Arch::Arm64) => &["bento/debian-12"],
        (Provider::Utm, _) => &["utm/bookworm"],
        _ => &["generic/alpine319"],
    }
}

/// The Vagrant box provider a box is added and listed under for `p`. QEMU is the exception: the
/// vagrant-qemu plugin boots a libvirt-format box's qcow2 directly, so its box is stored as
/// `libvirt`, never `qemu`. `generic/alpine319`'s own "qemu" box is a libvirt image mislabeled
/// (`box add --provider qemu` fetches it and then rejects it), while its `libvirt` box bakes in
/// the vagrant user and key, so the test VM gets SSH without cloud-init (which vagrant-qemu does
/// not seed).
#[must_use]
pub fn box_provider(p: Provider) -> &'static str {
    match p {
        Provider::Qemu => "libvirt",
        _ => p.id(),
    }
}

/// One box of `vagrant box list`: `name (provider, version[, (arch)])`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listed {
    name: String,
    provider: String,
    version: String,
    arch: Option<String>,
}

/// The boxes `vagrant box list` printed.
fn listed(out: &str) -> Vec<Listed> {
    out.lines()
        .filter_map(|line| {
            let (name, rest) = line.trim().split_once(char::is_whitespace)?;
            let inner = rest.trim().strip_prefix('(')?;
            let mut fields = inner.split(',').map(|f| f.trim().trim_end_matches(')'));
            let provider = fields.next().filter(|p| !p.is_empty())?;
            let version = fields.next().filter(|v| !v.is_empty())?;
            let arch = fields.next().map(|a| a.trim_start_matches('(')).filter(|a| !a.is_empty()).map(str::to_owned);
            Some(Listed {
                name: name.to_owned(),
                provider: provider.to_owned(),
                version: version.to_owned(),
                arch,
            })
        })
        .collect()
}

/// The CPU's name in a box listing.
fn arch_name(arch: Arch) -> &'static str {
    match arch {
        Arch::Amd64 => "amd64",
        Arch::Arm64 => "arm64",
    }
}

/// The version of box `name` for `p` that `list` (`vagrant box list`) holds for `arch`; a box
/// with no architecture in the listing counts for any.
fn held(list: &str, name: &str, p: Provider, arch: Arch) -> Option<String> {
    listed(list).into_iter().find_map(|b| {
        let other_cpu = matches!(b.arch.as_deref(), Some(a @ ("amd64" | "arm64")) if a != arch_name(arch));
        (b.name == name && b.provider == box_provider(p) && !other_cpu).then_some(b.version)
    })
}

/// The test box for `p` on this computer: a candidate Vagrant holds already, else the first
/// candidate; whether it is held.
pub async fn box_for(cx: &Context, p: Provider) -> (&'static str, bool) {
    let arch = cx.host.arch;
    let candidates = candidate_boxes(p, arch);
    let list = cx.read_ok("vagrant", &["box", "list"]).await.unwrap_or_default();
    match candidates.iter().find(|c| held(&list, c, p, arch).is_some()) {
        Some(c) => (c, true),
        None => (candidates.first().copied().unwrap_or_default(), false),
    }
}

/// Adds box `name` for `p` (`vagrant box add --force`), each line Vagrant prints said to `line`;
/// past half an hour it is given up on. The box as added, for a caller that keeps a record of its
/// downloads (none when its version can't be read back).
///
/// # Errors
///
/// What Vagrant said, or that it stalled.
pub async fn add_box(cx: &Context, p: Provider, name: &str, line: &mut (dyn FnMut(&str) + Send)) -> Result<Option<DownloadToAdopt>, String> {
    let spec = ProcessSpec::new("vagrant", ["box", "add", name, "--provider", box_provider(p), "--force"]);
    let mut on_line = |_: isothrum_runner::Stream, l: &str| line(l);
    let added = tokio::time::timeout(ADD_LIMIT, cx.runner.run(&spec, &mut on_line)).await;
    let exit = added.map_err(|_| format!("vagrant box add stalled: nothing for {} minutes", ADD_LIMIT.as_secs() / 60))?;
    let exit = exit.map_err(|e| e.to_string())?;
    if !exit.success() {
        return Err(format!("vagrant box add {name} failed ({exit})"));
    }
    let list = cx.read_ok("vagrant", &["box", "list"]).await.unwrap_or_default();
    Ok(held(&list, name, p, cx.host.arch).map(|version| super::adopted(name, p, version)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_is_held_for_its_provider_and_this_cpu() {
        let list = "generic/debian12   (virtualbox, 202510.26.0, (amd64))\r\ngeneric/alpine319\t(libvirt, 4.3.12)\nThere are no installed boxes!\nodd (\n";
        assert_eq!(
            held(list, "generic/debian12", Provider::Virtualbox, Arch::Amd64).as_deref(),
            Some("202510.26.0")
        );
        assert_eq!(held(list, "generic/debian12", Provider::Virtualbox, Arch::Arm64), None, "another CPU's");
        assert_eq!(
            held(list, "generic/alpine319", Provider::Qemu, Arch::Arm64).as_deref(),
            Some("4.3.12"),
            "QEMU boots libvirt boxes, any CPU listed"
        );
        assert_eq!(held(list, "generic/alpine319", Provider::Virtualbox, Arch::Amd64), None);
    }

    #[test]
    fn each_provider_and_cpu_has_a_box() {
        for p in [
            Provider::Virtualbox,
            Provider::Libvirt,
            Provider::Qemu,
            Provider::Utm,
            Provider::Parallels,
            Provider::Hyperv,
        ] {
            for arch in [Arch::Amd64, Arch::Arm64] {
                assert_ne!(candidate_boxes(p, arch).len(), 0);
            }
        }
        assert_eq!(candidate_boxes(Provider::Virtualbox, Arch::Arm64), ["bento/debian-12"]);
    }
}
