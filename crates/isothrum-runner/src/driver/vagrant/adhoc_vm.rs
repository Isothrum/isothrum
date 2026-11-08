//! The ad-hoc attack VM of a VM lab (a Vagrant run with no `guest` and no on-demand access
//! machine of its own): a node the runner adds to the running environment, as GNS3 adds one to a
//! live topology. `attach` writes its definition under the run's state folder
//! (`<VAGRANT_DOTFILE_PATH>/adhoc/isoloom-access.rb`), which every Vagrantfile Isoloom writes
//! loads at the end of `Vagrant.configure`: one line calling the Vagrantfile's
//! `IsoloomAdHoc.define` with the name, the chosen lab network, an address the runner picked free
//! among the manifest's machines, and the learner's box (none: the Vagrantfile's default for this
//! host's CPU). Isoloom's hook renders what the VM needs to join that network on the run's
//! provider; the runner then boots it alone (`vagrant up isoloom-access --provider=<p>`). Status
//! reads it from `vagrant status`, its address kept beside the definition; `detach` destroys it
//! alone and removes the definition; the shell is `vagrant ssh isoloom-access`, as into any
//! define. Hyper-V, ESXi and UTM can't give a late VM a lab network this way: refused, as a
//! Vagrantfile older than the hook is.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use isoloom_manifest::Manifest;
use serde::{Deserialize, Serialize};

use super::{DETACH_TIMEOUT, Vagrant, status};
use crate::Result;
use crate::driver::{ADHOC_NAME, Attached};
use crate::error::Error;
use crate::executor::{Cx, Echo, call_ok};
use crate::state::Op;
use crate::status::MachineStatus;

/// The folder of the runner's definitions in the run's state folder: what the Vagrantfile's
/// `IsoloomAdHoc.files` globs.
const DIR: &str = "adhoc";

/// The Ruby module every Vagrantfile with the hook defines.
const HOOK: &str = "IsoloomAdHoc";

/// The providers on which the hook gives a late VM its lab network (an internal network, a
/// libvirt network, a QEMU hub port). Hyper-V's switches and MACs, ESXi's port groups and UTM's
/// missing private networks are not rendered for it.
const PROVIDERS: [&str; 5] = ["virtualbox", "vmware_desktop", "parallels", "libvirt", "qemu"];

/// The VM as the runner added it, kept beside its definition for status.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Node {
    /// The lab network it joined.
    network: String,
    /// Its address there.
    address: Ipv4Addr,
    /// The box it was booted from, when the learner picked one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    r#box: Option<String>,
}

/// The definition's folder, `<dotfile>/adhoc`.
fn dir(v: &Vagrant, cx: &Cx<'_>) -> Result<PathBuf> {
    Ok(v.dotfile(cx)?.join(DIR))
}

/// The definition (`.rb`) and the node beside it (`.json`).
fn files(v: &Vagrant, cx: &Cx<'_>) -> Result<(PathBuf, PathBuf)> {
    let dir = dir(v, cx)?;
    Ok((dir.join(format!("{ADHOC_NAME}.rb")), dir.join(format!("{ADHOC_NAME}.json"))))
}

/// Whether the run has an ad-hoc VM defined (added, and not yet detached).
pub(super) fn present(v: &Vagrant, cx: &Cx<'_>) -> bool {
    files(v, cx).is_ok_and(|(rb, _)| rb.is_file())
}

/// The node the runner added, when one is defined and its record reads.
fn node(v: &Vagrant, cx: &Cx<'_>) -> Option<Node> {
    let (_, json) = files(v, cx).ok()?;
    serde_json::from_str(&std::fs::read_to_string(json).ok()?).ok()
}

/// A Ruby double-quoted string: no interpolation, no quote or backslash escaping out of it.
fn ruby_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' | '"' | '#' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The one-line definition the Vagrantfile's hook reads.
fn definition(node: &Node) -> String {
    let r#box = node.r#box.as_deref().map_or_else(|| "nil".to_owned(), ruby_str);
    format!(
        "# The attack VM the runner added to this running lab (`isothrum attach`); `isothrum detach` removes it.\n{HOOK}.define(config, {}, network: {}, ip: {}, box: {box})\n",
        ruby_str(ADHOC_NAME),
        ruby_str(&node.network),
        ruby_str(&node.address.to_string())
    )
}

/// The lab network the VM joins: `chosen`, else the manifest's first.
fn network(manifest: &Manifest, chosen: Option<&str>) -> Result<String> {
    let names: Vec<&str> = manifest.networks.iter().map(|n| n.name.as_str()).collect();
    match chosen {
        Some(n) if names.contains(&n) => Ok(n.to_owned()),
        Some(n) => Err(Error::Unsupported {
            what: format!("an ad-hoc attack VM on network `{n}` (this lab's networks: {})", names.join(", ")),
        }),
        None => names.first().map(|n| (*n).to_owned()).ok_or_else(|| Error::Unsupported {
            what: "an ad-hoc attack VM (this lab has no network)".into(),
        }),
    }
}

/// A free address on `network`: the highest below the two last usable ones (Isoloom's controller
/// and router) and the tools' that no manifest machine holds (`.252` in a /24 with no tools), as
/// Isoloom places its own VMs. IPv4 blocks of /24 to /29 only (what Isoloom accepts).
fn free_address(manifest: &Manifest, network: &str) -> Result<Ipv4Addr> {
    let cidr = manifest
        .networks
        .iter()
        .find(|n| n.name.as_str() == network)
        .map(|n| n.cidr.to_string())
        .unwrap_or_default();
    let block = || Error::Unsupported {
        what: format!("an ad-hoc attack VM on network `{network}` ({cidr}: not an IPv4 block of /24 to /29)"),
    };
    let (base, len) = cidr.split_once('/').ok_or_else(block)?;
    let base: Ipv4Addr = base.parse().map_err(|_| block())?;
    let len: u8 = len.parse().map_err(|_| block())?;
    if !(24..=29).contains(&len) {
        return Err(block());
    }
    let taken: Vec<Ipv4Addr> = manifest
        .machines
        .iter()
        .filter_map(|m| m.addresses.iter().find(|(n, _)| n.as_str() == network))
        .filter_map(|(_, a)| match a {
            IpAddr::V4(v4) => Some(*v4),
            IpAddr::V6(_) => None,
        })
        .collect();
    let size = 1u32 << (32 - len);
    let (lowest, mut ip) = (u32::from(base) + 2, u32::from(base) + size - 4);
    while taken.contains(&Ipv4Addr::from(ip)) {
        if ip <= lowest {
            return Err(Error::Unsupported {
                what: format!("an ad-hoc attack VM on network `{network}` (no address left)"),
            });
        }
        ip -= 1;
    }
    Ok(Ipv4Addr::from(ip))
}

/// Why the hook can't serve the VM, if it can't: a provider it renders no network for, or a
/// Vagrantfile written before the hook existed.
fn refusal(provider: &str, vagrantfile: &str) -> Option<String> {
    if !PROVIDERS.contains(&provider) {
        return Some(format!(
            "an ad-hoc attack VM on `{provider}` (it joins VirtualBox, VMware, Parallels, libvirt and QEMU networks; declare an `access: true` machine for the others)"
        ));
    }
    (!vagrantfile.contains(HOOK))
        .then(|| "an ad-hoc attack VM (this lab's Vagrantfile predates the hook: generate it again with Isoloom 1.0.9 or later)".to_owned())
}

/// Refuses what the hook can't serve ([`refusal`]), before anything is written or booted.
fn check(cx: &Cx<'_>) -> Result<()> {
    let vagrantfile = cx.run.dir().join("Vagrantfile");
    let text = std::fs::read_to_string(&vagrantfile).map_err(Error::io(&vagrantfile, "read"))?;
    match refusal(cx.scope.provider.as_deref().unwrap_or_default(), &text) {
        Some(what) => Err(Error::Unsupported { what }),
        None => Ok(()),
    }
}

/// Adds the attack VM to the running lab: its definition written on `network` (the first
/// otherwise) at a free address, from `image` (the host's default box otherwise), then booted
/// alone on the run's provider. A definition a previous attach left is replaced.
pub(super) async fn attach(v: &Vagrant, cx: &Cx<'_>, image: Option<&str>, network: Option<&str>) -> Result<Attached> {
    check(cx)?;
    let manifest = cx.run.manifest();
    let network = self::network(manifest, network)?;
    let node = Node {
        address: free_address(manifest, &network)?,
        network,
        r#box: image.map(str::to_owned),
    };
    let (rb, json) = files(v, cx)?;
    let dir = dir(v, cx)?;
    std::fs::create_dir_all(&dir).map_err(Error::io(&dir, "created"))?;
    std::fs::write(&rb, definition(&node)).map_err(Error::io(&rb, "written"))?;
    let record = serde_json::to_string(&node).unwrap_or_default();
    std::fs::write(&json, record).map_err(Error::io(&json, "written"))?;
    v.boot(cx, ADHOC_NAME, image).await?;
    Ok(Attached {
        name: Some(ADHOC_NAME.to_owned()),
        network: Some(node.network),
        address: Some(node.address.to_string()),
    })
}

/// Takes the attack VM out of the running lab: destroyed alone, its definition removed. A run
/// with none is left as it is.
pub(super) async fn detach(v: &Vagrant, cx: &Cx<'_>) -> Result<()> {
    if !present(v, cx) {
        return Ok(());
    }
    let spec = v.vagrant(cx, Op::Detach.as_str(), &["destroy", "-f", ADHOC_NAME])?;
    call_ok(cx, Op::Detach.as_str(), &spec, DETACH_TIMEOUT, Echo::Shown).await?;
    let (rb, json) = files(v, cx)?;
    drop(std::fs::remove_file(rb));
    drop(std::fs::remove_file(json));
    Ok(())
}

/// The attack VM as a status machine, when one is defined: its state as `vagrant status` reads
/// it (`read`), its address on the network it joined.
pub(super) fn status(v: &Vagrant, cx: &Cx<'_>, read: &status::Reading) -> Option<MachineStatus> {
    if !present(v, cx) {
        return None;
    }
    let state = read
        .states
        .get(ADHOC_NAME)
        .map_or(crate::status::MachineState::Absent, |s| status::machine_state(s));
    let addresses: BTreeMap<String, String> = node(v, cx).map(|n| [(n.network, n.address.to_string())].into()).unwrap_or_default();
    Some(MachineStatus {
        name: ADHOC_NAME.to_owned(),
        role: isoloom_manifest::Role::Access,
        state,
        addresses,
        published: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        isoloom_manifest::parse(crate::testing::FLAG_VM).unwrap()
    }

    #[test]
    fn the_definition_is_one_hook_call_with_its_strings_kept_literal() {
        let node = Node {
            network: "lab".into(),
            address: "10.61.0.252".parse().unwrap(),
            r#box: Some("me/kali \"2026\" #1".into()),
        };
        let text = definition(&node);
        assert!(text.starts_with("# The attack VM the runner added"), "{text}");
        assert!(
            text.ends_with("IsoloomAdHoc.define(config, \"isoloom-access\", network: \"lab\", ip: \"10.61.0.252\", box: \"me/kali \\\"2026\\\" \\#1\")\n"),
            "{text}"
        );
        let default = definition(&Node { r#box: None, ..node });
        assert!(default.ends_with("box: nil)\n"), "the host's default box: {default}");
    }

    #[test]
    fn the_address_is_the_highest_free_one_below_isolooms_own() {
        // flag-vm: web at .10, the controller at .253 on 10.61.0.0/24.
        assert_eq!(free_address(&manifest(), "lab").unwrap().to_string(), "10.61.0.252");
        let mut m = manifest();
        let taken: isoloom_manifest::Name = "lab".parse().unwrap();
        m.machines[0].addresses.insert(taken, "10.61.0.252".parse().unwrap());
        assert_eq!(free_address(&m, "lab").unwrap().to_string(), "10.61.0.251");
        assert_eq!(free_address(&manifest(), "nope").unwrap_err().code(), crate::ErrorCode::Unsupported);
    }

    #[test]
    fn the_hook_serves_the_desktop_hypervisors_libvirt_and_qemu_from_a_vagrantfile_that_has_it() {
        let hooked = "# generated\nmodule IsoloomAdHoc\nend\n";
        for p in PROVIDERS {
            assert_eq!(refusal(p, hooked), None, "{p}");
        }
        for p in ["hyperv", "vmware_esxi", "utm", ""] {
            assert!(refusal(p, hooked).is_some_and(|why| why.contains(&format!("on `{p}`"))), "{p}");
        }
        assert!(refusal("qemu", "# generated before the hook\n").is_some_and(|why| why.contains("predates the hook")));
    }

    #[test]
    fn the_network_is_the_chosen_one_else_the_first() {
        let m = manifest();
        assert_eq!(network(&m, None).unwrap(), "lab");
        assert_eq!(network(&m, Some("lab")).unwrap(), "lab");
        let err = network(&m, Some("dmz")).unwrap_err().to_string();
        assert!(err.contains("network `dmz`") && err.contains("lab"), "{err}");
    }
}
