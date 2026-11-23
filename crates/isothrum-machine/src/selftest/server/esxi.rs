//! The test VM on an ESXi host, from this computer: a Vagrantfile of the test's own for Vagrant's
//! `vmware_esxi` (the `vagrant-vmware-esxi` plugin, which uploads the box with VMware's OVF Tool),
//! its steps read from Vagrant's and the plugin's lines: the plugin's summary (`--- VMID : …`)
//! once the VM is registered, `VM has been Powered On`, then its address reported by VMware Tools
//! (`state is now "running"`), then `up` succeeding once SSH answers.

use std::path::{Path, PathBuf};

use isothrum_runner::{ErrorReport, Provider, RunOptions, SecretValue, Secrets};

use super::{Course, Failure, Found, Step, TestVm};

/// The box the VM is built from, and its architecture (an ESXi host's CPU).
const BOX: &str = "bento/debian-12";
const BOX_ARCH: &str = "amd64";
/// The secret the Vagrantfile reads the host's password from (the plugin's `env:` form).
const PASSWORD: &str = "ESXI_PASSWORD";
/// The VM's machine name in the run (Vagrant's default).
const MACHINE: &str = "default";

/// An ESXi host, and where on it the test VM goes.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Esxi {
    /// Its name or address.
    pub host: String,
    /// Its SSH port.
    pub port: u16,
    /// Who signs in to it.
    pub username: String,
    /// Their password.
    pub password: SecretValue,
    /// The datastore the VM is uploaded to (the plugin's pick when none).
    pub datastore: Option<String>,
    /// The port group the VM's card goes on (`VM Network` when none).
    pub network: Option<String>,
    /// Vagrant's box store, where the size and architecture of a box already downloaded are read
    /// (`VAGRANT_HOME`'s `boxes`, else `~/.vagrant.d/boxes`, when none).
    pub boxes: Option<PathBuf>,
}

impl Esxi {
    /// Host `host` on SSH port 22, signed in to as `username` with `password`.
    #[must_use]
    pub fn new(host: impl Into<String>, username: impl Into<String>, password: SecretValue) -> Esxi {
        Esxi {
            host: host.into(),
            port: 22,
            username: username.into(),
            password,
            datastore: None,
            network: None,
            boxes: None,
        }
    }

    /// Its SSH port's `host:port`.
    pub(super) fn ssh_address(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// The project: the Vagrantfile of VM `vm`.
    pub(super) fn project(&self, vm: &TestVm<'_>) -> (&'static str, String) {
        let mut provider = vec![
            format!("    v.esxi_hostname = {}", ruby(&self.host)),
            format!("    v.esxi_hostport = {}", self.port),
            format!("    v.esxi_username = {}", ruby(&self.username)),
            format!("    v.esxi_password = \"env:{PASSWORD}\""),
            format!("    v.guest_name = {}", ruby(vm.name)),
            "    v.guest_numvcpus = 1".to_owned(),
            "    v.guest_memsize = 512".to_owned(),
        ];
        if let Some(datastore) = &self.datastore {
            provider.push(format!("    v.esxi_disk_store = {}", ruby(datastore)));
        }
        if let Some(network) = &self.network {
            provider.push(format!("    v.esxi_virtual_network = [{}]", ruby(network)));
        }
        let text = [
            "Vagrant.configure(\"2\") do |config|".to_owned(),
            format!("  config.vm.box = \"{BOX}\""),
            format!("  config.vm.box_architecture = \"{BOX_ARCH}\""),
            format!("  config.vm.hostname = {}", ruby(vm.hostname)),
            "  config.vm.synced_folder \".\", \"/vagrant\", disabled: true".to_owned(),
            "  config.vm.provider \"vmware_esxi\" do |v|".to_owned(),
            provider.join("\n"),
            "  end".to_owned(),
            "end\n".to_owned(),
        ]
        .join("\n");
        ("Vagrantfile", text)
    }

    /// The run's options: the `vmware_esxi` provider, the password as its secret.
    pub(super) fn options(&self) -> RunOptions {
        let mut secrets = Secrets::new();
        secrets.insert(PASSWORD, self.password.clone());
        RunOptions::new().with_provider(Provider::VmwareEsxi).with_secrets(secrets)
    }
}

/// `s` as a Ruby single-quoted string.
fn ruby(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// How the test VM comes up on `e`.
pub(super) fn course(e: &Esxi) -> OnEsxi {
    OnEsxi::new(e.boxes.clone().or_else(vagrant_boxes))
}

/// Vagrant's box store on this computer (`$VAGRANT_HOME/boxes`, else `~/.vagrant.d/boxes`).
fn vagrant_boxes() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let home = var("VAGRANT_HOME").or_else(|| var("HOME").or_else(|| var("USERPROFILE")).map(|h| h.join(".vagrant.d")))?;
    Some(home.join("boxes"))
}

/// The box the VM is built from, as Vagrant's lines name it.
#[derive(Debug, Default)]
struct Named {
    name: Option<String>,
    version: Option<String>,
    arch: Option<String>,
}

/// How the test VM comes up on ESXi, from Vagrant's and its plugin's lines.
#[derive(Debug)]
pub(super) struct OnEsxi {
    /// Vagrant's box store.
    boxes: Option<PathBuf>,
    named: Named,
    /// The size of the box's VMware build in the store, once named and stored: about what the
    /// OVF Tool uploads.
    size: Option<u64>,
    /// The OVF Tool's last error line, which the plugin's own error leaves out.
    ovftool_said: Option<String>,
}

impl OnEsxi {
    fn new(boxes: Option<PathBuf>) -> OnEsxi {
        OnEsxi {
            boxes,
            named: Named::default(),
            size: None,
            ovftool_said: None,
        }
    }

    /// Notes the box `line` names: Vagrant adding it (`Adding box 'NAME' (vVERSION) for provider:
    /// PROVIDER (ARCH)`), or the plugin's summary (`--- Box : NAME`, `--- Box Ver : VERSION`).
    fn note_box(&mut self, line: &str) {
        if let Some((_, rest)) = line.split_once("Adding box '") {
            let Some((name, rest)) = rest.split_once('\'') else { return };
            self.named.name = Some(name.to_owned());
            self.named.version = rest.split_once("(v").and_then(|(_, v)| v.split_once(')')).map(|(v, _)| v.to_owned());
            self.named.arch = rest
                .split_once("for provider:")
                .and_then(|(_, p)| p.rsplit_once('('))
                .and_then(|(_, a)| a.split_once(')'))
                .map(|(a, _)| a.trim().to_owned());
        } else if let Some((key, value)) = summary(line) {
            match key {
                "Box" => self.named.name = Some(value.to_owned()),
                "Box Ver" => self.named.version = Some(value.to_owned()),
                _ => {}
            }
        }
        if self.size.is_none()
            && let (
                Some(boxes),
                Named {
                    name: Some(name),
                    version: Some(version),
                    arch,
                },
            ) = (self.boxes.as_deref(), &self.named)
        {
            self.size = stored_size(boxes, name, version, arch.as_deref());
        }
    }

    /// The box the VM was built from, with its architecture.
    fn box_found(&self) -> Found {
        let Named {
            name: Some(name),
            version,
            arch,
        } = &self.named
        else {
            return Found::Nothing;
        };
        let arch = arch.clone().or_else(|| stored_arch(self.boxes.as_deref()?, name, version.as_deref()?));
        Found::Box {
            name: name.clone(),
            version: version.clone(),
            arch,
        }
    }
}

/// The plugin's summary line `--- KEY : VALUE`, as its key and value.
fn summary(line: &str) -> Option<(&str, &str)> {
    let (_, said) = line.split_once("--- ")?;
    let (key, value) = said.split_once(':')?;
    Some((key.trim(), value.trim()))
}

/// The architectures box `name` at `version` is stored for in Vagrant's box store `boxes`, for a
/// VMware provider, `or`-joined.
fn stored_arch(boxes: &Path, name: &str, version: &str) -> Option<String> {
    let arches = vmware_builds(boxes, name, version);
    (!arches.is_empty()).then(|| arches.into_keys().collect::<Vec<_>>().join(" or "))
}

/// The size of box `name` at `version`'s VMware build for `arch` in `boxes` (the only one stored,
/// when `arch` isn't known); none when it isn't stored.
fn stored_size(boxes: &Path, name: &str, version: &str, arch: Option<&str>) -> Option<u64> {
    let builds = vmware_builds(boxes, name, version);
    let build = match arch {
        Some(arch) => builds.get(arch)?,
        None if builds.len() == 1 => builds.values().next()?,
        None => return None,
    };
    // A box's build is one folder of files (its disks, its `.vmx`).
    let size: u64 = std::fs::read_dir(build)
        .ok()?
        .flatten()
        .filter_map(|f| f.metadata().ok())
        .filter(std::fs::Metadata::is_file)
        .map(|m| m.len())
        .sum();
    (size > 0).then_some(size)
}

/// Box `name` at `version`'s VMware builds in `boxes`: each architecture's folder, by
/// architecture.
fn vmware_builds(boxes: &Path, name: &str, version: &str) -> std::collections::BTreeMap<String, PathBuf> {
    let dir = boxes.join(name.replace('/', "-VAGRANTSLASH-")).join(version);
    let vmware = |arch: &Path| {
        std::fs::read_dir(arch)
            .into_iter()
            .flatten()
            .flatten()
            .map(|p| p.path())
            .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("vmware")))
    };
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|a| Some((a.file_name().to_string_lossy().into_owned(), vmware(&a.path())?)))
        .collect()
}

impl Course for OnEsxi {
    const UP: &'static [Step] = &[Step::Image, Step::Vm, Step::Address, Step::Ssh];

    fn machine(&self) -> &'static str {
        MACHINE
    }

    fn ends(&mut self, line: &str) -> Option<usize> {
        self.note_box(line);
        if line.trim_start().starts_with("Error:") {
            self.ovftool_said = Some(line.trim().to_owned());
        }
        if summary(line).is_some_and(|(key, _)| key == "VMID") {
            Some(0)
        } else if line.contains("VM has been Powered On") {
            Some(1)
        } else if line.contains("state is now \"running\"") {
            Some(2)
        } else {
            None
        }
    }

    fn bytes(&self, at: usize) -> Option<u64> {
        (at == 0).then_some(self.size).flatten()
    }

    fn found(&self, at: usize) -> Found {
        if at == 0 { self.box_found() } else { Found::Nothing }
    }

    fn explain(&self, report: &ErrorReport) -> Option<(usize, Failure)> {
        let raw = &report.message;
        let said = self.ovftool_said.as_deref().unwrap_or_default();
        let boxed = || Box::new(report.clone());
        if said.contains("Duplicate name") || raw.contains("Duplicate name") {
            return Some((0, Failure::NameTaken));
        }
        if raw.contains("OVF Tool error") || raw.contains("ovftool not found") {
            let why = if said.is_empty() { raw.trim() } else { said };
            return Some((0, Failure::Upload(why.to_owned())));
        }
        if raw.contains("box") && (raw.contains("could not be found") || raw.contains("doesn't support the provider") || raw.contains("architecture")) {
            return Some((0, Failure::NoBox(boxed())));
        }
        if raw.contains("Unable to power on VM") {
            return Some((1, Failure::PowerOn(boxed())));
        }
        if raw.contains("timeout waiting for \"running\"") {
            return Some((2, Failure::NoAddress(boxed())));
        }
        if [
            "Timed out while waiting for the machine to boot",
            "SSH authentication failed",
            "The SSH connection was unexpectedly closed",
        ]
        .iter()
        .any(|s| raw.contains(s))
        {
            return Some((3, Failure::NoSsh(boxed())));
        }
        None
    }
}

#[cfg(test)]
mod tests;
