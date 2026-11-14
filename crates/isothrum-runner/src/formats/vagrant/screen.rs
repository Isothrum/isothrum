//! A Vagrant VM's screen, found where its hypervisor keeps it, by the VM's own id as Vagrant
//! records it in the run's dotfile (`machines/<machine>/<provider>/id`), never by a name another
//! copy of the project could share.
//!
//! | Provider | Graphical console | Its own app |
//! |---|---|---|
//! | VirtualBox | its remote display (RDP), `VBoxManage showvminfo` | `VBoxManage startvm --type separate` |
//! | libvirt | `virsh domdisplay` (VNC) | `virt-viewer`, else `remote-viewer` |
//! | QEMU | `info vnc` on vagrant-qemu's monitor socket | none |
//! | Hyper-V | none | `vmconnect.exe` |
//! | VMware Desktop | none | Fusion, Workstation, on the VM's `.vmx` |
//! | Parallels | none | Parallels Desktop, on the bundle `prlctl` names |
//! | UTM | none | UTM's `utm://` link, by the name `utmctl` gives |
//! | ESXi | none | none (the run doesn't say which host) |

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::dotfile::Record;
use super::{call, dot, providers};
use crate::Result;
use crate::cli::virsh::Display;
use crate::cli::{prlctl, utmctl, vboxmanage, virsh};
use crate::formats::{Seen, SeenMachine, ToolCx};
use crate::graphical::native::{argument, needs_program, no_app, program};
use crate::graphical::{Console, ConsoleOffer, ConsoleProtocol, Endpoint, NativeApp, NativeConsole, NativeNeed, NativeOpen, no_console, on_loopback, why_not};
use crate::host_env::LIBVIRT_DEFAULT_URI;
use crate::project::platform::{HostOs, Provider};
use crate::tool_output::vboxmanage::Vrde;

/// How long a read of the hypervisor may take.
const READ_LIMIT: Duration = Duration::from_secs(30);

/// The connection vagrant-libvirt uses when nothing names another.
pub(super) const LIBVIRT_URI: &str = "qemu:///system";

/// What VirtualBox's remote display needs.
const VRDE_NOTE: &str = "VirtualBox's remote display needs the Oracle VM VirtualBox Extension Pack, installed before the VM boots";

/// Where VMware Workstation installs on Windows, newest layout first.
const WORKSTATION_WINDOWS: [&str; 2] = [
    r"C:\Program Files (x86)\VMware\VMware Workstation\vmware.exe",
    r"C:\Program Files\VMware\VMware Workstation\vmware.exe",
];

/// Where the macOS apps install.
const FUSION: &str = "/Applications/VMware Fusion.app";
const PARALLELS: &str = "/Applications/Parallels Desktop.app";
const UTM: &str = "/Applications/UTM.app";

/// The provider Vagrant names `id`.
fn provider_of(id: &str) -> Option<Provider> {
    providers::ALL.into_iter().find(|p| p.id() == id)
}

/// What Vagrant records of `machine`'s VM: on the provider asked for, else the one it was made
/// on; refused when it records none (the machine isn't up).
fn recorded(cx: &ToolCx<'_>, machine: &str) -> Result<(Provider, String)> {
    let records: Vec<Record> = dot(cx).recorded()?.into_iter().filter(|r| r.machine == machine).collect();
    let asked = cx.provider.map(Provider::id);
    let record = records
        .iter()
        .find(|r| Some(r.provider.as_str()) == asked)
        .or_else(|| records.first())
        .ok_or_else(|| no_console(machine, "Vagrant records no VM for it: it isn't up"))?;
    let provider =
        provider_of(&record.provider).ok_or_else(|| no_console(machine, format!("its provider `{}` isn't one this runner knows", record.provider)))?;
    Ok((provider, record.id.clone()))
}

/// What a status says of `machine`'s console: by the provider its VM was made on, with why it
/// can't be shown now when it runs (VirtualBox, libvirt, QEMU are asked); none for a machine not
/// made.
pub(super) async fn offer(cx: &ToolCx<'_>, machine: &SeenMachine) -> Option<ConsoleOffer> {
    let provider = provider_of(machine.kind.as_deref()?)?;
    let runs = machine.state.runs();
    let offer = match provider {
        Provider::Virtualbox => {
            let offer = ConsoleOffer::of(ConsoleProtocol::Rdp, Some(VRDE_NOTE));
            if runs {
                offer.unavailable(why_not(vrde(cx, &machine.name).await))
            } else {
                offer
            }
        }
        Provider::Libvirt | Provider::Qemu => {
            let offer = ConsoleOffer::of(ConsoleProtocol::Vnc, None);
            if runs {
                offer.unavailable(why_not(local(cx, &machine.name, provider).await))
            } else {
                offer
            }
        }
        other => match app_of(other, providers::this_host()) {
            Some(app) => ConsoleOffer::none(&format!("its hypervisor gives no screen this runner reaches: open it in {}", app.name)),
            None => ConsoleOffer::none("its hypervisor gives no screen this runner reaches"),
        },
    };
    Some(offer)
}

/// Where running `machine`'s console answers, as its hypervisor says.
pub(super) async fn reach(cx: &ToolCx<'_>, machine: &str) -> Result<Console> {
    let (provider, _) = recorded(cx, machine)?;
    match provider {
        Provider::Virtualbox => Ok(Console::new(machine, ConsoleProtocol::Rdp, Endpoint::Loopback(vrde(cx, machine).await?)).with_note(Some(VRDE_NOTE))),
        Provider::Libvirt | Provider::Qemu => Ok(Console::new(
            machine,
            ConsoleProtocol::Vnc,
            Endpoint::Loopback(local(cx, machine, provider).await?),
        )),
        other => Err(no_console(
            machine,
            match app_of(other, providers::this_host()) {
                Some(app) => format!("its hypervisor gives no screen this runner reaches: open it in {}", app.name),
                None => "its hypervisor gives no screen this runner reaches".to_owned(),
            },
        )),
    }
}

/// The VNC display of a libvirt or QEMU VM.
async fn local(cx: &ToolCx<'_>, machine: &str, provider: Provider) -> Result<std::net::SocketAddr> {
    if provider == Provider::Libvirt {
        libvirt(cx, machine).await
    } else {
        qemu(cx, machine).await
    }
}

/// VirtualBox's remote display of `machine`: on, and on this computer's loopback.
async fn vrde(cx: &ToolCx<'_>, machine: &str) -> Result<std::net::SocketAddr> {
    let (_, uuid) = recorded(cx, machine)?;
    let said = vboxmanage::remote_display(&call(cx, READ_LIMIT), &uuid)
        .await
        .map_err(|e| no_console(machine, format!("VirtualBox didn't say how its VM is: {e}")))?;
    match said {
        Vrde::Off => Err(no_console(
            machine,
            "VirtualBox's remote display is off: it needs the Oracle VM VirtualBox Extension Pack, installed before the VM boots (then restart the VM)",
        )),
        Vrde::NotListening => Err(no_console(machine, "VirtualBox's remote display isn't listening: the VM isn't running")),
        Vrde::Unreadable(address) => Err(no_console(
            machine,
            format!("VirtualBox's remote display address `{address}` isn't an IP address"),
        )),
        Vrde::At(at) => on_loopback(machine, at),
    }
}

/// The libvirt connection the run's Vagrant uses: none when `LIBVIRT_DEFAULT_URI` names it
/// (`virsh` reads it itself), else vagrant-libvirt's default.
fn libvirt_uri(cx: &ToolCx<'_>) -> Option<&'static str> {
    cx.var(LIBVIRT_DEFAULT_URI).is_none().then_some(LIBVIRT_URI)
}

/// libvirt's VNC display of `machine`'s domain.
async fn libvirt(cx: &ToolCx<'_>, machine: &str) -> Result<std::net::SocketAddr> {
    let (_, domain) = recorded(cx, machine)?;
    let said = virsh::domdisplay(&call(cx, READ_LIMIT), libvirt_uri(cx), &domain)
        .await
        .map_err(|e| no_console(machine, format!("libvirt didn't say where its display is: {e}")))?;
    match said {
        Display::Vnc(at) => on_loopback(machine, at),
        Display::Other(said) => Err(no_console(
            machine,
            format!("libvirt gives it no VNC display (`virsh domdisplay` said `{said}`)"),
        )),
    }
}

/// The text of `path`; none when there is no file there.
fn read_if_there(path: &Path) -> Result<Option<String>> {
    crate::store::if_there(std::fs::read_to_string(path)).map_err(crate::error::Error::io(path, "read"))
}

/// The monitor socket of `machine`'s QEMU: the one vagrant-qemu recorded in its run folder
/// (`<dotfile>/machines/<machine>/qemu/<id>/run/options.yml`, `:monitor_socket:`), else its
/// default under Vagrant's own folder (`$VAGRANT_HOME/tmp/vagrant-qemu/<id>/qemu_socket`).
fn monitor_socket(cx: &ToolCx<'_>, machine: &str) -> Result<PathBuf> {
    let (_, id) = recorded(cx, machine)?;
    let options = read_if_there(
        &dot(cx)
            .path
            .join("machines")
            .join(machine)
            .join("qemu")
            .join(&id)
            .join("run")
            .join("options.yml"),
    )?;
    if let Some(socket) = options.as_deref().and_then(crate::tool_output::qemu::recorded_socket) {
        return Ok(socket);
    }
    let home = cx
        .var("VAGRANT_HOME")
        .map(PathBuf::from)
        .or_else(|| cx.var("HOME").map(|h| PathBuf::from(h).join(".vagrant.d")))
        .ok_or_else(|| no_console(machine, "no Vagrant home to find its QEMU monitor in"))?;
    Ok(home.join("tmp").join("vagrant-qemu").join(id).join("qemu_socket"))
}

/// QEMU's VNC display of `machine`, asked of its monitor.
#[cfg(unix)]
async fn qemu(cx: &ToolCx<'_>, machine: &str) -> Result<std::net::SocketAddr> {
    let socket = monitor_socket(cx, machine)?;
    match crate::cli::qemu::vnc_server(&socket).await {
        Ok(Some(at)) => on_loopback(machine, at),
        Ok(None) => Err(no_console(
            machine,
            "its VM was started without a screen (its QEMU has no VNC display): restart it to get one",
        )),
        Err(e) => Err(no_console(machine, format!("its QEMU monitor didn't answer ({}): {e}", socket.display()))),
    }
}

/// QEMU's VNC display of `machine`: none on a host without Unix sockets, where vagrant-qemu's
/// monitor is out of reach.
#[cfg(not(unix))]
fn qemu(cx: &ToolCx<'_>, machine: &str) -> impl Future<Output = Result<std::net::SocketAddr>> {
    let answer = monitor_socket(cx, machine).and_then(|_| Err(no_console(machine, "vagrant-qemu's monitor is a Unix socket, which this host doesn't have")));
    std::future::ready(answer)
}

/// The app VMs of `provider` show in on `host`; none where it has none.
fn app_of(provider: Provider, host: HostOs) -> Option<NativeApp> {
    let mac = |p: &str| NativeNeed::MacApp(PathBuf::from(p));
    let (name, needs) = match provider {
        Provider::Virtualbox => ("VirtualBox", vec![needs_program("VBoxManage")]),
        Provider::Hyperv => ("Hyper-V", vec![needs_program("vmconnect.exe")]),
        Provider::Libvirt => ("virt-viewer", vec![needs_program("virt-viewer"), needs_program("remote-viewer")]),
        Provider::VmwareDesktop => match host {
            HostOs::Macos => ("VMware Fusion", vec![mac(FUSION)]),
            HostOs::Windows => ("VMware Workstation", WORKSTATION_WINDOWS.iter().map(|p| needs_program(p)).collect()),
            HostOs::Linux => ("VMware Workstation", vec![needs_program("vmware")]),
        },
        Provider::Parallels => ("Parallels Desktop", vec![mac(PARALLELS)]),
        Provider::Utm => ("UTM", vec![mac(UTM)]),
        Provider::Qemu | Provider::VmwareEsxi => return None,
    };
    Some(NativeApp::new(name, needs))
}

/// The app the run's VMs show in: their provider's, as the dotfile records it (`seen`), else the
/// one asked for.
pub(super) fn app(cx: &ToolCx<'_>, seen: &Seen) -> Option<NativeApp> {
    let provider = seen.machines.iter().find_map(|m| m.kind.as_deref().and_then(provider_of)).or(cx.provider)?;
    app_of(provider, providers::this_host())
}

/// What opens running `machine` in its provider's app.
pub(super) async fn native(cx: &ToolCx<'_>, machine: &str) -> Result<NativeConsole> {
    let (provider, id) = recorded(cx, machine)?;
    let app = app_of(provider, providers::this_host()).ok_or_else(|| no_app(machine))?;
    let id = argument(machine, &id)?;
    let read = call(cx, READ_LIMIT);
    let ways = match provider {
        Provider::Virtualbox => vec![program("VBoxManage", ["startvm", id, "--type", "separate"])],
        Provider::Hyperv => vec![program("vmconnect.exe", ["localhost", "-G", id])],
        Provider::Libvirt => {
            let uri = cx.var(LIBVIRT_DEFAULT_URI).unwrap_or_else(|| LIBVIRT_URI.to_owned());
            let mut ways = vec![program("virt-viewer", ["--connect", argument(machine, &uri)?, id])];
            if let Ok(at) = libvirt(cx, machine).await
                && let Some(display) = crate::tool_output::virsh::vnc_display_number(at.port())
            {
                ways.push(program("remote-viewer", [format!("vnc://{}:{display}", at.ip()).as_str()]));
            }
            ways
        }
        Provider::VmwareDesktop => app
            .needs
            .iter()
            .filter_map(|need| match need {
                NativeNeed::MacApp(bundle) => Some(NativeOpen::MacApp {
                    app: bundle.clone(),
                    args: vec![id.to_owned()],
                }),
                NativeNeed::Program(p) => Some(program(p, [id])),
                NativeNeed::Browser => None,
            })
            .collect(),
        Provider::Parallels => {
            let home = prlctl::home(&read, id)
                .await
                .map_err(|e| no_console(machine, format!("Parallels didn't say where its VM is: {e}")))?;
            vec![NativeOpen::MacApp {
                app: PathBuf::from(PARALLELS),
                args: vec![argument(machine, &home)?.to_owned()],
            }]
        }
        Provider::Utm => {
            let name = utmctl::name_of(&read, id)
                .await
                .map_err(|e| no_console(machine, format!("UTM didn't list its VMs: {e}")))?
                .ok_or_else(|| no_console(machine, "UTM lists no VM with the id Vagrant records"))?;
            vec![NativeOpen::MacApp {
                app: PathBuf::from(UTM),
                args: vec![format!("utm://start?name={}", crate::encoding::percent_encoded(argument(machine, &name)?))],
            }]
        }
        Provider::Qemu | Provider::VmwareEsxi => return Err(no_app(machine)),
    };
    Ok(NativeConsole::new(machine, &app, ways))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_provider_has_its_own_app_or_none() {
        let name = |p: Provider, h: HostOs| app_of(p, h).map(|a| a.name);
        assert_eq!(name(Provider::Virtualbox, HostOs::Linux).as_deref(), Some("VirtualBox"));
        assert_eq!(name(Provider::VmwareDesktop, HostOs::Macos).as_deref(), Some("VMware Fusion"));
        assert_eq!(name(Provider::VmwareDesktop, HostOs::Windows).as_deref(), Some("VMware Workstation"));
        assert_eq!(name(Provider::Qemu, HostOs::Linux), None);
        assert_eq!(name(Provider::VmwareEsxi, HostOs::Windows), None);
        assert_eq!(provider_of("vmware_desktop"), Some(Provider::VmwareDesktop));
        assert_eq!(provider_of("docker"), None);
    }
}
