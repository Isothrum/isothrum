//! KVM on Linux: whether a provider boots its VMs through `/dev/kvm` (libvirt, and QEMU with
//! `accel=kvm`), whether this account may open it, whether KVM holds the CPU's virtualization
//! (which VirtualBox then can't have), and the groups a user joins for it, which apply from their
//! next login.

use super::{Provider, Unavailable};
use crate::context::Context;
use crate::host::{Host, HostOs};

/// Why KVM's hypervisors can't run VMs on `host` although they are installed: vagrant-libvirt
/// and vagrant-qemu boot KVM guests (QEMU with `accel=kvm`, as Vagrantfiles ask on Linux), so they
/// need `/dev/kvm`, and the user must be allowed to open it. `None` when they can.
pub(super) fn kvm_problem(host: &Host) -> Option<Unavailable> {
    let device = &host.devices.kvm;
    if !device.exists() {
        return Some(Unavailable::NoKvm);
    }
    match std::fs::OpenOptions::new().read(true).write(true).open(device) {
        Ok(_) => None,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Some(Unavailable::KvmDenied { group: kvm_group(host) }),
        Err(e) => Some(Unavailable::KvmUnusable { error: e.to_string() }),
    }
}

/// Whether KVM holds the CPU's virtualization extensions on `host`, so VirtualBox can't start VMs
/// there whatever is installed. Not off Linux, when it doesn't, or when it can't be told
/// (`/proc/modules` unreadable).
pub(crate) fn holds_virtualization(host: &Host) -> bool {
    if host.os != HostOs::Linux {
        return false;
    }
    let Ok(modules) = std::fs::read_to_string(&host.devices.proc_modules) else {
        return false;
    };
    let at_load = std::fs::read_to_string(&host.devices.kvm_virt_at_load).ok();
    holds(&modules, at_load.as_deref())
}

/// [`holds_virtualization`], from `/proc/modules`' text and KVM's `enable_virt_at_load` (none
/// before Linux 6.12, whose KVM takes VT-x only while one of its guests runs): KVM's CPU module
/// loaded, and turning virtualization on as it loads.
fn holds(modules: &str, enable_virt_at_load: Option<&str>) -> bool {
    let loaded = modules.lines().any(|l| matches!(l.split_whitespace().next(), Some("kvm_intel" | "kvm_amd")));
    loaded && enable_virt_at_load.is_some_and(|v| matches!(v.trim(), "Y" | "y" | "1"))
}

/// KVM's CPU module loaded on `host` (`kvm_intel`, `kvm_amd`), as `/proc/modules` lists it.
#[must_use]
pub(crate) fn cpu_module(host: &Host) -> Option<&'static str> {
    let modules = std::fs::read_to_string(&host.devices.proc_modules).ok()?;
    modules.lines().find_map(|l| match l.split_whitespace().next() {
        Some("kvm_intel") => Some("kvm_intel"),
        Some("kvm_amd") => Some("kvm_amd"),
        _ => None,
    })
}

/// Whether KVM is a module Linux can unload and load again with new options (`kvm`'s `refcnt`
/// in `/sys/module`), not built into the kernel, where only the kernel's command line sets them.
#[must_use]
pub(crate) fn loadable(host: &Host) -> bool {
    host.devices.sys_module.join("kvm/refcnt").is_file()
}

/// Whether anything but the modules built on it uses KVM now: a VM running (each holds KVM's CPU
/// module), a program holding `/dev/kvm` open. Read for `kvm` and its CPU module from
/// `/sys/module`: more holding one (`refcnt`) than the modules that do (`holders`). Unreadable
/// counts as in use: nothing is unloaded on a guess.
#[must_use]
pub(crate) fn in_use(host: &Host) -> bool {
    let module = |name: &str| host.devices.sys_module.join(name);
    std::iter::once("kvm").chain(cpu_module(host)).any(|name| {
        let held = std::fs::read_to_string(module(name).join("refcnt"))
            .ok()
            .and_then(|r| r.trim().parse::<usize>().ok());
        let by_modules = std::fs::read_dir(module(name).join("holders")).map_or(0, Iterator::count);
        held.is_none_or(|held| held > by_modules)
    })
}

/// Whether modprobe gives KVM `enable_virt_at_load=0` the next time it loads it (a `*.conf` file
/// of `/etc/modprobe.d` says so): KVM then takes the CPU's virtualization only while one of its
/// VMs runs.
#[must_use]
pub(crate) fn shares_from_next_load(host: &Host) -> bool {
    let Ok(entries) = std::fs::read_dir(&host.devices.modprobe_d) else {
        return false;
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "conf"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .any(|text| text.lines().any(shares_on_load))
}

/// Whether one line of a modprobe configuration gives KVM `enable_virt_at_load` off.
fn shares_on_load(line: &str) -> bool {
    let mut words = line.split_whitespace();
    words.next() == Some("options") && words.next() == Some("kvm") && words.any(|w| matches!(w.split_once('='), Some(("enable_virt_at_load", "0" | "N" | "n"))))
}

/// The group a user joins to open `host`'s `/dev/kvm`: the one it belongs to (`kvm` wherever a
/// distribution makes it `0660`), else `kvm`, the name every distribution gives it.
#[must_use]
pub fn kvm_group(host: &Host) -> String {
    let groups = std::fs::read_to_string(&host.devices.etc_group).unwrap_or_default();
    group_of(device_gid(&host.devices.kvm), &groups)
}

/// The group id the file `path` belongs to, where the OS has one.
#[cfg(unix)]
fn device_gid(path: &std::path::Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).ok().map(|m| m.gid())
}

/// The group id the file `path` belongs to, where the OS has one.
#[cfg(not(unix))]
fn device_gid(_path: &std::path::Path) -> Option<u32> {
    None
}

/// The name of group `gid` in `etc_group` (`/etc/group`'s text), when it is one a user joins (not
/// root's); else `kvm`.
fn group_of(gid: Option<u32>, etc_group: &str) -> String {
    gid.filter(|g| *g != 0)
        .and_then(|gid| {
            etc_group.lines().find_map(|l| {
                let mut fields = l.split(':');
                let name = fields.next()?;
                (fields.nth(1)?.parse::<u32>().ok()? == gid).then(|| name.to_owned())
            })
        })
        .unwrap_or_else(|| "kvm".to_owned())
}

/// The groups libvirt's socket and `/dev/kvm` are opened through.
const LIBVIRT_GROUPS: [&str; 2] = ["libvirt", "kvm"];

/// Whether `provider` boots KVM guests on `os`, through `/dev/kvm`: libvirt, and QEMU on Linux
/// (vagrant-qemu asks for `accel=kvm` there; HVF on a Mac).
#[must_use]
pub fn uses_kvm(provider: Provider, os: HostOs) -> bool {
    provider == Provider::Libvirt || (provider == Provider::Qemu && os == HostOs::Linux)
}

/// The groups a user joins to use `provider` on `host`, which apply from their next login:
/// libvirt's and `/dev/kvm`'s for libvirt, `/dev/kvm`'s for QEMU on Linux ([`kvm_group`]); none
/// for another.
#[must_use]
pub fn login_groups(provider: Provider, host: &Host) -> Vec<String> {
    match provider {
        Provider::Libvirt => {
            let mut groups: Vec<String> = LIBVIRT_GROUPS.map(str::to_owned).to_vec();
            let device = kvm_group(host);
            if !groups.contains(&device) {
                groups.push(device);
            }
            groups
        }
        Provider::Qemu if uses_kvm(provider, host.os) => vec![kvm_group(host)],
        _ => Vec::new(),
    }
}

/// The group a new login applies for `provider` ([`Unavailable::NewLogin`]): libvirt's for
/// libvirt, `/dev/kvm`'s for QEMU.
pub(super) fn new_login_group(provider: Provider, host: &Host) -> String {
    if provider == Provider::Libvirt {
        "libvirt".to_owned()
    } else {
        kvm_group(host)
    }
}

/// The groups this account belongs to that this session doesn't have yet (Linux): added since it
/// logged in, like the libvirt and kvm groups an install adds it to, they apply from its next
/// login. `id -nG <user>` reads the group database, `id -nG` this process's own groups. Empty
/// elsewhere, or when it can't be told.
pub async fn groups_pending(cx: &Context) -> Vec<String> {
    if cx.host.os != HostOs::Linux {
        return Vec::new();
    }
    let Ok(user) = cx.read_ok("id", &["-un"]).await else {
        return Vec::new();
    };
    let user = user.trim();
    match (cx.read_ok("id", &["-nG", user]).await, cx.read_ok("id", &["-nG"]).await) {
        (Ok(account), Ok(session)) => pending_groups(&account, &session),
        _ => Vec::new(),
    }
}

/// Of the account's groups (`id -nG <user>`), those the session's (`id -nG`) lack.
fn pending_groups(account: &str, session: &str) -> Vec<String> {
    let session: Vec<&str> = session.split_whitespace().collect();
    account.split_whitespace().filter(|g| !session.contains(g)).map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use isothrum_runner::fake::{FakeProcesses, Reply};

    use super::*;

    fn linux(root: &std::path::Path) -> Host {
        crate::testing::linux_host(root)
    }

    #[test]
    fn kvm_holds_the_cpus_virtualization_once_its_module_turns_it_on_as_it_loads() {
        // Debian 13 (Linux 6.12): kvm_intel loaded, enable_virt_at_load Y.
        let modules =
            "kvm_intel 413696 0 - Live 0x0000000000000000\nkvm 1404928 1 kvm_intel, Live 0x0000000000000000\nirqbypass 12288 1 kvm, Live 0x0000000000000000\n";
        assert!(holds(modules, Some("Y\n")));
        assert!(holds(&modules.replace("kvm_intel", "kvm_amd"), Some("Y\n")));
        assert!(!holds(modules, Some("N\n")), "kvm.enable_virt_at_load=0: only while a KVM guest runs");
        assert!(!holds(modules, None), "before Linux 6.12: only while a KVM guest runs");
        assert!(
            !holds(
                "vboxdrv 696320 2 vboxnetadp, Live 0x0000000000000000\nkvm 1404928 0 - Live 0x0000000000000000\n",
                Some("Y\n")
            ),
            "no CPU module"
        );
    }

    #[test]
    fn kvm_holding_virtualization_is_read_from_the_hosts_files_on_linux_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut host = linux(dir.path());
        assert!(!holds_virtualization(&host), "no /proc/modules: can't be told");
        for (path, text) in [
            (&host.devices.proc_modules, "kvm_amd 1 0 - Live 0x0\n"),
            (&host.devices.kvm_virt_at_load, "Y\n"),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        assert!(holds_virtualization(&host));
        host.os = HostOs::Macos;
        assert!(!holds_virtualization(&host));
    }

    #[test]
    fn the_kvm_group_is_the_one_dev_kvm_belongs_to() {
        let etc_group = "root:x:0:\nkvm:x:993:\nlibvirt:x:994:ada\nhypervisors:x:1200:\n";
        assert_eq!(group_of(Some(993), etc_group), "kvm");
        assert_eq!(group_of(Some(1200), etc_group), "hypervisors", "a distribution's own name for it");
        assert_eq!(group_of(Some(0), etc_group), "kvm", "root's: a user doesn't join it");
        assert_eq!(group_of(Some(4242), etc_group), "kvm", "a group /etc/group doesn't name");
        assert_eq!(group_of(None, etc_group), "kvm", "no /dev/kvm");
    }

    #[test]
    fn a_kvm_device_missing_or_not_the_users_or_unusable_says_which() {
        let dir = tempfile::tempdir().unwrap();
        let host = linux(dir.path());
        assert_eq!(kvm_problem(&host), Some(Unavailable::NoKvm));
        std::fs::create_dir_all(host.devices.kvm.parent().unwrap()).unwrap();
        std::fs::write(&host.devices.kvm, "").unwrap();
        assert_eq!(kvm_problem(&host), None);
        // Regression: any failure to open it (a folder here; EBUSY, ENXIO on a real device) was
        // said as a device the user may not open. Unix only: Windows refuses to open a folder
        // with "access denied", the permission problem's own kind.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut host = host;
            host.devices.kvm = dir.path().to_path_buf();
            assert!(matches!(kvm_problem(&host), Some(Unavailable::KvmUnusable { .. })), "{:?}", kvm_problem(&host));
            let denied = dir.path().join("denied");
            std::fs::write(&denied, "").unwrap();
            std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o400)).unwrap();
            host.devices.kvm = denied;
            // Root opens any file: as root the device is usable.
            if let Some(problem) = kvm_problem(&host) {
                assert!(matches!(problem, Unavailable::KvmDenied { .. }), "{problem:?}");
            }
        }
    }

    #[test]
    fn libvirt_and_qemu_on_linux_boot_through_kvm_and_join_its_groups() {
        let dir = tempfile::tempdir().unwrap();
        let host = linux(dir.path());
        assert!(uses_kvm(Provider::Libvirt, HostOs::Linux) && uses_kvm(Provider::Qemu, HostOs::Linux));
        assert!(!uses_kvm(Provider::Qemu, HostOs::Macos) && !uses_kvm(Provider::Qemu, HostOs::Windows) && !uses_kvm(Provider::Virtualbox, HostOs::Linux));
        assert_eq!(login_groups(Provider::Libvirt, &host), ["libvirt", "kvm"]);
        assert_eq!(login_groups(Provider::Qemu, &host), ["kvm"]);
        assert_eq!(login_groups(Provider::Virtualbox, &host), Vec::<String>::new());
        let mac = Host {
            os: HostOs::Macos,
            ..host.clone()
        };
        assert_eq!(login_groups(Provider::Qemu, &mac), Vec::<String>::new());
        assert_eq!(new_login_group(Provider::Libvirt, &host), "libvirt");
        assert_eq!(new_login_group(Provider::Qemu, &host), "kvm");
    }

    #[test]
    fn the_groups_pending_are_the_accounts_the_session_lacks() {
        assert_eq!(
            pending_groups("vagrant sudo docker libvirt kvm\n", "vagrant sudo\n"),
            ["docker", "libvirt", "kvm"]
        );
        assert_eq!(pending_groups("vagrant sudo libvirt\n", "vagrant libvirt sudo\n"), Vec::<String>::new());
        assert_eq!(pending_groups("", "vagrant"), Vec::<String>::new());
    }

    #[tokio::test(start_paused = true)]
    async fn the_groups_pending_are_asked_of_id_on_linux_only() {
        let dir = tempfile::tempdir().unwrap();
        let tools = Arc::new(
            FakeProcesses::script()
                .expect("id -un", Reply::ok("ada"))
                .expect("id -nG ada", Reply::ok("ada sudo libvirt kvm"))
                .expect("id -nG", Reply::ok("ada sudo"))
                .build(),
        );
        let cx = Context::new(tools, linux(dir.path()));
        assert_eq!(groups_pending(&cx).await, ["libvirt", "kvm"]);
        let unknown = Context::new(Arc::new(FakeProcesses::script().expect("id -un", Reply::missing()).build()), linux(dir.path()));
        assert_eq!(groups_pending(&unknown).await, Vec::<String>::new());
        let nothing = Arc::new(FakeProcesses::script().build());
        let mac = Context::new(
            nothing,
            Host {
                os: HostOs::Macos,
                ..linux(dir.path())
            },
        );
        assert_eq!(groups_pending(&mac).await, Vec::<String>::new());
    }
}
