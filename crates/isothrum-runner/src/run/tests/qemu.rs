//! A vagrant-qemu run: its provider can't list forwarded ports, so its QEMU says them.

use std::path::Path;

use super::vagrant::{VERSION, a_vagrant_stack_on, up_and_ready};
use super::*;
use crate::process::fake::FakeProcesses;

const PORT_UNSUPPORTED: &str = include_str!("../../tool_output/fixtures/vagrant/2.4.1-qemu-port-unsupported.txt");
const USERNET: &str = include_str!("../../tool_output/fixtures/qemu/8.2-info-usernet.txt");

/// A QEMU monitor on a real Unix socket at `socket`: its banner, then the forwards `info usernet`
/// lists, for every connection until the test ends.
fn monitor(socket: &Path) -> tokio::task::JoinHandle<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::UnixListener::bind(socket).expect("the monitor's socket");
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            s.write_all(b"QEMU 8.2.0 monitor - type 'help' for more information\r\n(qemu) ").await.ok();
            let mut buf = [0u8; 64];
            let n = s.read(&mut buf).await.unwrap_or(0);
            if buf.get(..n) == Some(b"info usernet\n".as_slice()) {
                s.write_all(USERNET.as_bytes()).await.ok();
            }
        }
    })
}

/// What vagrant-qemu holds: each machine's state, and the monitor socket it records for them.
#[derive(Debug, Default)]
struct Qemu {
    machines: Vec<(&'static str, &'static str)>,
    socket: std::path::PathBuf,
}

/// Vagrant with vagrant-qemu. An `up` boots `web` and the controller, which powers itself off;
/// it records each VM as vagrant-qemu does (its id, and `run/options.yml` naming the monitor).
fn answer(qemu: &Mutex<Qemu>, call: &Call) -> Reply {
    let mut q = qemu.lock().unwrap_or_else(PoisonError::into_inner);
    let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["--version"] => Reply::ok(VERSION),
        ["plugin", "list", "--machine-readable"] => Reply::ok("1794305819,,plugin-name,vagrant-qemu"),
        ["up", "--provider", "qemu", ..] => {
            let dot = Path::new(call.env("VAGRANT_DOTFILE_PATH").expect("the dotfile folder"));
            for m in ["controller", "web"] {
                let dir = dot.join("machines").join(m).join("qemu");
                std::fs::create_dir_all(dir.join(format!("id-{m}")).join("run")).expect("its folders");
                std::fs::write(dir.join("id"), format!("id-{m}")).expect("its id");
                let options = format!("---\n:ssh_port: 50022\n:monitor_socket: \"{}\"\n", q.socket.display());
                std::fs::write(dir.join(format!("id-{m}")).join("run").join("options.yml"), options).expect("its options");
            }
            q.machines = vec![("controller", "stopped"), ("web", "running")];
            Reply::ok("")
        }
        ["status", "--machine-readable"] => {
            let lines: Vec<String> = q.machines.iter().map(|(m, s)| format!("1794305819,{m},state,{s}")).collect();
            Reply::ok(&lines.join("\n"))
        }
        ["port", "web", "--machine-readable"] => Reply {
            code: 1,
            ..Reply::ok(PORT_UNSUPPORTED)
        },
        ["halt"] => {
            for (_, s) in &mut q.machines {
                *s = "stopped";
            }
            Reply::ok("")
        }
        _ => panic!("a call the model doesn't know: {}", call.line()),
    }
}

#[tokio::test]
async fn a_qemu_run_reads_ready_with_the_ports_its_qemu_forwards_and_parks() {
    let sockets = tempfile::tempdir().expect("a folder");
    let socket = sockets.path().join("m.sock");
    let listening = monitor(&socket);
    let qemu = Arc::new(Mutex::new(Qemu {
        socket: socket.clone(),
        ..Qemu::default()
    }));
    let fake = Arc::new(FakeProcesses::model({
        let qemu = Arc::clone(&qemu);
        move |call| answer(&qemu, call)
    }));
    let (_root, project, runner) = a_vagrant_stack_on(&fake);
    let run = runner.open(&project, None).expect("opened");
    let opts = RunOptions::new().with_provider(crate::Provider::Qemu);

    let status = up_and_ready(&run, &opts).await;
    let web = status.machines.iter().find(|m| m.name == "web").expect("web");
    let ports: Vec<(u16, u16)> = web.published.iter().map(|p| (p.port.get(), p.host_port.get())).collect();
    assert_eq!(ports, [(80, 8080), (53, 5353), (22, 50022)], "what its QEMU forwards");

    run.park(crate::Park::Shutdown, &opts, &Recorder::new()).await.expect("a park is accepted");
    assert_eq!(run.status_with(&opts).await.expect("status").state, RunState::Parked);
    listening.abort();
}
