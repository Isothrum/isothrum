//! The crate's own test doubles.

use std::time::Duration;

use isothrum_runner::fake::FakeProcesses;
use isothrum_runner::{BoxFuture, Exit, OnLine, ProcessError, ProcessRunner, ProcessSpec};

/// A tool that never answers.
pub(crate) struct Stuck;

impl ProcessRunner for Stuck {
    fn run<'a>(&'a self, _spec: &'a ProcessSpec, _on_line: OnLine<'a>) -> BoxFuture<'a, Result<Exit, ProcessError>> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok(Exit::new(Some(0)))
        })
    }
}

/// One call that never answers (its whole command line), every other answered by a strict script.
pub(crate) struct StuckOn {
    /// The call that hangs.
    call: String,
    /// What answers the others.
    rest: FakeProcesses,
}

impl StuckOn {
    /// `call` hangs, `rest` answers the others.
    pub(crate) fn new(call: &str, rest: FakeProcesses) -> StuckOn {
        StuckOn { call: call.to_owned(), rest }
    }
}

impl ProcessRunner for StuckOn {
    fn run<'a>(&'a self, spec: &'a ProcessSpec, on_line: OnLine<'a>) -> BoxFuture<'a, Result<Exit, ProcessError>> {
        if spec.line() == self.call {
            Stuck.run(spec, on_line)
        } else {
            self.rest.run(spec, on_line)
        }
    }
}

/// `text` written to `path`, its folders made.
pub(crate) fn write(path: &std::path::Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// `host`'s KVM loaded as a module holding the CPU's virtualization, `modules` its `/proc/modules`.
pub(crate) fn kvm_holding(host: &crate::host::Host, modules: &str) {
    write(&host.devices.proc_modules, modules);
    write(&host.devices.kvm_virt_at_load, "Y\n");
}

/// A context on a Linux host with apt (its devices nowhere), its tools answered by `tools`.
pub(crate) fn on_apt(tools: FakeProcesses) -> crate::Context {
    let mut host = linux_host(std::path::Path::new("/nonexistent/isothrum-machine-test"));
    host.packages = Some(crate::host::Packages::Linux(crate::host::LinuxPackages::Apt));
    crate::Context::new(std::sync::Arc::new(tools), host)
}

/// A Linux x86-64 host whose devices are under `root`, with no package manager known.
pub(crate) fn linux_host(root: &std::path::Path) -> crate::host::Host {
    crate::host::Host {
        os: isothrum_runner::HostOs::Linux,
        arch: crate::host::Arch::Amd64,
        packages: None,
        devices: crate::host::Devices::under(root),
    }
}

/// The virtiofsd package as Debian and Fedora install it under `devices`: `/usr/libexec/virtiofsd`,
/// which QEMU finds there, named by libvirt's descriptor
/// `/usr/share/qemu/vhost-user/50-virtiofsd.json`.
pub(crate) fn install_virtiofsd(devices: &crate::host::Devices) {
    let write = |path: &std::path::Path, text: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    let binary = &devices.virtiofsd[0];
    write(binary, "#!/bin/sh\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let descriptor = serde_json::json!({ "description": "virtiofsd", "type": "fs", "binary": binary });
    write(&devices.vhost_user[0].join("50-virtiofsd.json"), &descriptor.to_string());
}

/// A context on a Linux host whose tools `runner` answers.
pub(crate) fn answered_by(runner: impl isothrum_runner::ProcessRunner + 'static) -> crate::Context {
    shared(std::sync::Arc::new(runner))
}

/// [`answered_by`] a runner the test keeps too (a fake whose calls it reads).
pub(crate) fn shared(runner: std::sync::Arc<dyn isothrum_runner::ProcessRunner>) -> crate::Context {
    crate::Context::new(runner, linux_host(std::path::Path::new("/nonexistent/isothrum-machine-test")))
}

/// How a tool a test ran ended, and what it printed (each stream's lines, one `\n` after each),
/// kept on unix only, where the shell tests read it.
#[derive(Debug)]
pub(crate) struct Out {
    pub(crate) status: isothrum_runner::Exit,
    #[cfg(unix)]
    pub(crate) stdout: Vec<u8>,
    #[cfg(unix)]
    pub(crate) stderr: Vec<u8>,
}

/// Runs `program args` with the variables `env` through the runner's own processes (on a runtime
/// of its own: the tests calling it are not async); `None` when it can't be started.
pub(crate) fn run(program: &str, args: &[&str]) -> Option<Out> {
    run_with(program, args, &[])
}

/// [`run`], with the variables `env` added (macOS's tests only call it).
#[cfg(target_os = "macos")]
pub(crate) fn run_env(program: &str, args: &[&str], env: &[(&str, &str)]) -> Option<Out> {
    run_with(program, args, env)
}

/// [`run`], with the variables `env` added. It runs on a thread of its own, with a runtime of its
/// own, so an async test (already inside a runtime) can call it too.
fn run_with(program: &str, args: &[&str], env: &[(&str, &str)]) -> Option<Out> {
    std::thread::scope(|scope| scope.spawn(|| run_here(program, args, env)).join().expect("the program's thread ends"))
}

/// [`run_with`], on this thread: one that isn't driving a runtime.
fn run_here(program: &str, args: &[&str], env: &[(&str, &str)]) -> Option<Out> {
    let mut spec = isothrum_runner::ProcessSpec::new(program, args.iter().copied());
    spec.env = env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = runtime.block_on(async {
        let mut keep = |stream: isothrum_runner::Stream, line: &str| {
            let to = if stream == isothrum_runner::Stream::Stdout {
                &mut stdout
            } else {
                &mut stderr
            };
            to.extend_from_slice(line.as_bytes());
            to.push(b'\n');
        };
        isothrum_runner::ProcessRunner::run(&isothrum_runner::SystemProcesses::default(), &spec, &mut keep).await
    });
    exit.ok().map(|status| Out {
        status,
        #[cfg(unix)]
        stdout,
        #[cfg(unix)]
        stderr,
    })
}

/// `sh -c script` with the variables `env` added ([`run`]); a test that can't start `sh` fails.
#[cfg(unix)]
pub(crate) fn sh(script: &str, env: &[(&str, &str)]) -> Out {
    run_with("sh", &["-c", script], env).expect("sh starts")
}

/// Whether `sh -n` parses `script` (no `sh`, on Windows: taken as parsing).
pub(crate) fn sh_parses(script: &str) -> bool {
    match run("sh", &["-n", "-c", script]) {
        Some(out) => out.status.success(),
        None => cfg!(windows),
    }
}

/// Writes `contents` at `path`, executable (mode 0755).
#[cfg(unix)]
pub(crate) fn write_executable(path: impl AsRef<std::path::Path>, contents: impl AsRef<str>) {
    let path = path.as_ref();
    std::fs::write(path, contents.as_ref()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A fresh folder of its own for one test, named after `tag`, which the test removes.
pub(crate) fn fresh_dir(tag: &str) -> std::path::PathBuf {
    tempfile::Builder::new().prefix(&format!("isothrum-machine-{tag}-")).tempdir().unwrap().keep()
}
