//! The host networks the test makes, removed after it: VirtualBox's host-only adapter
//! (`vboxnet<n>`, its DHCP server and route) for the ping's private network, or VirtualBox 7's
//! host-only network on a Mac; libvirt's management network (`vagrant-libvirt`). Vagrant never
//! removes them, and a route to 192.168.56.0/24 can collide with a VPN's ranges.
//!
//! Only what the test made goes: the networks there before it ([`Networks::here`], read before
//! the VM boots) stay, and so does one a VM still uses once the test VM is gone (other Vagrant
//! projects share them). Linux and macOS only: on Windows removing a host-only adapter asks for
//! elevation, which this never does.

use isothrum_runner::{HostOs, Provider};

use crate::Context;

/// libvirt's system instance.
const LIBVIRT: [&str; 2] = ["-c", "qemu:///system"];

/// The host networks seen at one moment, by kind.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Networks {
    /// VirtualBox's host-only adapters (`VBoxManage list hostonlyifs`).
    vbox_ifs: Vec<String>,
    /// VirtualBox 7's host-only networks, on a Mac (`VBoxManage list hostonlynets`).
    vbox_nets: Vec<String>,
    /// libvirt's networks on the system instance (`virsh net-list --all --name`).
    libvirt: Vec<String>,
}

/// The `Name:` values of a `VBoxManage list …` listing.
fn vbox_names(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.strip_prefix("Name:"))
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty())
        .collect()
}

/// The non-empty lines of a listing.
fn lines(out: &str) -> Vec<String> {
    out.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect()
}

/// `virsh -c qemu:///system` and `args`.
fn virsh<'a>(args: &[&'a str]) -> Vec<&'a str> {
    LIBVIRT.iter().copied().chain(args.iter().copied()).collect()
}

impl Networks {
    /// What is here now of the networks a test on `provider` can make (VirtualBox's, libvirt's;
    /// none for another hypervisor, nor on Windows); a tool that fails lists nothing.
    pub(super) async fn here(cx: &Context, provider: Provider) -> Networks {
        if cx.host.os == HostOs::Windows {
            return Networks::default();
        }
        let listed =
            async |program: &str, args: &[&str], parse: fn(&str) -> Vec<String>| cx.read_ok(program, args).await.map(|out| parse(&out)).unwrap_or_default();
        match provider {
            Provider::Virtualbox => Networks {
                vbox_ifs: listed("VBoxManage", &["list", "hostonlyifs"], vbox_names).await,
                vbox_nets: listed("VBoxManage", &["list", "hostonlynets"], vbox_names).await,
                libvirt: Vec::new(),
            },
            Provider::Libvirt => Networks {
                libvirt: listed("virsh", &virsh(&["net-list", "--all", "--name"]), lines).await,
                ..Networks::default()
            },
            _ => Networks::default(),
        }
    }

    /// What is in `self` and wasn't in `before`.
    fn made_since(&self, before: &Networks) -> Networks {
        let new = |now: &[String], then: &[String]| now.iter().filter(|n| !then.contains(n)).cloned().collect();
        Networks {
            vbox_ifs: new(&self.vbox_ifs, &before.vbox_ifs),
            vbox_nets: new(&self.vbox_nets, &before.vbox_nets),
            libvirt: new(&self.libvirt, &before.libvirt),
        }
    }
}

/// Removes the networks made on `provider` since `before` (read before the test VM booted) that no
/// VM uses now the test VM is gone: the names removed; why one couldn't be (the test's result
/// stands either way).
pub(super) async fn remove_made_since(cx: &Context, provider: Provider, before: &Networks) -> Result<Vec<String>, String> {
    if cx.host.os == HostOs::Windows {
        return Ok(Vec::new());
    }
    let made = Networks::here(cx, provider).await.made_since(before);
    let mut done = Removal::default();
    remove_vbox(cx, &made, &mut done).await;
    remove_libvirt(cx, &made.libvirt, &mut done).await;
    if done.failed.is_empty() {
        Ok(done.removed)
    } else {
        Err(done.failed.join("; "))
    }
}

/// What a removal did: the networks removed, and the ones that couldn't be, each with why.
#[derive(Default)]
struct Removal {
    removed: Vec<String>,
    failed: Vec<String>,
}

impl Removal {
    /// Notes how removing `name` went.
    fn note<T>(&mut self, name: &str, went: Result<T, String>) {
        match went {
            Ok(_) => self.removed.push(name.to_owned()),
            Err(e) => self.failed.push(format!("{name}: {e}")),
        }
    }
}

/// Removes VirtualBox's host-only adapters (each with its DHCP server) and networks of `made` that
/// no registered VM is attached to.
async fn remove_vbox(cx: &Context, made: &Networks, done: &mut Removal) {
    if made.vbox_ifs.is_empty() && made.vbox_nets.is_empty() {
        return;
    }
    let vms = cx.read_ok("VBoxManage", &["list", "vms", "--long"]).await.unwrap_or_default();
    for name in made.vbox_ifs.iter().filter(|n| !vbox_uses(&vms, "Host-only Interface", n)) {
        let went = cx.read_ok("VBoxManage", &["hostonlyif", "remove", name]).await;
        if went.is_ok() {
            // Its DHCP server goes with it; one never made is no error.
            drop(
                cx.read_ok("VBoxManage", &["dhcpserver", "remove", &format!("--network=HostInterfaceNetworking-{name}")])
                    .await,
            );
        }
        done.note(name, went);
    }
    for name in made.vbox_nets.iter().filter(|n| !vbox_uses(&vms, "Host-only Network", n)) {
        done.note(name, cx.read_ok("VBoxManage", &["hostonlynet", "remove", &format!("--name={name}")]).await);
    }
}

/// Removes the libvirt networks `made` that no domain has an interface on.
async fn remove_libvirt(cx: &Context, made: &[String], done: &mut Removal) {
    for name in made {
        if libvirt_in_use(cx, name).await {
            continue;
        }
        drop(cx.read_ok("virsh", &virsh(&["net-destroy", name])).await);
        done.note(name, cx.read_ok("virsh", &virsh(&["net-undefine", name])).await);
    }
}

/// Whether a VM of `VBoxManage list vms --long` (`vms`) is attached to `kind` `name`
/// (`Attachment: Host-only Interface 'vboxnet0'`).
fn vbox_uses(vms: &str, kind: &str, name: &str) -> bool {
    vms.lines().any(|l| l.contains(kind) && l.contains(&format!("'{name}'")))
}

/// Whether a libvirt domain of the system instance has an interface on network `name` (one that
/// can't be told is in use).
async fn libvirt_in_use(cx: &Context, name: &str) -> bool {
    let Ok(domains) = cx.read_ok("virsh", &virsh(&["list", "--all", "--name"])).await else {
        return true;
    };
    for domain in lines(&domains) {
        let Ok(ifs) = cx.read_ok("virsh", &virsh(&["domiflist", &domain])).await else {
            return true;
        };
        if ifs.lines().any(|l| l.split_whitespace().nth(2) == Some(name)) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_names_listed_are_read_and_what_is_new_is_told_apart() {
        let listing = "Name:            vboxnet0\nGUID:            786f\n\nName:            vboxnet1\n";
        assert_eq!(vbox_names(listing), ["vboxnet0", "vboxnet1"]);
        let before = Networks {
            vbox_ifs: vec!["vboxnet0".into()],
            libvirt: vec!["default".into()],
            ..Networks::default()
        };
        let now = Networks {
            vbox_ifs: vec!["vboxnet0".into(), "vboxnet1".into()],
            libvirt: vec!["default".into(), "vagrant-libvirt".into()],
            ..Networks::default()
        };
        let made = Networks {
            vbox_ifs: vec!["vboxnet1".into()],
            libvirt: vec!["vagrant-libvirt".into()],
            ..Networks::default()
        };
        assert_eq!(now.made_since(&before), made);
        assert!(vbox_uses(
            "NIC 2:  MAC: 0800, Attachment: Host-only Interface 'vboxnet1', Cable connected: on",
            "Host-only Interface",
            "vboxnet1"
        ));
        assert!(!vbox_uses(
            "NIC 2:  MAC: 0800, Attachment: Host-only Interface 'vboxnet10'",
            "Host-only Interface",
            "vboxnet1"
        ));
    }
}
