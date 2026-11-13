//! A VM's screen in its hypervisor's own app, beside the embedder's viewer: which app the run's
//! provider has ([`NativeApp`], in a status), and what opens a running VM in it
//! ([`NativeConsole`], [`crate::Running::native_console`]). The runner runs nothing to open it:
//! it hands the embedder a program and its arguments (never a shell line), a macOS app and what
//! it opens, or an `https` URL, each argument checked first.
//!
//! - **VirtualBox**: `VBoxManage startvm <vm> --type separate`, a window on the headless VM.
//! - **Hyper-V**: `vmconnect.exe localhost <vm>`.
//! - **libvirt**: `virt-viewer --connect <uri> <domain>`, else `remote-viewer vnc://…`.
//! - **VMware**: Fusion on a Mac, Workstation elsewhere, on the VM's `.vmx` (Vagrant's record).
//! - **Parallels Desktop**: on the VM's bundle, as `prlctl` says where it is.
//! - **UTM**: its `utm://` link to the VM.
//! - **ESXi**: the Host Client's console of the VM (Vagrant's record of its id).
//! - **Proxmox VE**: its web console of the VM.
//! - **QEMU** (vagrant-qemu) has no app: none is offered.

use std::path::PathBuf;

use isoloom_manifest::{ConsoleSource, Driver, HostOs, Machine, Provider};
use serde::{Deserialize, Serialize};

use super::{no_console, vagrant};
use crate::Result;
use crate::error::Error;
use crate::executor::{Cx, Echo, call};
use crate::process::ProcessSpec;

/// What of a hypervisor's app must be on this computer for a way to open it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "at")]
#[non_exhaustive]
pub enum NativeNeed {
    /// A program, by its name on `PATH` or its full path.
    Program(String),
    /// A macOS app bundle.
    MacApp(PathBuf),
    /// The default browser (a web console).
    Browser,
}

/// The hypervisor's own app that shows a running VM's screen in a window of its own, and what of
/// it this computer must have (any one of `needs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct NativeApp {
    /// Its name as its maker writes it (`VirtualBox`, `VMware Fusion`): what a button names.
    pub name: String,
    /// What opens it, best first: the program or app [`NativeConsole::ways`] runs, in the same
    /// order.
    pub needs: Vec<NativeNeed>,
}

/// One way to open a VM in its hypervisor's app: what to run, never through a shell.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NativeOpen {
    /// Run `program` (a name on `PATH`, or a full path) with `args`, each passed as it is.
    Program {
        /// The program.
        program: String,
        /// Its arguments.
        args: Vec<String>,
    },
    /// Open the macOS app `app` on `args` (`open -a <app> <args>…`: a file or link it opens).
    MacApp {
        /// The app bundle.
        app: PathBuf,
        /// What it opens.
        args: Vec<String>,
    },
    /// Open `url` (`https`) in the default browser.
    Url {
        /// The URL.
        url: String,
    },
}

impl NativeOpen {
    /// What of the app this way needs on this computer.
    #[must_use]
    pub fn need(&self) -> NativeNeed {
        match self {
            NativeOpen::Program { program, .. } => NativeNeed::Program(program.clone()),
            NativeOpen::MacApp { app, .. } => NativeNeed::MacApp(app.clone()),
            NativeOpen::Url { .. } => NativeNeed::Browser,
        }
    }
}

/// What opens a running VM's screen in its hypervisor's own app.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NativeConsole {
    /// The machine.
    pub machine: String,
    /// The app's name ([`NativeApp::name`]).
    pub app: String,
    /// The ways to open it, best first: the first whose program or app this computer has is
    /// the one to take.
    pub ways: Vec<NativeOpen>,
}

/// Where VMware Workstation installs on Windows, newest layout first.
const WORKSTATION_WINDOWS: [&str; 2] = [
    r"C:\Program Files (x86)\VMware\VMware Workstation\vmware.exe",
    r"C:\Program Files\VMware\VMware Workstation\vmware.exe",
];

/// Where the macOS apps install.
const FUSION: &str = "/Applications/VMware Fusion.app";
const PARALLELS: &str = "/Applications/Parallels Desktop.app";
const UTM: &str = "/Applications/UTM.app";

/// The app a run on `provider` (its manifest `manifest`) shows its VMs in on host `host`; none
/// where it has none (vagrant-qemu), or for a run of containers.
pub(crate) fn app(manifest: &isoloom_manifest::Manifest, provider: Option<Provider>, host: HostOs) -> Option<NativeApp> {
    let program = |p: &str| NativeNeed::Program(p.to_owned());
    let mac = |p: &str| NativeNeed::MacApp(PathBuf::from(p));
    let (name, needs) = match (manifest.driver, provider) {
        (Driver::Vagrant, Some(Provider::Virtualbox)) => ("VirtualBox", vec![program("VBoxManage")]),
        (Driver::Vagrant, Some(Provider::Hyperv)) => ("Hyper-V", vec![program("vmconnect.exe")]),
        (Driver::Vagrant, Some(Provider::Libvirt)) => ("virt-viewer", vec![program("virt-viewer"), program("remote-viewer")]),
        (Driver::Vagrant, Some(Provider::VmwareDesktop)) => match host {
            HostOs::Macos => ("VMware Fusion", vec![mac(FUSION)]),
            HostOs::Windows => ("VMware Workstation", WORKSTATION_WINDOWS.iter().map(|p| program(p)).collect()),
            HostOs::Linux => ("VMware Workstation", vec![program("vmware")]),
        },
        (Driver::Vagrant, Some(Provider::Parallels)) => ("Parallels Desktop", vec![mac(PARALLELS)]),
        (Driver::Vagrant, Some(Provider::Utm)) => ("UTM", vec![mac(UTM)]),
        (Driver::Vagrant, Some(Provider::VmwareEsxi)) => ("ESXi", vec![NativeNeed::Browser]),
        (Driver::Terraform, _) if manifest.machines.iter().any(proxmox) => ("Proxmox VE", vec![NativeNeed::Browser]),
        _ => return None,
    };
    Some(NativeApp { name: name.to_owned(), needs })
}

/// Whether `m`'s console is Proxmox VE's.
fn proxmox(m: &Machine) -> bool {
    m.consoles.iter().any(|c| matches!(c.source, Some(ConsoleSource::ProxmoxApi { .. })))
}

/// What opens running machine `machine` in the app the run's provider has, `app` being it: the
/// ways [`NativeApp::needs`] names, with their arguments.
pub(crate) async fn console(cx: &Cx<'_>, machine: &Machine, app: &NativeApp) -> Result<NativeConsole> {
    let provider = cx.scope.provider;
    let ways = match provider {
        _ if cx.manifest().driver == Driver::Terraform => vec![proxmox_url(cx, machine)?],
        Some(Provider::Virtualbox) => {
            let vm = argument(machine, vagrant::vm_name(machine, Provider::Virtualbox)?)?;
            vec![program("VBoxManage", ["startvm", vm, "--type", "separate"])]
        }
        Some(Provider::Hyperv) => vec![program(
            "vmconnect.exe",
            ["localhost", argument(machine, vagrant::vm_name(machine, Provider::Hyperv)?)?],
        )],
        Some(Provider::Libvirt) => libvirt(cx, machine).await?,
        Some(Provider::VmwareDesktop) => {
            let vmx = recorded(cx, machine, "vmware_desktop")?;
            let vmx = argument(machine, &vmx)?.to_owned();
            app.needs
                .iter()
                .filter_map(|need| match need {
                    NativeNeed::MacApp(bundle) => Some(NativeOpen::MacApp {
                        app: bundle.clone(),
                        args: vec![vmx.clone()],
                    }),
                    NativeNeed::Program(p) => Some(program(p, [vmx.as_str()])),
                    NativeNeed::Browser => None,
                })
                .collect()
        }
        Some(Provider::Parallels) => vec![NativeOpen::MacApp {
            app: PathBuf::from(PARALLELS),
            args: vec![parallels_bundle(cx, machine).await?],
        }],
        Some(Provider::Utm) => {
            let vm = argument(machine, vagrant::vm_name(machine, Provider::Utm)?)?;
            vec![NativeOpen::MacApp {
                app: PathBuf::from(UTM),
                args: vec![format!("utm://start?name={}", percent(vm))],
            }]
        }
        Some(Provider::VmwareEsxi) => vec![esxi_url(cx, machine)?],
        Some(Provider::Qemu) | None => return Err(no_app(machine)),
    };
    Ok(NativeConsole {
        machine: machine.name.to_string(),
        app: app.name.clone(),
        ways,
    })
}

/// `program` with `args`.
fn program<'a>(program: &str, args: impl IntoIterator<Item = &'a str>) -> NativeOpen {
    NativeOpen::Program {
        program: program.to_owned(),
        args: args.into_iter().map(str::to_owned).collect(),
    }
}

/// `value` as one argument of a program: refused when it could be read as anything else (empty,
/// an option, a control character, a line break).
fn argument<'a>(machine: &Machine, value: &'a str) -> Result<&'a str> {
    if value.trim().is_empty() || value.starts_with('-') || value.chars().any(char::is_control) {
        return Err(no_console(
            machine,
            format!("its VM's name or file `{}` can't be handed to its app", value.escape_debug()),
        ));
    }
    Ok(value)
}

/// What Vagrant records of `machine`'s VM under `provider_dir` (`<dotfile>/machines/<define>/<provider>/id`):
/// VMware's `.vmx` path, ESXi's VM id, Parallels' VM UUID.
fn recorded(cx: &Cx<'_>, machine: &Machine, provider_dir: &str) -> Result<String> {
    let path = vagrant::dotfile(cx)?
        .join("machines")
        .join(vagrant::define(machine)?)
        .join(provider_dir)
        .join("id");
    vagrant::read_if_there(&path)?
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| no_console(machine, "Vagrant records no VM for it: it isn't up"))
}

/// libvirt's viewers: `virt-viewer` on the domain, over the run's connection; else
/// `remote-viewer` on the display libvirt says, when it says one.
async fn libvirt(cx: &Cx<'_>, machine: &Machine) -> Result<Vec<NativeOpen>> {
    let domain = argument(machine, vagrant::vm_name(machine, Provider::Libvirt)?)?;
    let uri = vagrant::libvirt_uri(cx.host_env());
    let mut ways = vec![program("virt-viewer", ["--connect", argument(machine, &uri)?, domain])];
    if let Ok(super::Endpoint::Loopback(at)) = vagrant::libvirt(cx, machine).await {
        let display = at.port().saturating_sub(5900);
        ways.push(program("remote-viewer", [format!("vnc://{}:{display}", at.ip()).as_str()]));
    }
    Ok(ways)
}

/// The bundle of `machine`'s Parallels VM: where `prlctl list --info --json <uuid>` says it is
/// (`Home`).
async fn parallels_bundle(cx: &Cx<'_>, machine: &Machine) -> Result<String> {
    let uuid = recorded(cx, machine, "parallels")?;
    let uuid = argument(machine, &uuid)?;
    let spec = ProcessSpec::new("prlctl", ["list", "--info", "--json", uuid]);
    let out = call(cx, "console", &spec, std::time::Duration::from_secs(30), Echo::Quiet).await?;
    if !out.success() {
        return Err(no_console(machine, format!("Parallels didn't say where its VM is: {}", out.reason())));
    }
    let said: serde_json::Value = serde_json::from_str(&out.stdout.join("\n")).unwrap_or_default();
    let home = said
        .as_array()
        .and_then(|vms| vms.first())
        .and_then(|vm| vm.get("Home"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| no_console(machine, "Parallels didn't say where its VM is"))?;
    Ok(argument(machine, home.trim_end_matches('/'))?.to_owned())
}

/// The ESXi Host Client's console of `machine`'s VM: `https://<host>/ui/#/console/<id>`.
fn esxi_url(cx: &Cx<'_>, machine: &Machine) -> Result<NativeOpen> {
    let host = cx
        .vars
        .params()
        .get(isoloom_manifest::contract::ESXI_HOSTNAME)
        .filter(|h| host_name(h))
        .ok_or_else(|| no_console(machine, "the run names no ESXi host"))?;
    let id = recorded(cx, machine, "vmware_esxi")?;
    if !id.chars().all(|c| c.is_ascii_digit()) {
        return Err(no_console(machine, format!("its ESXi VM id `{}` isn't one", id.escape_debug())));
    }
    Ok(NativeOpen::Url {
        url: format!("https://{host}/ui/#/console/{id}"),
    })
}

/// Proxmox VE's web console of `machine`'s VM: `<endpoint>?console=kvm&novnc=1&vmid=<id>&node=<node>`.
fn proxmox_url(cx: &Cx<'_>, machine: &Machine) -> Result<NativeOpen> {
    let Some(ConsoleSource::ProxmoxApi { endpoint, node, vm_id, .. }) = machine
        .consoles
        .iter()
        .find_map(|c| c.source.as_ref().filter(|s| matches!(s, ConsoleSource::ProxmoxApi { .. })))
    else {
        return Err(no_console(machine, "it isn't a Proxmox VE VM"));
    };
    let at = |field: &str| format!("machines.{}.consoles.source.{field}", machine.name);
    let endpoint = cx.vars.render(endpoint, &at("endpoint"))?;
    let node = cx.vars.render(node, &at("node"))?;
    let vm_id = cx.vars.render(vm_id, &at("vm_id"))?;
    let base = endpoint.trim_end_matches('/');
    let server = base.strip_prefix("https://").filter(|s| host_name(s.split(':').next().unwrap_or_default()));
    let port_ok = base
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once(':'))
        .is_none_or(|(_, p)| p.parse::<u16>().is_ok());
    if server.is_none() || !port_ok {
        return Err(no_console(
            machine,
            format!("its Proxmox VE address `{}` isn't an https server", endpoint.escape_debug()),
        ));
    }
    if !vm_id.chars().all(|c| c.is_ascii_digit()) || vm_id.is_empty() || !host_name(&node) {
        return Err(no_console(machine, "its Proxmox VE node or VM id isn't one"));
    }
    Ok(NativeOpen::Url {
        url: format!("{base}/?console=kvm&novnc=1&vmid={vm_id}&node={node}"),
    })
}

/// Whether `h` is a host name or an IPv4 address, nothing else (no user, path or query).
fn host_name(h: &str) -> bool {
    !h.is_empty() && h.len() <= 253 && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') && !h.starts_with('-')
}

/// `s` percent-encoded for a URL's query value: unreserved characters as they are.
fn percent(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => char::from(b).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// The error a machine whose run has no app gets.
pub(crate) fn no_app(machine: &Machine) -> Error {
    no_console(machine, "its hypervisor has no app of its own to show it in")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_argument_that_could_be_read_as_more_is_refused() {
        let m: Machine = serde_json::from_value(serde_json::json!({ "name": "web", "role": "member", "addresses": {} })).unwrap();
        assert_eq!(argument(&m, "web-vm - web").unwrap(), "web-vm - web");
        for bad in ["", "  ", "--type=gui", "a\nb", "a\u{0}b"] {
            assert!(argument(&m, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_host_name_is_only_a_name() {
        assert!(host_name("172.16.1.20") && host_name("pve.lab.example"));
        for bad in ["", "-x", "a b", "user@host", "host/path", "host?q", "host#x", "h:1"] {
            assert!(!host_name(bad), "{bad}");
        }
    }

    #[test]
    fn a_name_in_a_link_is_percent_encoded() {
        assert_eq!(percent("web-vm - web"), "web-vm%20-%20web");
        assert_eq!(percent("a&b=c"), "a%26b%3Dc");
    }
}
