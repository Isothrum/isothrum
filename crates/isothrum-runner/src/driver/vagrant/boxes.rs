//! The boxes a Vagrant run's VMs boot, downloaded in a phase of their own before the `vagrant up`
//! that creates the VMs (a step `vagrant up` without `--no-provision`), as the images of a Compose
//! start are ([`crate::driver::compose`]'s pull): each machine's `native.boxes` for the run's
//! provider and this computer's architecture, the access machine's left to its hot-add (which may
//! be given another box). A box already there, its provider, version (when exact) and build the
//! same, is skipped (`vagrant box list --machine-readable`); each other one is added
//! (`vagrant box add <box> --provider <format> [--box-version <v>] [--architecture <a>]`, the
//! format the provider boots: its own, libvirt's for QEMU) in the step
//! `boxes-<step>`, its lines that step's `log` events.
//!
//! A download still moving has no time limit: it is stopped once it shows no progress for
//! [`DOWNLOAD_STALL`], as `download_stalled`; one that fails (a box or version the catalog doesn't
//! have, the network) is `download_failed`, with Vagrant's reason. Vagrant saying the box is there
//! already (another process added it meanwhile) is no failure. The `up` then runs under its own
//! limit, with nothing left to download.

use std::collections::BTreeSet;

use isoloom_manifest::{Arch, Native, Provider, Run, Step, VagrantBox};

use super::{READ_LIMIT, Vagrant};
use crate::Result;
use crate::error::Error;
use crate::executor::{Cx, DOWNLOAD_STALL, Echo, call, download, spec_for};
use crate::tool_output::vagrant_fields;

/// What the download's step says when it starts.
const NOTE: &str = "Downloading the VMs' boxes…";

/// What Vagrant says of a box it has already.
const THERE_ALREADY: &str = "already exists";

/// The id of the download that comes before step `id`.
fn id_before(id: &str) -> String {
    format!("boxes-{id}")
}

/// One box as `vagrant box list` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listed {
    name: String,
    provider: String,
    version: String,
    /// Its build, when the box says (an older box says none, or `unknown`).
    arch: Option<String>,
}

/// The boxes `vagrant box list --machine-readable` lists: a `box-name` line starts each, its
/// `box-provider`, `box-version` and `box-architecture` lines follow.
fn listed(out: &str) -> Vec<Listed> {
    let mut boxes: Vec<Listed> = Vec::new();
    for (_, kind, data) in out.lines().filter_map(vagrant_fields) {
        let data = data.trim();
        match (kind, boxes.last_mut()) {
            ("box-name", _) => boxes.push(Listed {
                name: data.to_owned(),
                provider: String::new(),
                version: String::new(),
                arch: None,
            }),
            ("box-provider", Some(b)) => data.clone_into(&mut b.provider),
            ("box-version", Some(b)) => data.clone_into(&mut b.version),
            ("box-architecture", Some(b)) => b.arch = Some(data.to_owned()).filter(|a| !a.is_empty() && a != "unknown"),
            _ => {}
        }
    }
    boxes
}

/// Whether `wanted` is among `there`, for a computer of architecture `host`: its name and
/// provider, its version when it names one exactly (any otherwise), its build (pinned, else the
/// computer's; a box that says none matches any).
fn is_there(wanted: &VagrantBox, host: Arch, there: &[Listed]) -> bool {
    let build = arch_id(wanted.architecture.unwrap_or(host));
    there.iter().any(|b| {
        b.name == wanted.name.as_str()
            && b.provider == wire(wanted)
            && wanted.version.as_ref().is_none_or(|v| !v.is_exact() || b.version == v.as_str())
            && b.arch.as_deref().is_none_or(|a| a == build)
    })
}

/// An architecture as Vagrant names it.
fn arch_id(a: Arch) -> &'static str {
    match a {
        Arch::Amd64 => "amd64",
        Arch::Arm64 => "arm64",
    }
}

/// The box format Vagrant looks a box up by when its provider boots it: the provider's own name,
/// but vagrant-qemu's, which boots libvirt-format boxes (its `box_format`). Added as the qemu
/// format, a box was downloaded twice: once by the download, then again by `vagrant up`, which
/// found no libvirt build of it.
fn wire(b: &VagrantBox) -> &'static str {
    match b.provider {
        Provider::Qemu => Provider::Libvirt.id(),
        other => other.id(),
    }
}

/// The boxes the run's machines boot on its provider and this computer, each once, in the
/// manifest's order; the access machine's left out.
fn wanted(cx: &Cx<'_>) -> Vec<VagrantBox> {
    let Some(provider) = cx.scope.provider else { return Vec::new() };
    let host = cx.run.runner.config.arch;
    let mut seen = BTreeSet::new();
    cx.run
        .manifest()
        .machines
        .iter()
        .filter(|m| !m.on_demand)
        .filter_map(|m| match &m.native {
            Some(Native::Vagrant { boxes, .. }) => Some(boxes),
            _ => None,
        })
        .flatten()
        .filter(|b| b.provider == provider && b.host_architecture.is_none_or(|h| h == host))
        .filter(|b| seen.insert((b.name.clone(), b.version.clone(), b.architecture.map(arch_id))))
        .cloned()
        .collect()
}

/// Whether argv step `step` creates the run's VMs: a `vagrant up` that provisions (a resume's
/// `--no-provision` boots VMs that exist, their boxes there).
fn creates_vms(cx: &Cx<'_>, list: &str, step: &Step) -> bool {
    let Run::Argv(_) = &step.run else { return false };
    let Ok(spec) = spec_for(cx, list, step) else { return false };
    spec.program == "vagrant" && spec.args.first().is_some_and(|a| a == "up") && !spec.args.iter().any(|a| a == "--no-provision")
}

/// Downloads the boxes step `step` of list `list` boots that aren't there yet, in a step of their
/// own, when it creates the run's VMs ([`creates_vms`]); nothing otherwise, nor when every box is
/// there.
pub(in crate::driver) async fn before_step(vagrant: &Vagrant, cx: &Cx<'_>, list: &str, step: &Step) -> Result<()> {
    if !creates_vms(cx, list, step) {
        return Ok(());
    }
    let wanted = wanted(cx);
    if wanted.is_empty() {
        return Ok(());
    }
    let id = id_before(step.id.as_str());
    // What can't be listed is added: Vagrant says which are there already.
    let list_spec = vagrant.vagrant(cx, &id, &["box", "list", "--machine-readable"])?;
    let there = call(cx, &id, &list_spec, READ_LIMIT, Echo::Quiet)
        .await
        .ok()
        .filter(crate::process::Captured::success)
        .map(|out| listed(&out.stdout.join("\n")))
        .unwrap_or_default();
    let host = cx.run.runner.config.arch;
    let missing: Vec<&VagrantBox> = wanted.iter().filter(|b| !is_there(b, host, &there)).collect();
    if missing.is_empty() {
        return Ok(());
    }
    cx.events
        .stepped(&id, Some(NOTE), async {
            for b in missing {
                add(vagrant, cx, &id, b).await?;
            }
            Ok(())
        })
        .await
}

/// `vagrant box add` of `b`, for the step `id`, under a download's rule.
async fn add(vagrant: &Vagrant, cx: &Cx<'_>, id: &str, b: &VagrantBox) -> Result<()> {
    let mut args = vec!["box", "add", b.name.as_str(), "--provider", wire(b)];
    if let Some(v) = &b.version {
        args.extend(["--box-version", v.as_str()]);
    }
    if let Some(a) = b.architecture {
        args.extend(["--architecture", arch_id(a)]);
    }
    let spec = vagrant.vagrant(cx, id, &args)?;
    let out = download(cx, id, &spec, DOWNLOAD_STALL).await?;
    if out.success() {
        return Ok(());
    }
    let reason = out.reason();
    if reason.contains(THERE_ALREADY) {
        return Ok(());
    }
    Err(Error::DownloadFailed {
        step: id.to_owned(),
        reason: cx.events.mask(&reason),
    })
}

#[cfg(test)]
mod tests {
    use isoloom_manifest::{BoxName, BoxVersion, Provider};

    use super::*;

    /// What Vagrant 2.4.9 lists: a box with a build per provider and architecture, an older one
    /// without its architecture.
    const LIST: &str = "1794406460,,ui,info,bento/debian-12 (virtualbox%!(VAGRANT_COMMA) 202510.26.0%!(VAGRANT_COMMA) (arm64))
1794406460,,box-name,bento/debian-12
1794406460,,box-provider,virtualbox
1794406460,,box-version,202510.26.0
1794406460,,box-architecture,arm64
1794406460,,box-name,generic/alpine319
1794406460,,box-provider,libvirt
1794406460,,box-version,4.3.12
1794406460,,box-info,Author,Ladar Levison
1794406460,,box-name,old/box
1794406460,,box-provider,virtualbox
1794406460,,box-version,1.0.0
1794406460,,box-architecture,unknown
";

    fn wanted(name: &str, provider: Provider, version: Option<&str>, arch: Option<Arch>) -> VagrantBox {
        VagrantBox {
            provider,
            name: BoxName::parse(name).unwrap(),
            version: version.map(|v| BoxVersion::parse(v).unwrap()),
            architecture: arch,
            host_architecture: None,
        }
    }

    #[test]
    fn the_boxes_vagrant_lists_are_read_with_their_provider_version_and_build() {
        let there = listed(LIST);
        assert_eq!(there.len(), 3);
        assert_eq!(
            there[0],
            Listed {
                name: "bento/debian-12".into(),
                provider: "virtualbox".into(),
                version: "202510.26.0".into(),
                arch: Some("arm64".into()),
            }
        );
        assert_eq!((there[1].arch.as_ref(), there[2].arch.as_ref()), (None, None), "none said, or unknown");
    }

    #[test]
    fn a_qemu_box_is_the_libvirt_format_vagrant_qemu_boots() {
        // Regression: a lab on QEMU downloaded `cloud-image/debian-12` for `--provider qemu`, then
        // `vagrant up` said "Box Provider: libvirt" and downloaded it again (vagrant-qemu 0.8.0).
        let qemu = wanted("generic/alpine319", Provider::Qemu, None, None);
        assert_eq!(wire(&qemu), "libvirt");
        assert!(is_there(&qemu, Arch::Amd64, &listed(LIST)), "the libvirt build listed is the one it boots");
        assert_eq!(wire(&wanted("bento/debian-12", Provider::Virtualbox, None, None)), "virtualbox");
    }

    #[test]
    fn a_box_is_there_with_its_provider_build_and_exact_version() {
        let there = listed(LIST);
        let bento = |v, a| wanted("bento/debian-12", Provider::Virtualbox, v, a);
        // The computer's build: there on an arm64 computer, not on an x86-64 one.
        assert!(is_there(&bento(None, None), Arch::Arm64, &there));
        assert!(!is_there(&bento(None, None), Arch::Amd64, &there));
        assert!(!is_there(&bento(None, Some(Arch::Amd64)), Arch::Arm64, &there), "a pinned build");
        assert!(is_there(&bento(Some("202510.26.0"), None), Arch::Arm64, &there));
        assert!(!is_there(&bento(Some("202508.03.0"), None), Arch::Arm64, &there), "another version");
        assert!(is_there(&bento(Some(">= 202501"), None), Arch::Arm64, &there), "a constraint: any version");
        assert!(!is_there(&wanted("bento/debian-12", Provider::Parallels, None, None), Arch::Arm64, &there));
        // A box that says no build matches any.
        assert!(is_there(
            &wanted("generic/alpine319", Provider::Libvirt, None, Some(Arch::Amd64)),
            Arch::Arm64,
            &there
        ));
        assert!(is_there(&wanted("old/box", Provider::Virtualbox, Some("1.0.0"), None), Arch::Amd64, &there));
    }
}
