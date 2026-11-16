//! A Vagrant run whose controller powers itself off at the end of its own set-up.

use std::collections::BTreeMap;
use std::path::Path;

use super::*;
use crate::process::fake::FakeProcesses;

/// `vagrant --version`, as Vagrant 2.4.1 prints it.
pub(super) const VERSION: &str = "Vagrant 2.4.1";

/// What VirtualBox holds of the stack: each machine's state label, by name.
#[derive(Debug, Default)]
struct Stack {
    machines: BTreeMap<String, &'static str>,
}

/// The stack's `status --machine-readable`, as Vagrant prints it.
fn status_of(stack: &Stack) -> String {
    ["controller", "web"]
        .iter()
        .flat_map(|m| {
            let state = stack.machines.get(*m).copied().unwrap_or("not_created");
            [
                format!("1794305819,{m},metadata,provider,virtualbox"),
                format!("1794305819,{m},provider-name,virtualbox"),
                format!("1794305819,{m},state,{state}"),
            ]
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Records machine `m` as Vagrant does, in the dotfile folder `call` is given.
fn record(call: &Call, m: &str) {
    let dot = call.env("VAGRANT_DOTFILE_PATH").expect("the dotfile folder");
    let dir = Path::new(dot).join("machines").join(m).join("virtualbox");
    std::fs::create_dir_all(&dir).expect("its folder");
    std::fs::write(dir.join("id"), format!("uuid-{m}")).expect("its id");
}

/// Vagrant over VirtualBox. An `up` boots both machines, and the controller's last provisioner
/// powers it off; a `halt` powers every machine off.
fn answer(stack: &Mutex<Stack>, call: &Call) -> Reply {
    let mut stack = stack.lock().unwrap_or_else(PoisonError::into_inner);
    let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
    match (call.program.as_str(), args.as_slice()) {
        ("vagrant", ["--version"]) => Reply::ok(VERSION),
        ("vagrant", ["up", "--provider", "virtualbox", ..]) => {
            for m in ["controller", "web"] {
                record(call, m);
            }
            stack.machines.insert("web".into(), "running");
            stack.machines.insert("controller".into(), "poweroff");
            Reply::ok("==> controller: Running provisioner: halt (shell)...")
        }
        ("vagrant", ["status", "--machine-readable"]) => Reply::ok(&status_of(&stack)),
        ("vagrant", ["port", m, "--machine-readable"]) => Reply::ok(&format!("1794305819,{m},forwarded_port,22,2222")),
        ("vagrant", ["halt"]) => {
            for state in stack.machines.values_mut() {
                *state = "poweroff";
            }
            Reply::ok("")
        }
        ("VBoxManage", ["showvminfo", _, "--machinereadable"]) => Reply::ok("VMState=\"running\""),
        _ => panic!("a call the model doesn't know: {}", call.line()),
    }
}

/// A modelled VirtualBox `M` (its state, `Default` at first), a fake answering from it with
/// `answer`, and a Vagrant project with a runner on that fake.
pub(super) fn a_modelled_vagrant_stack<M: Default + Send + 'static>(
    answer: fn(&Mutex<M>, &Call) -> Reply,
) -> (Arc<Mutex<M>>, Arc<FakeProcesses>, tempfile::TempDir, std::path::PathBuf, Runner) {
    let stack = Arc::new(Mutex::new(M::default()));
    let fake = Arc::new(FakeProcesses::model({
        let stack = Arc::clone(&stack);
        move |call| answer(&stack, call)
    }));
    let (root, project, runner) = a_vagrant_stack_on(&fake);
    (stack, fake, root, project, runner)
}

/// A modelled Vagrant over VirtualBox's answer to `call`: what every model answers the same way
/// (the preamble's look at a saved state, none cut off, and `vagrant --version`), else `rest`'s,
/// given the model locked and the call's arguments.
pub(super) fn vagrant_answer<M>(stack: &Mutex<M>, call: &Call, rest: impl FnOnce(&mut M, &[&str]) -> Reply) -> Reply {
    if call.program == "VBoxManage" {
        return Reply::ok("VMState=\"running\"");
    }
    let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
    if args == ["--version"] {
        return Reply::ok(VERSION);
    }
    rest(&mut stack.lock().unwrap_or_else(PoisonError::into_inner), &args)
}

/// A Vagrant project in a temporary folder, and a runner with a temporary home on `fake`.
pub(super) fn a_vagrant_stack_on(fake: &Arc<FakeProcesses>) -> (tempfile::TempDir, std::path::PathBuf, Runner) {
    let root = tempfile::tempdir().expect("a folder");
    let project = root.path().join("stack");
    std::fs::create_dir(&project).expect("the project");
    std::fs::write(project.join("Vagrantfile"), "Vagrant.configure('2') do |c|\nend\n").expect("a Vagrantfile");
    let config = runner_config(&root.path().join("home"), processes(fake), no_ports(), crate::runner::this_host());
    let runner = Runner::new(config).expect("a runner");
    (root, project, runner)
}

/// `run` brought up with `opts`, and its status, read ready.
pub(super) async fn up_and_ready(run: &crate::Running<'_>, opts: &RunOptions) -> crate::Status {
    run.up(opts, &Recorder::new()).await.expect("up");
    let status = run.status_with(opts).await.expect("status");
    assert_eq!(status.state, RunState::Ready, "{:?}", status.machines);
    status
}

/// [`a_vagrant_stack_on`] `fake`, which the test doesn't read.
fn a_vagrant_stack(fake: FakeProcesses) -> (tempfile::TempDir, std::path::PathBuf, Runner) {
    a_vagrant_stack_on(&Arc::new(fake))
}

#[tokio::test]
async fn a_controller_that_powered_itself_off_leaves_the_run_ready_to_update_and_park() {
    let stack = Arc::new(Mutex::new(Stack::default()));
    let (_root, project, runner) = a_vagrant_stack(FakeProcesses::model({
        let stack = Arc::clone(&stack);
        move |call| answer(&stack, call)
    }));
    let run = runner.open(&project, None).expect("opened");
    let opts = RunOptions::new().with_provider(crate::Provider::Virtualbox);

    let status = up_and_ready(&run, &opts).await;
    let machines: Vec<(&str, &MachineState)> = status.machines.iter().map(|m| (m.name.as_str(), &m.state)).collect();
    assert_eq!(machines, [("controller", &MachineState::Stopped), ("web", &MachineState::Running)]);

    run.update(&opts, &Recorder::new()).await.expect("an update is accepted");
    run.park(crate::Park::Shutdown, &opts, &Recorder::new()).await.expect("a park is accepted");
    assert_eq!(run.status_with(&opts).await.expect("status").state, RunState::Parked);
}

/// A refused sign-in to the ESXi host, as the `vmware_esxi` plugin says it.
const REFUSED: &str = include_str!("../../tool_output/fixtures/vagrant/vmware-esxi-2.5.5-auth-failed.txt");

/// Vagrant with the `vmware_esxi` plugin, whose host takes the password `right` only.
fn esxi(call: &Call) -> Reply {
    let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["--version"] => Reply::ok(VERSION),
        ["plugin", "list", "--machine-readable"] => Reply::ok("1794305819,,plugin-name,vagrant-vmware-esxi"),
        ["status", "--machine-readable"] if call.env("ESXI_PASSWORD") == Some("right") => {
            Reply::ok("1794305819,web,metadata,provider,vmware_esxi\n1794305819,web,provider-name,vmware_esxi\n1794305819,web,state,powered_off")
        }
        ["status", "--machine-readable"] => Reply::fail(1, REFUSED),
        _ => panic!("a call the model doesn't know: {}", call.line()),
    }
}

#[tokio::test]
async fn an_esxi_status_gets_the_secrets_given_and_a_refused_sign_in_is_server_auth_failed() {
    let fake = Arc::new(FakeProcesses::model(esxi));
    let (_root, project, runner) = a_vagrant_stack_on(&fake);
    let run = runner.open(&project, None).expect("opened");
    let mut secrets = crate::Secrets::new();
    secrets.insert("ESXI_PASSWORD", crate::SecretValue::new("right"));
    let given = RunOptions::new().with_provider(crate::Provider::VmwareEsxi).with_secrets(secrets);

    let status = run.status_with(&given).await.expect("the host takes the password given");
    assert_eq!(status.state, RunState::Parked);
    let read = fake.made("vagrant status");
    assert_eq!(read.last().and_then(|c| c.env("ESXI_PASSWORD")), Some("right"), "the status read got it");

    let none = RunOptions::new().with_provider(crate::Provider::VmwareEsxi);
    let e = run.status_with(&none).await.expect_err("no password");
    assert_eq!(e.code(), ErrorCode::ServerAuthFailed, "{e}");
    let detail = e.detail().expect("a detail");
    assert_eq!(
        detail.data,
        crate::error::Detail::ServerAuthFailed {
            server: Some("esxi.stack.example".to_owned())
        }
    );
}
