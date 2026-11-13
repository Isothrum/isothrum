//! A Vagrant VM's console where its hypervisor keeps it: libvirt's display, QEMU's monitor,
//! VirtualBox's remote display. Each is read where the provider keeps it, never guessed.

use std::path::PathBuf;
use std::time::Duration;

use isoloom_manifest::{Identity, Machine, Native, Provider};

use super::{Endpoint, no_console, on_loopback};
use crate::Result;
use crate::executor::{Cx, Echo, call};
use crate::host_env::{HostEnv, LIBVIRT_DEFAULT_URI};
use crate::process::ProcessSpec;
use crate::tool_output::qemu::recorded_socket;
#[cfg(unix)]
use crate::tool_output::qemu::vnc_server;
use crate::tool_output::vboxmanage::{Vrde, remote_display};
use crate::tool_output::virsh::vnc_display;

/// How long a read of the hypervisor may go without progress.
const READ_LIMIT: Duration = Duration::from_secs(30);

/// The step id the reads report under.
const STEP: &str = "console";

/// The connection vagrant-libvirt uses when nothing names another.
const LIBVIRT_URI: &str = "qemu:///system";

/// `machine`'s VM name under `provider`, as the manifest records it.
pub(super) fn vm_name(machine: &Machine, provider: Provider) -> Result<&str> {
    match &machine.native {
        Some(Native::Vagrant { vm_names, .. }) => vm_names
            .get(&provider)
            .map(String::as_str)
            .ok_or_else(|| no_console(machine, format!("the manifest names no {} VM for it", provider.id()))),
        _ => Err(no_console(machine, "it isn't a Vagrant VM")),
    }
}

/// libvirt's display of `machine`'s domain: `virsh domdisplay`, on the connection
/// `LIBVIRT_DEFAULT_URI` names (which `virsh` inherits and reads itself, UTF-8 or not), else
/// vagrant-libvirt's default.
pub(super) async fn libvirt(cx: &Cx<'_>, machine: &Machine) -> Result<Endpoint> {
    let domain = vm_name(machine, Provider::Libvirt)?;
    let spec = virsh_domdisplay(cx.host_env(), domain);
    let out = call(cx, STEP, &spec, READ_LIMIT, Echo::Quiet).await?;
    if !out.success() {
        return Err(no_console(machine, format!("libvirt didn't say where its display is: {}", out.reason())));
    }
    let said = out.stdout.join("\n");
    match vnc_display(&said) {
        Some(at) => on_loopback(machine, at),
        None => Err(no_console(
            machine,
            format!("libvirt gives it no VNC display (`virsh domdisplay` said `{}`)", said.trim()),
        )),
    }
}

/// `virsh domdisplay <domain>`, on vagrant-libvirt's default connection unless `env` sets
/// `LIBVIRT_DEFAULT_URI`.
fn virsh_domdisplay(env: &HostEnv, domain: &str) -> ProcessSpec {
    if env.get(LIBVIRT_DEFAULT_URI).is_some_and(|u| !u.is_empty()) {
        ProcessSpec::new("virsh", ["domdisplay", domain])
    } else {
        ProcessSpec::new("virsh", ["-c", LIBVIRT_URI, "domdisplay", domain])
    }
}

/// The libvirt connection the run's Vagrant uses: `LIBVIRT_DEFAULT_URI` as `env` sets it, else
/// vagrant-libvirt's default.
pub(super) fn libvirt_uri(env: &HostEnv) -> String {
    env.get(LIBVIRT_DEFAULT_URI)
        .filter(|u| !u.is_empty())
        .map_or_else(|| LIBVIRT_URI.to_owned(), |u| u.to_string_lossy().into_owned())
}

/// The run's Vagrant state folder (`identity.vagrant.dotfile`), as its driver renders it.
pub(super) fn dotfile(cx: &Cx<'_>) -> Result<PathBuf> {
    match &cx.manifest().identity {
        Identity::Vagrant(v) => match &v.dotfile {
            Some(t) => Ok(cx.run.dir().join(cx.vars.render(t.template(), "identity.vagrant.dotfile")?)),
            None => Ok(cx.run.dir().join(".vagrant")),
        },
        _ => Ok(cx.run.dir().join(".vagrant")),
    }
}

/// The define name of `machine`.
pub(super) fn define(machine: &Machine) -> Result<&str> {
    match &machine.native {
        Some(Native::Vagrant { vagrant, .. }) => Ok(vagrant.as_str()),
        _ => Err(no_console(machine, "it isn't a Vagrant VM")),
    }
}

/// The monitor socket of `machine`'s QEMU: the one vagrant-qemu recorded in its run folder
/// (`<dotfile>/machines/<define>/qemu/<id>/run/options.yml`, `:monitor_socket:`), else its
/// default under Vagrant's own folder (`$VAGRANT_HOME/tmp/vagrant-qemu/<id>/qemu_socket`).
fn monitor_socket(cx: &Cx<'_>, machine: &Machine) -> Result<PathBuf> {
    let qemu_dir = dotfile(cx)?.join("machines").join(define(machine)?).join("qemu");
    let id = read_if_there(&qemu_dir.join("id"))?.map(|s| s.trim().to_owned()).unwrap_or_default();
    if id.is_empty() {
        return Err(no_console(machine, "Vagrant records no QEMU VM for it: it isn't up"));
    }
    let options = read_if_there(&qemu_dir.join(&id).join("run").join("options.yml"))?;
    if let Some(recorded) = options.as_deref().and_then(recorded_socket) {
        return Ok(recorded);
    }
    let home = vagrant_home(cx.host_env()).ok_or_else(|| no_console(machine, "no Vagrant home to find its QEMU monitor in"))?;
    Ok(home.join("tmp").join("vagrant-qemu").join(id).join("qemu_socket"))
}

/// Vagrant's own folder, as the runner's environment says: `VAGRANT_HOME`, else `~/.vagrant.d`.
fn vagrant_home(env: &HostEnv) -> Option<PathBuf> {
    let set = |name: &str| env.get(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    set("VAGRANT_HOME").or_else(|| set("HOME").map(|h| h.join(".vagrant.d")))
}

/// The text of `path`; none when there is no file there, an error naming it when it can't be read.
pub(super) fn read_if_there(path: &std::path::Path) -> Result<Option<String>> {
    crate::store::if_there(std::fs::read_to_string(path)).map_err(crate::error::Error::io(path, "read"))
}

/// QEMU's VNC display of `machine`: `info vnc` asked of the VM's monitor.
#[cfg(unix)]
pub(super) async fn qemu(cx: &Cx<'_>, machine: &Machine) -> Result<Endpoint> {
    let socket = monitor_socket(cx, machine)?;
    let said = ask_monitor(&socket, "info vnc")
        .await
        .map_err(|e| no_console(machine, format!("its QEMU monitor didn't answer ({}): {e}", socket.display())))?;
    match vnc_server(&said) {
        Some(at) => on_loopback(machine, at),
        None => Err(no_console(
            machine,
            "its VM was started without a screen (its QEMU has no VNC display): restart it to get one",
        )),
    }
}

/// QEMU's VNC display of `machine`: none on a host without Unix sockets, where vagrant-qemu's
/// monitor is out of reach. Ready at once, so its callers await it as on Unix.
#[cfg(not(unix))]
pub(super) fn qemu(cx: &Cx<'_>, machine: &Machine) -> std::future::Ready<Result<Endpoint>> {
    let _ = (monitor_socket, cx);
    std::future::ready(Err(no_console(
        machine,
        "vagrant-qemu's monitor is a Unix socket, which this host doesn't have",
    )))
}

/// What QEMU's human monitor at `socket` answers `command`: its text up to the next prompt.
#[cfg(unix)]
pub(super) async fn ask_monitor(socket: &std::path::Path, command: &str) -> std::io::Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    const PROMPT: &str = "(qemu) ";
    let talk = async {
        let mut stream = tokio::net::UnixStream::connect(socket).await?;
        let mut said = Vec::new();
        let mut buf = [0u8; 4096];
        // The banner, up to the first prompt.
        while !String::from_utf8_lossy(&said).contains(PROMPT) {
            let n = stream.read(&mut buf).await?;
            if n == 0 {
                return Err(std::io::Error::other("the monitor closed before its prompt"));
            }
            said.extend(buf.iter().take(n));
        }
        stream.write_all(format!("{command}\n").as_bytes()).await?;
        let mut answer = Vec::new();
        while !String::from_utf8_lossy(&answer).contains(PROMPT) {
            let n = stream.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            answer.extend(buf.iter().take(n));
        }
        Ok(String::from_utf8_lossy(&answer).into_owned())
    };
    tokio::time::timeout(Duration::from_secs(10), talk)
        .await
        .unwrap_or_else(|_| Err(std::io::Error::other("no answer in 10 seconds")))
}

/// VirtualBox's remote display of `machine`: on, and its port, from `VBoxManage showvminfo`.
pub(super) async fn vrde(cx: &Cx<'_>, machine: &Machine) -> Result<Endpoint> {
    let name = vm_name(machine, Provider::Virtualbox)?;
    let spec = ProcessSpec::new("VBoxManage", ["showvminfo", name, "--machinereadable"]);
    let out = call(cx, STEP, &spec, READ_LIMIT, Echo::Quiet).await?;
    if !out.success() {
        return Err(no_console(machine, format!("VirtualBox didn't say how its VM is: {}", out.reason())));
    }
    match remote_display(&out.stdout) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libvirt_and_vagrant_are_found_where_the_runners_environment_says() {
        // Regression: read from the process's environment mid-operation, a non-UTF-8 value as unset.
        assert_eq!(virsh_domdisplay(&HostEnv::default(), "d").args, ["-c", "qemu:///system", "domdisplay", "d"]);
        let session = HostEnv::of([(LIBVIRT_DEFAULT_URI, "qemu:///session")]);
        assert_eq!(virsh_domdisplay(&session, "d").args, ["domdisplay", "d"], "virsh reads it itself");
        assert_eq!(vagrant_home(&HostEnv::default()), None);
        let home = HostEnv::of([("HOME", "/home/u")]);
        assert_eq!(vagrant_home(&home), Some(PathBuf::from("/home/u/.vagrant.d")));
        let both = HostEnv::of([("HOME", "/home/u"), ("VAGRANT_HOME", "/v")]);
        assert_eq!(vagrant_home(&both), Some(PathBuf::from("/v")));
    }

    #[test]
    fn a_vagrant_qemu_file_that_cant_be_read_is_an_error_not_a_vm_that_isnt_up() {
        let dir = tempfile::tempdir().unwrap();
        let id = dir.path().join("id");
        assert_eq!(read_if_there(&id).unwrap(), None, "no file: nothing recorded");
        // A folder where the file goes: there, but unreadable as text.
        std::fs::create_dir(&id).unwrap();
        let err = read_if_there(&id).unwrap_err();
        assert_eq!(err.code(), crate::ErrorCode::Io, "{err}");
        assert!(err.to_string().starts_with(&format!("{} couldn't be read: ", id.display())), "{err}");
    }

    /// A fake QEMU monitor on a real Unix socket: its banner, the command echoed, its answer.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_monitor_is_asked_after_its_banner() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("m.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let monitor = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            s.write_all(b"QEMU 10.1.0 monitor - type 'help' for more information\r\n(qemu) ").await.unwrap();
            let mut buf = [0u8; 64];
            let n = s.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"info vnc\n");
            s.write_all(b"info vnc\r\ndefault:\r\n  Server: 127.0.0.1:5902 (ipv4)\r\n(qemu) ")
                .await
                .unwrap();
        });
        let said = ask_monitor(&socket, "info vnc").await.unwrap();
        assert_eq!(vnc_server(&said), Some("127.0.0.1:5902".parse().unwrap()));
        monitor.await.unwrap();
    }
}
