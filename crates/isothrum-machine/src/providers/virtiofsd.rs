//! Whether a hypervisor finds the virtiofsd it shares a folder of this computer with a VM through,
//! on Linux:
//! - libvirt: vagrant-libvirt shares the folder as a `virtiofs` filesystem, and libvirt starts the
//!   `virtiofsd` a vhost-user descriptor names for it. With none, the VM's start fails deep in
//!   vagrant-libvirt ("Unable to find a satisfying virtiofsd").
//! - QEMU: vagrant-qemu (0.7.0 on) starts the `virtiofsd` it finds itself, as its `virtiofsd_bin`
//!   defaults: on the `PATH`, then `/usr/libexec/virtiofsd` (the package's: Debian, Ubuntu,
//!   Fedora), then `/usr/lib/qemu/virtiofsd` (QEMU's own, before the package). With none, Vagrant
//!   stops with its error ("virtiofsd was not found or is not executable").
//!
//! Without one, the hypervisor isn't ready. virtiofsd is its own package from Debian 12, Ubuntu
//! 23.10 and Fedora 36 on (QEMU's before), and neither libvirt nor QEMU depends on it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::Provider;
use crate::host::{Devices, HostOs};

/// Where libvirt reads QEMU's vhost-user descriptors, in the order it reads them: a descriptor in
/// a later folder takes the place of an earlier one of the same name (an administrator's, in
/// `/etc`, over the distribution's). libvirt's system daemon (`qemu:///system`, which Vagrant
/// uses) reads no user's folder.
pub(crate) const VHOST_USER_DIRS: [&str; 2] = ["/usr/share/qemu/vhost-user", "/etc/qemu/vhost-user"];

/// Where vagrant-qemu looks for a virtiofsd after the `PATH`, in its order (its
/// `Config#find_virtiofsd`).
pub(crate) const VAGRANT_QEMU_FALLBACKS: [&str; 2] = ["/usr/libexec/virtiofsd", "/usr/lib/qemu/virtiofsd"];

/// Whether `provider`'s VMs need a virtiofsd on `os` for the folders this computer shares with
/// them: libvirt's and QEMU's, on Linux (QEMU on macOS shares none,
/// [`ProviderRow::shares_folders`](super::ProviderRow::shares_folders)).
#[must_use]
pub fn needs_virtiofsd(provider: Provider, os: HostOs) -> bool {
    matches!(provider, Provider::Libvirt | Provider::Qemu) && os == HostOs::Linux
}

/// The virtiofsd `provider` would start, where it looks: libvirt the one its descriptors name
/// ([`Devices::vhost_user`]), vagrant-qemu the first it may run of [`Devices::virtiofsd`]. `None`
/// when it finds none, or for a provider that starts none.
#[must_use]
pub fn virtiofsd(provider: Provider, devices: &Devices) -> Option<PathBuf> {
    match provider {
        Provider::Libvirt => described_in(&devices.vhost_user),
        Provider::Qemu => devices.virtiofsd.iter().find(|f| runnable(f)).cloned(),
        _ => None,
    }
}

/// Where vagrant-qemu looks for a virtiofsd, in its order: `virtiofsd` in each folder of `path`
/// (an empty entry skipped: it isn't the current folder), then `fallbacks`.
pub(crate) fn vagrant_qemu_candidates(path: Option<&OsStr>, fallbacks: &[impl AsRef<Path>]) -> Vec<PathBuf> {
    let on_path = path
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.join("virtiofsd"));
    on_path.chain(fallbacks.iter().map(|f| f.as_ref().to_path_buf())).collect()
}

/// Whether `file` is a file this account may run (an execute bit set, on Unix), as vagrant-qemu
/// reads it (`File.executable?`).
fn runnable(file: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(file).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        file.is_file()
    }
}

/// The virtiofsd libvirt would start, from the vhost-user descriptors in `dirs` (read as libvirt
/// reads them, [`VHOST_USER_DIRS`]): the first, by file name, of type `fs` whose program is
/// there. An empty file hides the earlier folders' descriptor of its name, as libvirt reads it.
fn described_in(dirs: &[impl AsRef<Path>]) -> Option<PathBuf> {
    let mut descriptors = std::collections::BTreeMap::new();
    for dir in dirs {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                descriptors.insert(entry.file_name(), path);
            }
        }
    }
    descriptors.values().find_map(|path| fs_binary(&std::fs::read_to_string(path).ok()?))
}

/// The program descriptor `json` names, when it is a virtiofsd's (`"type": "fs"`) and the program
/// is there.
fn fs_binary(json: &str) -> Option<PathBuf> {
    let descriptor: serde_json::Value = serde_json::from_str(json).ok()?;
    if descriptor.get("type").and_then(serde_json::Value::as_str) != Some("fs") {
        return None;
    }
    let binary = PathBuf::from(descriptor.get("binary")?.as_str()?);
    binary.is_file().then_some(binary)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A descriptor naming `binary`, of `kind`.
    fn descriptor(kind: &str, binary: &Path) -> String {
        serde_json::json!({ "description": "virtiofsd", "type": kind, "binary": binary }).to_string()
    }

    #[test]
    fn libvirt_finds_the_virtiofsd_its_packages_descriptor_names() {
        // Debian 13's and Fedora's `virtiofsd` package: /usr/libexec/virtiofsd, named by
        // /usr/share/qemu/vhost-user/50-virtiofsd.json. Without it, the VM's start failed with
        // "Unable to find a satisfying virtiofsd".
        let root = tempfile::tempdir().unwrap();
        let (share, etc) = (root.path().join("share"), root.path().join("etc"));
        std::fs::create_dir_all(&share).unwrap();
        let binary = root.path().join("virtiofsd");
        assert_eq!(described_in(&[&share, &etc]), None, "no descriptor: none found");

        std::fs::write(share.join("50-virtiofsd.json"), descriptor("fs", &binary)).unwrap();
        assert_eq!(described_in(&[&share, &etc]), None, "a descriptor whose program is gone");
        std::fs::write(&binary, "").unwrap();
        assert_eq!(described_in(&[&share, &etc]), Some(binary.clone()));

        // An administrator's empty file of the same name hides it, as libvirt reads it.
        std::fs::create_dir_all(&etc).unwrap();
        std::fs::write(etc.join("50-virtiofsd.json"), "").unwrap();
        assert_eq!(described_in(&[&share, &etc]), None);
    }

    #[test]
    fn a_descriptor_of_another_vhost_user_device_isnt_a_virtiofsd() {
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("vhost-user-gpu");
        std::fs::write(&binary, "").unwrap();
        std::fs::write(root.path().join("50-qemu-gpu.json"), descriptor("gpu", &binary)).unwrap();
        std::fs::write(root.path().join("README"), descriptor("fs", &binary)).unwrap();
        assert_eq!(described_in(&[root.path()]), None);
    }

    /// Writes an executable `file`.
    #[cfg(unix)]
    fn executable(file: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::write(file, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn qemu_finds_the_virtiofsd_vagrant_qemu_starts_where_it_looks() {
        // vagrant-qemu's own lookup (`Config#find_virtiofsd`): the PATH, then /usr/libexec (the
        // package's), then /usr/lib/qemu (QEMU's own, before the package). Without one, the VM's
        // start stopped at "virtiofsd was not found or is not executable".
        let root = tempfile::tempdir().unwrap();
        let (bin, libexec, qemu) = (root.path().join("bin"), root.path().join("libexec"), root.path().join("qemu"));
        for d in [&bin, &libexec, &qemu] {
            std::fs::create_dir_all(d).unwrap();
        }
        let mut devices = Devices::under(root.path());
        let fallbacks = [libexec.join("virtiofsd"), qemu.join("virtiofsd")];
        let mut looked_at = |path: &OsStr| {
            devices.virtiofsd = vagrant_qemu_candidates(Some(path), &fallbacks);
            virtiofsd(Provider::Qemu, &devices)
        };
        let path = std::env::join_paths([&bin]).unwrap();
        assert_eq!(looked_at(&path), None, "none anywhere");

        // A file it may not run isn't one.
        std::fs::write(&fallbacks[1], "").unwrap();
        assert_eq!(looked_at(&path), None, "not executable");
        executable(&fallbacks[1]);
        assert_eq!(looked_at(&path), Some(fallbacks[1].clone()), "QEMU's own");
        executable(&fallbacks[0]);
        assert_eq!(looked_at(&path), Some(fallbacks[0].clone()), "the package's first");
        executable(&bin.join("virtiofsd"));
        assert_eq!(looked_at(&path), Some(bin.join("virtiofsd")), "the PATH's first");
        // A folder named virtiofsd isn't one; an empty PATH entry isn't the current folder.
        std::fs::remove_file(bin.join("virtiofsd")).unwrap();
        std::fs::create_dir(bin.join("virtiofsd")).unwrap();
        let with_empty = std::env::join_paths([Path::new(""), &bin]).unwrap();
        assert_eq!(looked_at(&with_empty), Some(fallbacks[0].clone()));
        let no_fallbacks: [PathBuf; 0] = [];
        assert_eq!(vagrant_qemu_candidates(None, &no_fallbacks), Vec::<PathBuf>::new());
    }

    #[test]
    fn each_provider_looks_where_it_starts_its_virtiofsd_from() {
        // libvirt reads its descriptors, never vagrant-qemu's places; QEMU the other way round.
        let root = tempfile::tempdir().unwrap();
        let devices = Devices::under(root.path());
        let binary = root.path().join("usr/libexec/virtiofsd");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, "").unwrap();
        let share = &devices.vhost_user[0];
        std::fs::create_dir_all(share).unwrap();
        std::fs::write(share.join("50-virtiofsd.json"), descriptor("fs", &binary)).unwrap();
        assert_eq!(virtiofsd(Provider::Libvirt, &devices), Some(binary.clone()));
        let only_described = Devices {
            virtiofsd: Vec::new(),
            ..devices.clone()
        };
        assert_eq!(virtiofsd(Provider::Qemu, &only_described), None);
        let only_on_disk = Devices {
            vhost_user: Vec::new(),
            ..devices
        };
        assert_eq!(virtiofsd(Provider::Libvirt, &only_on_disk), None);
        assert_eq!(virtiofsd(Provider::Virtualbox, &only_on_disk), None);
    }

    #[test]
    fn libvirt_and_qemu_on_linux_are_held_to_a_virtiofsd() {
        assert!(needs_virtiofsd(Provider::Libvirt, HostOs::Linux) && needs_virtiofsd(Provider::Qemu, HostOs::Linux));
        for (p, os) in [
            (Provider::Qemu, HostOs::Macos),
            (Provider::Virtualbox, HostOs::Linux),
            (Provider::Libvirt, HostOs::Macos),
        ] {
            assert!(!needs_virtiofsd(p, os), "{p:?} on {os:?}");
        }
    }
}
