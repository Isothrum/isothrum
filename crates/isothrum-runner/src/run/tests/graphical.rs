//! A machine's graphical console through the fake engines: where each Vagrant provider keeps it
//! (libvirt's display, VirtualBox's remote display, the port and password the runner chose for
//! VMware), refused where the manifest says there is none, off the loopback, or the machine
//! isn't running. No real tool ever runs.

use super::*;
use crate::testing::WEB_VM;
use crate::{Endpoint, ErrorCode};
use isoloom_manifest::contract::CONSOLES;
use isoloom_manifest::{ConsoleProtocol, Provider};

/// The consoles Isoloom gives a Vagrant VM, one per provider, as it writes them.
const CONSOLES_JSON: &str = r#"[
  {"providers": ["virtualbox"], "protocol": "rdp", "source": {"from": "vrde"}, "note": "on only when the Oracle VM VirtualBox Extension Pack is installed as the VM boots"},
  {"providers": ["vmware_desktop"], "protocol": "vnc", "source": {"from": "runner"}},
  {"providers": ["libvirt"], "protocol": "vnc", "source": {"from": "libvirt"}},
  {"providers": ["qemu"], "protocol": "vnc", "source": {"from": "qemu_monitor"}},
  {"providers": ["parallels"], "protocol": "none", "note": "Isoloom sets up no graphical console on Parallels Desktop (its VNC server stays off)"}
]"#;

/// The `web-vm` manifest, its web VM listing its consoles.
fn with_consoles() -> String {
    let mut m: serde_json::Value = serde_json::from_str(WEB_VM).unwrap();
    let web = m["machines"].as_array_mut().unwrap().iter_mut().find(|x| x["name"] == "web").unwrap();
    web["consoles"] = serde_json::from_str(CONSOLES_JSON).unwrap();
    m.to_string()
}

/// What the fake hypervisor says: whether the VMs run, what `virsh domdisplay` and
/// `VBoxManage showvminfo` print.
#[derive(Debug, Clone)]
struct Hypervisor {
    running: bool,
    display: &'static str,
    vminfo: &'static str,
}

/// A run of [`with_consoles`] up on `provider`, its hypervisor as `h` (changed through the
/// handle it gives), its free ports `ports`.
async fn up_on(provider: Provider, h: Hypervisor, ports: &[u16]) -> (RunFolder, Arc<FakeProcesses>, Arc<Mutex<Hypervisor>>, Runner) {
    let folder = RunFolder::new("vagrant", &with_consoles());
    // The VMs boot with `vagrant up`.
    let h = Arc::new(Mutex::new(Hypervisor { running: false, ..h }));
    let fake = Arc::new(FakeProcesses::new({
        let h = Arc::clone(&h);
        move |call| {
            let line = call.line();
            if line.starts_with("vagrant up") {
                h.lock().unwrap_or_else(PoisonError::into_inner).running = true;
            }
            let h = h.lock().unwrap_or_else(PoisonError::into_inner).clone();
            if line.contains("plugin list") {
                let plugins = ["vagrant-libvirt", "vagrant-vmware-desktop", "vagrant-qemu", "vagrant-parallels"];
                return Reply::ok(&plugins.map(|p| format!("1,,plugin-name,{p}")).join("\n"));
            }
            if line.starts_with("vagrant status") || line.contains(" status --machine-readable") {
                let state = if h.running { "running" } else { "poweroff" };
                return Reply::ok(&format!("1,web,state,{state}\n1,isoloom-controller,state,{state}"));
            }
            if call.program == "virsh" {
                return Reply::ok(h.display);
            }
            if line.contains("showvminfo") {
                return Reply::ok(h.vminfo);
            }
            Reply::ok("")
        }
    }));
    // The console ports first (picked before any step), then the web page's forward.
    let fake_ports = FakePorts::new(ports.iter().copied().chain([18080]));
    // Where the hypervisors below say the consoles are, they answer.
    for port in [5902, 3391, 5931, 5932, 18080] {
        fake_ports.answer("127.0.0.1", port);
    }
    let host_ports: Arc<dyn crate::HostPorts> = Arc::new(fake_ports);
    // Parallels is a Mac's; the others run on Linux too.
    let host = if matches!(provider, Provider::Parallels | Provider::Qemu) {
        isoloom_manifest::HostOs::Macos
    } else {
        isoloom_manifest::HostOs::Linux
    };
    let runner = Runner::new(runner_config(folder.home.path(), processes(&fake), host_ports, host)).unwrap();
    {
        let run = runner.open(&folder.manifest, None).unwrap();
        let mut secrets = Secrets::new();
        secrets.insert("DEMO_SECRET", SecretValue::new(SECRET));
        let opts = RunOptions::new().with_provider(provider).with_secrets(secrets);
        run.up(&opts, &Recorder::new()).await.unwrap();
    }
    (folder, fake, h, runner)
}

const RUNNING: Hypervisor = Hypervisor {
    running: true,
    display: "vnc://127.0.0.1:2\n",
    vminfo: "name=\"web-vm - web\"\nvrde=\"on\"\nvrdeport=3391\nvrdeaddress=\"127.0.0.1\"\n",
};

#[tokio::test]
async fn libvirts_display_is_read_from_libvirt_and_shown_in_the_status() {
    let (folder, fake, _, runner) = up_on(Provider::Libvirt, RUNNING, &[]).await;
    let run = runner.open(&folder.manifest, None).unwrap();
    let console = run.graphical_console("web").await.unwrap();
    assert_eq!(console.protocol, ConsoleProtocol::Vnc);
    assert_eq!(console.endpoint, Endpoint::Loopback("127.0.0.1:5902".parse().unwrap()));
    assert!(console.password.is_none() && !console.tunnelled());
    let asked = fake.last("domdisplay").expect("libvirt asked");
    assert_eq!(asked.line(), "virsh -c qemu:///system domdisplay web-vm_web", "the domain the manifest names");
    let status = run.status().await.unwrap();
    let web = status.machines.iter().find(|m| m.name == "web").unwrap();
    assert_eq!(web.console.as_ref().map(|c| c.protocol), Some(ConsoleProtocol::Vnc));
    let controller = status.machines.iter().find(|m| m.name == "isoloom-controller").unwrap();
    assert_eq!(controller.console, None, "Isoloom's own VM lists none");
}

#[tokio::test]
async fn a_display_off_the_loopback_is_refused() {
    let lan = Hypervisor {
        display: "vnc://0.0.0.0:0\n",
        ..RUNNING
    };
    let (folder, _, _, runner) = up_on(Provider::Libvirt, lan, &[]).await;
    let run = runner.open(&folder.manifest, None).unwrap();
    let err = run.graphical_console("web").await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::NoConsole);
    assert!(err.to_string().contains("0.0.0.0:5900, not on this computer's loopback"), "{err}");
}

#[tokio::test]
async fn virtualbox_without_its_extension_pack_says_so() {
    let (folder, _, _, runner) = up_on(Provider::Virtualbox, RUNNING, &[]).await;
    let run = runner.open(&folder.manifest, None).unwrap();
    let console = run.graphical_console("web").await.unwrap();
    assert_eq!(
        (console.protocol, console.endpoint.clone()),
        (ConsoleProtocol::Rdp, Endpoint::Loopback("127.0.0.1:3391".parse().unwrap()))
    );
    assert!(console.note.as_deref().is_some_and(|n| n.contains("Extension Pack")));
    drop(run);
    let off = Hypervisor {
        vminfo: "vrde=\"off\"\nvrdeport=-1\n",
        ..RUNNING
    };
    let (no_pack, _, _, its_runner) = up_on(Provider::Virtualbox, off, &[]).await;
    let err = its_runner.open(&no_pack.manifest, None).unwrap().graphical_console("web").await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::NoConsole);
    assert!(err.to_string().contains("needs the Oracle VM VirtualBox Extension Pack"), "{err}");
}

#[tokio::test]
async fn vmwares_console_is_the_runners_choice_handed_to_vagrant_alone() {
    let (folder, fake, _, runner) = up_on(Provider::VmwareDesktop, RUNNING, &[5931]).await;
    let up = fake.last("vagrant up").expect("the VMs booted");
    let given: serde_json::Value = serde_json::from_str(up.env(CONSOLES).expect("Vagrant is given the consoles")).unwrap();
    assert_eq!(given["web"]["port"], 5931, "a free loopback port");
    let password = given["web"]["password"].as_str().unwrap().to_owned();
    assert_eq!(password.len(), 8);
    assert!(!up.line().contains(&password), "never on a command line");
    let run = runner.open(&folder.manifest, None).unwrap();
    let console = run.graphical_console("web").await.unwrap();
    assert_eq!(console.endpoint, Endpoint::Loopback("127.0.0.1:5931".parse().unwrap()));
    assert_eq!(console.password.as_ref().map(SecretValue::expose), Some(password.as_str()));
    let shell = run.console_command("web", None).unwrap();
    assert!(
        !shell.env.iter().any(|(k, _)| k == CONSOLES),
        "an embedder's command never holds the passwords: {:?}",
        shell.env
    );
    // The same choice at every later start, until `down` forgets it.
    let state = folder.home.path().join("runs/web-vm/consoles.json");
    assert!(state.is_file());
    run.down(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert!(!state.exists(), "down forgets the run's consoles");
}

#[tokio::test]
async fn no_console_where_the_manifest_says_none_nor_on_a_stopped_vm() {
    let (folder, _, _, runner) = up_on(Provider::Parallels, RUNNING, &[]).await;
    let run = runner.open(&folder.manifest, None).unwrap();
    let none = run.graphical_console("web").await.unwrap_err();
    assert_eq!(
        none.to_string(),
        "machine `web` has no graphical console: Isoloom sets up no graphical console on Parallels Desktop (its VNC server stays off)"
    );
    let controller = run.graphical_console("isoloom-controller").await.unwrap_err();
    assert!(
        controller.to_string().ends_with("the run's manifest lists no graphical console for it"),
        "{controller}"
    );
    assert_eq!(run.graphical_console("nope").await.unwrap_err().code(), ErrorCode::MachineUnknown);
    drop(run);
    let (stopped, _, h, its_runner) = up_on(Provider::Libvirt, RUNNING, &[]).await;
    h.lock().unwrap_or_else(PoisonError::into_inner).running = false;
    let err = its_runner.open(&stopped.manifest, None).unwrap().graphical_console("web").await.unwrap_err();
    assert_eq!(
        (err.code(), err.to_string()),
        (
            ErrorCode::NoConsole,
            "machine `web` has no graphical console: it isn't running (stopped)".to_owned()
        )
    );
}

#[tokio::test]
async fn a_query_never_chooses_a_console_only_an_operation_does() {
    // Regression: the Vagrant driver's prepare chose them, so an unlocked `status` or
    // `graphical_console` generated passwords and wrote `consoles.json`, racing a start in another
    // process whose Vagrant had read the file already.
    let (folder, _, _, runner) = up_on(Provider::VmwareDesktop, RUNNING, &[5931, 5932]).await;
    let state = folder.home.path().join("runs/web-vm/consoles.json");
    std::fs::remove_file(&state).unwrap();
    let run = runner.open(&folder.manifest, None).unwrap();
    run.status().await.unwrap();
    let err = run.graphical_console("web").await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::NoConsole);
    assert!(err.to_string().ends_with("it boots with one at the run's next start"), "{err}");
    assert!(!state.exists(), "a query wrote no password");
    run.resume(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert!(state.is_file(), "an operation chooses them before its Vagrant commands");
    run.graphical_console("web").await.unwrap();
}

/// A console where its hypervisor says it is, that doesn't answer there, is offered as it stands:
/// the status says why it can't be shown now, and the viewer is refused it, saying where it
/// didn't answer.
#[tokio::test]
async fn a_console_that_doesnt_answer_where_its_hypervisor_says_is_said_unavailable() {
    let silent = Hypervisor {
        display: "vnc://127.0.0.1:7\n",
        ..RUNNING
    };
    let (folder, _, _, runner) = up_on(Provider::Libvirt, silent, &[]).await;
    let run = runner.open(&folder.manifest, None).unwrap();
    let err = run.graphical_console("web").await.unwrap_err();
    assert_eq!(
        (err.code(), err.to_string()),
        (
            ErrorCode::NoConsole,
            "machine `web` has no graphical console: its screen didn't answer at 127.0.0.1:5907".to_owned()
        )
    );
    let status = run.status().await.unwrap();
    let web = status.machines.iter().find(|m| m.name == "web").unwrap();
    let offer = web.console.as_ref().unwrap();
    assert_eq!(
        (offer.protocol, offer.unavailable.as_deref()),
        (ConsoleProtocol::Vnc, Some("its screen didn't answer at 127.0.0.1:5907"))
    );
}

/// A running machine whose console answers is offered with nothing against it; a stopped one
/// isn't looked at.
#[tokio::test]
async fn a_console_that_answers_is_offered_and_a_stopped_machines_isnt_looked_at() {
    let (folder, fake, h, runner) = up_on(Provider::Libvirt, RUNNING, &[]).await;
    let run = runner.open(&folder.manifest, None).unwrap();
    let offer = |status: &crate::Status| status.machines.iter().find(|m| m.name == "web").and_then(|m| m.console.clone());
    let up = run.status().await.unwrap();
    assert_eq!(offer(&up).map(|o| o.unavailable), Some(None));
    h.lock().unwrap_or_else(PoisonError::into_inner).running = false;
    let asked = fake.lines().iter().filter(|l| l.contains("domdisplay")).count();
    let down = run.status().await.unwrap();
    assert_eq!(offer(&down).map(|o| o.unavailable), Some(None));
    assert_eq!(fake.lines().iter().filter(|l| l.contains("domdisplay")).count(), asked, "libvirt not asked");
}

/// A fake QEMU monitor on a real Unix socket at `socket`, answering `info vnc` with `said`
/// (after its banner), to every connection.
#[cfg(unix)]
fn qemu_monitor(socket: &std::path::Path, said: &'static str) -> tokio::task::JoinHandle<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::UnixListener::bind(socket).unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            s.write_all(b"QEMU 10.1.0 monitor - type 'help' for more information\r\n(qemu) ").await.unwrap();
            let mut buf = [0u8; 64];
            let _ = s.read(&mut buf).await.unwrap();
            s.write_all(format!("info vnc\r\n{said}\r\n(qemu) ").as_bytes()).await.unwrap();
        }
    })
}

/// Regression (a Mac on vagrant-qemu): a VM QEMU runs with no VNC display, started before the
/// run's Vagrantfile gave it one, was offered a screen the viewer then never showed. Its QEMU
/// says so (`info vnc`: `None`): the status says it can't be shown and why, and so does the
/// console; one started with its display is offered once it answers.
#[cfg(unix)]
#[tokio::test]
async fn a_qemu_vm_started_without_a_vnc_display_says_to_restart_it() {
    let (folder, _, _, runner) = up_on(Provider::Qemu, RUNNING, &[]).await;
    let run = runner.open(&folder.manifest, None).unwrap();
    // Where vagrant-qemu records the VM and its monitor socket.
    let machine = folder.home.path().join("runs/web-vm/vagrant/machines/web/qemu");
    let sockets = tempfile::tempdir().unwrap();
    let socket = sockets.path().join("m.sock");
    std::fs::create_dir_all(machine.join("vq_1/run")).unwrap();
    std::fs::write(machine.join("id"), "vq_1").unwrap();
    std::fs::write(
        machine.join("vq_1/run/options.yml"),
        format!("---\n:monitor_socket: \"{}\"\n", socket.display()),
    )
    .unwrap();
    let without = qemu_monitor(&socket, "None");
    let why = "its VM was started without a screen (its QEMU has no VNC display): restart it to get one";
    let err = run.graphical_console("web").await.unwrap_err();
    assert_eq!(err.to_string(), format!("machine `web` has no graphical console: {why}"));
    let status = run.status().await.unwrap();
    let web = status.machines.iter().find(|m| m.name == "web").unwrap();
    assert_eq!(web.console.as_ref().and_then(|c| c.unavailable.as_deref()), Some(why));
    without.abort();
    std::fs::remove_file(&socket).unwrap();
    let with = qemu_monitor(&socket, "default:\r\n  Server: 127.0.0.1:5902 (ipv4)\r\n    Auth: none (Sub: none)");
    let console = run.graphical_console("web").await.unwrap();
    assert_eq!(console.endpoint, Endpoint::Loopback("127.0.0.1:5902".parse().unwrap()));
    with.abort();
}
