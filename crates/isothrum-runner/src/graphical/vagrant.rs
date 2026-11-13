//! A Vagrant VM's console where its hypervisor keeps it: libvirt's display, QEMU's monitor,
//! VirtualBox's remote display. Each is read where the provider keeps it, never guessed.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use isoloom_manifest::{Identity, Machine, Native, Provider};

use super::{Endpoint, no_console, on_loopback};
use crate::Result;
use crate::executor::{Cx, Echo, call};
use crate::host_env::{HostEnv, LIBVIRT_DEFAULT_URI};
use crate::process::ProcessSpec;

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

/// The address of a `vnc://<host>:<display>` line `virsh domdisplay` prints (port 5900 plus the
/// display number), a bare IPv6 host in brackets.
pub(super) fn vnc_display(said: &str) -> Option<SocketAddr> {
    let rest = said.lines().map(str::trim).find_map(|l| l.strip_prefix("vnc://"))?;
    let rest = rest.split(['/', '?']).next()?;
    let (host, display) = rest.rsplit_once(':')?;
    let display: u16 = display.parse().ok()?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let ip = if host.eq_ignore_ascii_case("localhost") {
        super::LOOPBACK
    } else {
        host.parse().ok()?
    };
    Some(SocketAddr::new(ip, 5900u16.checked_add(display)?))
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
    let data = dotfile(cx)?.join("machines").join(define(machine)?).join("qemu");
    let id = read_if_there(&data.join("id"))?.map(|s| s.trim().to_owned()).unwrap_or_default();
    if id.is_empty() {
        return Err(no_console(machine, "Vagrant records no QEMU VM for it: it isn't up"));
    }
    let options = read_if_there(&data.join(&id).join("run").join("options.yml"))?;
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

/// The `:monitor_socket:` vagrant-qemu's `options.yml` records.
pub(super) fn recorded_socket(options: &str) -> Option<PathBuf> {
    options.lines().find_map(|l| {
        let path = l.trim().strip_prefix(":monitor_socket:")?.trim().trim_matches(['"', '\'']);
        (!path.is_empty() && path != "~").then(|| PathBuf::from(path))
    })
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

/// The address `info vnc` names its server at (`Server: 127.0.0.1:5900 (ipv4)`, or an older
/// QEMU's `address: 127.0.0.1:5900`).
#[cfg(any(unix, test))]
pub(super) fn vnc_server(said: &str) -> Option<SocketAddr> {
    said.lines().find_map(|l| {
        let l = l.trim();
        let rest = l.strip_prefix("Server:").or_else(|| l.strip_prefix("address:"))?.trim();
        rest.split_whitespace().next()?.parse().ok()
    })
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

/// What `showvminfo --machinereadable` says of the remote display.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Vrde {
    /// Off (no extension pack when the VM booted).
    Off,
    /// On, but bound to no port (the VM is off).
    NotListening,
    /// Listening at an address that isn't an IP address, as said.
    Unreadable(String),
    /// Listening there.
    At(SocketAddr),
}

/// Reads `vrde="on"`, `vrdeport=<port in use>` and `vrdeaddress="<address>"`. An empty address
/// is VirtualBox's default, every interface (`0.0.0.0`), which [`on_loopback`] then refuses.
pub(super) fn remote_display(lines: &[String]) -> Vrde {
    let value = |key: &str| {
        lines
            .iter()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('=').map(|v| v.trim().trim_matches('"').to_owned()))
    };
    if value("vrde").as_deref() != Some("on") {
        return Vrde::Off;
    }
    let Some(port) = value("vrdeport")
        .and_then(|p| p.parse::<i32>().ok())
        .and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p > 0)
    else {
        return Vrde::NotListening;
    };
    let address = value("vrdeaddress").unwrap_or_default();
    if address.is_empty() {
        return Vrde::At(SocketAddr::new(std::net::IpAddr::from([0, 0, 0, 0]), port));
    }
    match address.parse() {
        Ok(ip) => Vrde::At(SocketAddr::new(ip, port)),
        Err(_) => Vrde::Unreadable(address),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virsh_says_a_vnc_display_by_its_number() {
        assert_eq!(vnc_display("vnc://127.0.0.1:0\n"), Some("127.0.0.1:5900".parse().unwrap()));
        assert_eq!(vnc_display("vnc://localhost:3"), Some("127.0.0.1:5903".parse().unwrap()));
        assert_eq!(vnc_display("vnc://[::1]:1"), Some("[::1]:5901".parse().unwrap()));
        assert_eq!(
            vnc_display("vnc://0.0.0.0:2"),
            Some("0.0.0.0:5902".parse().unwrap()),
            "read as said; refused later"
        );
        assert_eq!(vnc_display("spice://127.0.0.1:5930"), None);
        assert_eq!(vnc_display(""), None);
    }

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
    fn qemu_says_its_vnc_server_in_either_format() {
        let new = "info vnc\r\ndefault:\r\n  Server: 127.0.0.1:5901 (ipv4)\r\n    Auth: none (Sub: none)\r\n(qemu) ";
        assert_eq!(vnc_server(new), Some("127.0.0.1:5901".parse().unwrap()));
        let old = "Server:\n     address: 127.0.0.1:5900\n        auth: none\n";
        assert_eq!(vnc_server(old), Some("127.0.0.1:5900".parse().unwrap()));
        assert_eq!(vnc_server("default:\n  Server: none\n"), None);
    }

    #[test]
    fn vagrant_qemu_records_its_monitor_socket() {
        let options = "---\n:ssh_port: 50022\n:control_port:\n:monitor_socket: \"/Users/u/.vagrant.d/tmp/vagrant-qemu/abc/qemu_socket\"\n:serial_socket: x\n";
        assert_eq!(
            recorded_socket(options),
            Some(PathBuf::from("/Users/u/.vagrant.d/tmp/vagrant-qemu/abc/qemu_socket"))
        );
        assert_eq!(recorded_socket(":monitor_socket:\n"), None, "a control port instead");
        assert_eq!(recorded_socket(""), None);
    }

    #[test]
    fn virtualbox_says_whether_its_remote_display_is_on_and_where() {
        let lines = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let on = lines(&[
            "name=\"web\"",
            "vrde=\"on\"",
            "vrdeport=3390",
            "vrdeports=\"3390-3489\"",
            "vrdeaddress=\"127.0.0.1\"",
        ]);
        assert_eq!(remote_display(&on), Vrde::At("127.0.0.1:3390".parse().unwrap()));
        assert_eq!(remote_display(&lines(&["vrde=\"off\""])), Vrde::Off);
        assert_eq!(remote_display(&lines(&[])), Vrde::Off);
        assert_eq!(remote_display(&lines(&["vrde=\"on\"", "vrdeport=-1"])), Vrde::NotListening);
        assert_eq!(
            remote_display(&lines(&["vrde=\"on\"", "vrdeport=3389", "vrdeaddress=\"\""])),
            Vrde::At("0.0.0.0:3389".parse().unwrap()),
            "every interface: refused by on_loopback"
        );
        assert_eq!(
            remote_display(&lines(&["vrde=\"on\"", "vrdeport=3389", "vrdeaddress=\"vbox-host\""])),
            Vrde::Unreadable("vbox-host".into()),
            "regression: read as every interface, and refused as listening there"
        );
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
