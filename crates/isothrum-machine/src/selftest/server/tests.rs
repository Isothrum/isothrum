//! The server self-test's course on a scripted deployment: each step said as its line ends it,
//! the failures people hit said on their step, SSH waited for, the command checked, what an
//! earlier test left taken down first, and the teardown always run. Then the whole test on a
//! modelled Vagrant with the ESXi plugin, through the runner.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use isothrum_runner::fake::{Call, FakeProcesses, Reply};
use isothrum_runner::{ErrorCode, ProcessRunner, SecretValue};

use super::*;
use crate::testing::linux_host;

/// A scripted command's answer: its exit status and output, or the runner's failure.
type Answer = Result<(i32, String), Box<ErrorReport>>;

/// A deployment that prints `lines` as it comes up and ends as scripted; its commands answer in
/// turn (then `uname`'s answer); what was called is kept.
struct Scripted {
    lines: Vec<&'static str>,
    up: Option<Box<ErrorReport>>,
    execs: Mutex<VecDeque<Answer>>,
    down: Option<Box<ErrorReport>>,
    calls: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(lines: &[&'static str], up: Option<&str>) -> Scripted {
        Scripted {
            lines: lines.to_vec(),
            up: up.map(failed),
            execs: Mutex::new(VecDeque::new()),
            down: None,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn called(&self, what: impl Into<String>) {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).push(what.into());
    }
}

/// A tool's failure saying `message`.
fn failed(message: &str) -> Box<ErrorReport> {
    Box::new(ErrorReport::new(ErrorCode::ToolFailed, message))
}

impl Deployment for Scripted {
    fn up<'a>(&'a self, line: &'a (dyn Fn(&str) + Send + Sync)) -> BoxFuture<'a, Result<(), Box<ErrorReport>>> {
        self.called("up");
        self.lines.iter().for_each(|l| line(l));
        let up = self.up.clone().map_or(Ok(()), Err);
        Box::pin(async move { up })
    }

    fn exec<'a>(&'a self, argv: &'a [&'a str]) -> BoxFuture<'a, Result<(i32, String), Box<ErrorReport>>> {
        self.called(argv.join(" "));
        let next = self.execs.lock().unwrap_or_else(PoisonError::into_inner).pop_front();
        Box::pin(async move { next.unwrap_or_else(|| Ok((0, "Linux x86_64\n".to_owned()))) })
    }

    fn down<'a>(&'a self, line: &'a (dyn Fn(&str) + Send + Sync)) -> BoxFuture<'a, Result<(), Box<ErrorReport>>> {
        self.called("down");
        line("Destroying…");
        let down = self.down.clone().map_or(Ok(()), Err);
        Box::pin(async move { down })
    }
}

/// The test of `run` (`left`: an earlier test's VM recorded) along `course`: what it said and how
/// it ran.
async fn tested<C: Course>(run: &Scripted, left: bool, course: C) -> (Vec<StepProgress>, Ran) {
    let said = Mutex::new(Vec::new());
    let sink = |p: StepProgress| said.lock().unwrap_or_else(PoisonError::into_inner).push(p);
    let ran = test(run, left, course, &sink, Duration::from_secs(9)).await;
    (said.into_inner().unwrap_or_else(PoisonError::into_inner), ran)
}

/// Each step's last state, in the order steps were first said; lines aside.
fn ends(said: &[StepProgress]) -> Vec<String> {
    let mut out: Vec<(Step, &str)> = Vec::new();
    for p in said {
        let (step, state) = match p {
            Progress::Running(s) => (*s, "running"),
            Progress::Passed(_, Found::Line { .. } | Found::Removing) => continue,
            Progress::Passed(s, _) => (*s, "ok"),
            Progress::Skipped(s, _) => (*s, "skip"),
            Progress::Failed(s, _) => (*s, "fail"),
        };
        match out.iter_mut().find(|(s, _)| *s == step) {
            Some(found) => found.1 = state,
            None => out.push((step, state)),
        }
    }
    out.into_iter().map(|(s, st)| format!("{s:?}:{st}")).collect()
}

/// The lines step `step` showed while it ran, with the size each carried.
fn shown(said: &[StepProgress], step: Step) -> Vec<(String, Option<u64>)> {
    said.iter()
        .filter_map(|p| match p {
            Progress::Passed(s, Found::Line { text, bytes }) if *s == step => Some((text.clone(), *bytes)),
            _ => None,
        })
        .collect()
}

/// What step `step` found when it passed.
fn found(said: &[StepProgress], step: Step) -> Option<Found> {
    said.iter().find_map(|p| match p {
        Progress::Passed(s, f) if *s == step && !matches!(f, Found::Line { .. } | Found::Removing) => Some(f.clone()),
        _ => None,
    })
}

/// Terraform's lines of a Proxmox VE run that went all the way.
const PVE_ALL_THE_WAY: [&str; 6] = [
    "proxmox_download_file.selftest_image: Still creating... [10s elapsed]",
    "proxmox_download_file.selftest_image: Creation complete after 41s [id=local:iso/x.img]",
    "proxmox_sdn_applier.selftest_net: Creation complete after 2s [id=env]",
    "proxmox_virtual_environment_vm.selftest_vm: Still creating... [10s elapsed]",
    "proxmox_virtual_environment_vm.selftest_vm: Creation complete after 63s [id=105]",
    "Apply complete! Resources: 7 added, 0 changed, 0 destroyed.",
];

#[tokio::test]
async fn a_proxmox_run_that_works_says_every_step_waits_for_ssh_and_tears_down() {
    let run = Scripted::new(&PVE_ALL_THE_WAY, None);
    // SSH answers at the second try.
    run.execs
        .lock()
        .unwrap()
        .push_back(Err(failed("ssh: connect to host 10.0.0.9 port 22: Connection refused")));
    let (said, ran) = tested(&run, false, proxmox::Pve).await;
    assert_eq!((&ran.passed, &ran.cleaned_up), (&Ok(()), &Some(Ok(()))), "{said:?}");
    assert_eq!(
        ends(&said),
        ["Prepare:ok", "Image:ok", "Network:ok", "Vm:ok", "Address:ok", "Ssh:ok", "Exec:ok", "Down:ok"]
    );
    assert_eq!(run.calls(), ["up", "true", "true", "uname -sm", "down"]);
    assert_eq!(found(&said, Step::Exec), Some(Found::Uname("Linux x86_64".into())));
    assert!(
        shown(&said, Step::Image).iter().any(|(l, _)| l.contains("Still creating")),
        "the running step shows the tool's lines"
    );
    assert_eq!(shown(&said, Step::Down), [("Destroying…".to_owned(), None)]);
}

#[tokio::test]
async fn no_address_fails_that_step_skips_the_ones_no_line_ended_and_still_tears_down() {
    let raw =
        "terraform failed: Resource postcondition failed (proxmox_virtual_environment_vm.selftest_vm): the VM got no IPv4 address on bridge `vmbr1` within 8m";
    let run = Scripted::new(&PVE_ALL_THE_WAY[..3], Some(raw));
    let (said, ran) = tested(&run, false, proxmox::Pve).await;
    assert!(matches!(&ran.passed, Err(Failure::NoAddress(r)) if r.message == raw), "{:?}", ran.passed);
    assert_eq!(ran.cleaned_up, Some(Ok(())));
    assert_eq!(ends(&said), ["Prepare:ok", "Image:ok", "Network:ok", "Vm:skip", "Address:fail", "Down:ok"]);
    assert_eq!(run.calls(), ["up", "down"], "no command after a failed start, and always the teardown");
}

#[tokio::test]
async fn an_unexplained_failure_belongs_to_the_running_step_as_the_runners_report() {
    let run = Scripted::new(&PVE_ALL_THE_WAY[..2], Some("401 Unauthorized"));
    let (said, ran) = tested(&run, false, proxmox::Pve).await;
    assert!(matches!(&ran.passed, Err(Failure::Runner(r)) if r.message == "401 Unauthorized"));
    assert_eq!(ends(&said), ["Prepare:ok", "Image:ok", "Network:fail", "Down:ok"]);
}

#[tokio::test(start_paused = true)]
async fn ssh_that_never_answers_fails_its_step_with_the_last_try() {
    let run = Scripted::new(&PVE_ALL_THE_WAY, None);
    for n in 0..100 {
        run.execs.lock().unwrap().push_back(Err(failed(&format!("ssh: try {n} refused"))));
    }
    let (said, ran) = tested(&run, false, proxmox::Pve).await;
    let Err(Failure::NoSsh(last)) = &ran.passed else { panic!("{:?}", ran.passed) };
    assert!(last.message.starts_with("ssh: try "), "{last:?}");
    assert_eq!(ends(&said)[5..], ["Ssh:fail".to_owned(), "Down:ok".to_owned()]);
    assert_eq!(ran.cleaned_up, Some(Ok(())));
}

#[tokio::test]
async fn a_command_that_fails_or_says_no_linux_fails_its_step() {
    for answer in [(1, "uname: not found"), (0, "Darwin arm64")] {
        let run = Scripted::new(&PVE_ALL_THE_WAY, None);
        run.execs.lock().unwrap().extend([Ok((0, String::new())), Ok((answer.0, answer.1.to_owned()))]);
        let (said, ran) = tested(&run, false, proxmox::Pve).await;
        assert_eq!(
            ran.passed,
            Err(Failure::Command {
                exit: answer.0,
                output: answer.1.to_owned()
            })
        );
        assert_eq!(ends(&said)[6..], ["Exec:fail".to_owned(), "Down:ok".to_owned()]);
    }
}

#[tokio::test]
async fn a_failed_teardown_is_said_and_handed_back_after_a_pass_or_a_failure() {
    let mut run = Scripted::new(&PVE_ALL_THE_WAY, None);
    run.down = Some(failed("terraform destroy failed: timeout"));
    let (said, ran) = tested(&run, false, proxmox::Pve).await;
    assert_eq!(ran.passed, Ok(()));
    assert!(matches!(&ran.cleaned_up, Some(Err(Failure::Runner(r))) if r.message.contains("timeout")));
    assert_eq!(ends(&said).last().map(String::as_str), Some("Down:fail"));
    let mut failing = Scripted::new(&[], Some("boom"));
    failing.down = Some(failed("still there"));
    let (failed_said, failed_ran) = tested(&failing, false, proxmox::Pve).await;
    assert!(failed_ran.passed.is_err() && matches!(failed_ran.cleaned_up, Some(Err(_))));
    assert_eq!(ends(&failed_said), ["Prepare:ok", "Image:fail", "Down:fail"]);
}

#[tokio::test]
async fn what_an_earlier_test_left_is_taken_down_first_and_stops_the_test_when_it_cant_be() {
    let run = Scripted::new(&PVE_ALL_THE_WAY, None);
    let (said, ran) = tested(&run, true, proxmox::Pve).await;
    assert_eq!(ran.passed, Ok(()));
    assert!(said.contains(&Progress::Passed(Step::Prepare, Found::Removing)));
    assert_eq!(run.calls().first().map(String::as_str), Some("down"), "{:?}", run.calls());
    let mut locked = Scripted::new(&PVE_ALL_THE_WAY, None);
    locked.down = Some(failed("the VM is locked"));
    let (locked_said, locked_ran) = tested(&locked, true, proxmox::Pve).await;
    assert!(matches!(&locked_ran.passed, Err(Failure::Leftover(r)) if r.message == "the VM is locked"));
    assert_eq!(locked_ran.cleaned_up, None, "the test made nothing of its own");
    assert_eq!(ends(&locked_said), ["Prepare:fail"]);
    assert_eq!(locked.calls(), ["down"]);
}

#[tokio::test]
async fn the_server_answers_on_an_open_port_and_not_on_a_closed_one() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let open = listener.local_addr().unwrap().to_string();
    assert!(answers(&open, Duration::ZERO).await);
    // Port 0 never accepts (a freed port could be taken by a parallel test).
    assert!(!answers("127.0.0.1:0", Duration::ZERO).await);
}

#[tokio::test(start_paused = true)]
async fn a_server_that_never_answers_ends_the_test_before_anything_is_made() {
    let cx = crate::testing::answered_by(FakeProcesses::script().build());
    let dir = tempfile::tempdir().unwrap();
    let mut esxi = Esxi::new("127.0.0.1", "root", SecretValue::new("pw"));
    esxi.port = 0;
    let said = Mutex::new(Vec::new());
    let sink = |p: StepProgress| said.lock().unwrap_or_else(PoisonError::into_inner).push(p);
    let vm = TestVm::new("test-vm", "probe");
    let ran = tokio::time::timeout(Duration::from_secs(120), run(&cx, dir.path(), &Server::Esxi(esxi), &vm, &sink))
        .await
        .unwrap();
    assert_eq!(ran.passed, Err(Failure::Unreachable("127.0.0.1:0".into())));
    assert_eq!(ran.cleaned_up, None);
    assert_eq!(ends(&said.into_inner().unwrap()), ["Host:fail"]);
}

/// Vagrant's and the ESXi plugin's lines of a run that went all the way, the box downloaded.
const ESXI_ALL_THE_WAY: [&str; 13] = [
    "Bringing machine 'default' up with 'vmware_esxi' provider...",
    "==> default: Adding box 'bento/debian-12' (v202510.26.0) for provider: vmware_desktop (amd64)",
    "    default: Downloading: https://vagrantcloud.com/bento/boxes/debian-12/versions/202510.26.0/providers/vmware_desktop/amd64/vagrant.box",
    "==> default: --- Box             : bento/debian-12",
    "==> default: --- Box Ver         : 202510.26.0",
    "Opening VMX source: /tmp/default.vmx",
    "Transfer Completed",
    "==> default: --- VMID            : 12",
    "==> default: --- VM has been Powered On...",
    "==> default: --- Waiting for state \"running\"",
    "==> default: --- Success, state is now \"running\"",
    "==> default: Waiting for machine to boot. This may take a few minutes...",
    "    default: SSH address: 192.0.2.40:22",
];

/// A modelled Vagrant whose ESXi plugin brings the VM up with `up`'s lines and ending.
fn vagrant(up: Result<(), &'static str>, call: &Call, state: &Mutex<bool>) -> Reply {
    let mut running = state.lock().unwrap_or_else(PoisonError::into_inner);
    let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
    match (call.program.as_str(), args.as_slice()) {
        ("vagrant", ["--version"]) => Reply::ok("Vagrant 2.4.9"),
        ("vagrant", ["plugin", "list", ..]) => {
            Reply::ok("1,,plugin-name,vagrant-vmware-esxi\n1,vagrant-vmware-esxi,plugin-version,2.5.5%!(VAGRANT_COMMA) global")
        }
        ("vagrant", ["up", ..]) => match up {
            Ok(()) => {
                *running = true;
                Reply::ok(&ESXI_ALL_THE_WAY.join("\n"))
            }
            Err(why) => Reply::fail(1, &format!("{}\n{why}", ESXI_ALL_THE_WAY[..7].join("\n"))),
        },
        ("vagrant", ["status", "--machine-readable"]) => Reply::ok(&format!(
            "1,default,provider-name,vmware_esxi\n1,default,state,{}",
            if *running { "running" } else { "not_created" }
        )),
        ("vagrant", ["ssh", "default", "-c", ..]) => Reply::ok("Linux x86_64"),
        ("vagrant", ["destroy", ..]) => {
            *running = false;
            Reply::ok("==> default: Destroying VM")
        }
        _ => Reply::ok(""),
    }
}

/// The whole test on the ESXi host this computer's loopback stands for, its Vagrant modelled.
async fn on_esxi(up: Result<(), &'static str>) -> (Vec<StepProgress>, Ran, Arc<FakeProcesses>, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state = Mutex::new(false);
    let tools = Arc::new(FakeProcesses::model(move |call| vagrant(up, call, &state)));
    let runner: Arc<dyn ProcessRunner> = Arc::<FakeProcesses>::clone(&tools);
    let cx = Context::new(runner, linux_host(Path::new("/nonexistent/isothrum-machine-test")));
    let dir = tempfile::tempdir().unwrap();
    let mut esxi = Esxi::new("127.0.0.1", "root", SecretValue::new("s3cret"));
    esxi.port = listener.local_addr().unwrap().port();
    esxi.datastore = Some("ssd1".into());
    esxi.boxes = Some(dir.path().join("no-boxes"));
    let said = Mutex::new(Vec::new());
    let sink = |p: StepProgress| said.lock().unwrap_or_else(PoisonError::into_inner).push(p);
    let vm = TestVm::new("test-vm-0123", "probe");
    let ran = run(&cx, dir.path(), &Server::Esxi(esxi), &vm, &sink).await;
    let vagrantfile = std::fs::read_to_string(dir.path().join("project").join("Vagrantfile")).unwrap();
    (said.into_inner().unwrap_or_else(PoisonError::into_inner), ran, tools, vagrantfile)
}

#[tokio::test]
async fn on_esxi_every_step_passes_through_the_runner_and_the_vm_is_destroyed() {
    let (said, ran, tools, vagrantfile) = on_esxi(Ok(())).await;
    assert_eq!((&ran.passed, &ran.cleaned_up), (&Ok(()), &Some(Ok(()))), "{said:?}");
    assert_eq!(
        ends(&said),
        ["Host:ok", "Prepare:ok", "Image:ok", "Vm:ok", "Address:ok", "Ssh:ok", "Exec:ok", "Down:ok"]
    );
    assert!(matches!(found(&said, Step::Host), Some(Found::Answers(a)) if a.starts_with("127.0.0.1:")));
    assert_eq!(
        found(&said, Step::Image),
        Some(Found::Box {
            name: "bento/debian-12".into(),
            version: Some("202510.26.0".into()),
            arch: Some("amd64".into())
        })
    );
    assert!(shown(&said, Step::Ssh).iter().any(|(l, _)| l.contains("Waiting for machine to boot")));
    assert!(
        vagrantfile.contains("v.guest_name = 'test-vm-0123'") && vagrantfile.contains("v.esxi_disk_store = 'ssd1'"),
        "{vagrantfile}"
    );
    assert!(!vagrantfile.contains("s3cret"), "the password is the run's secret, never in the file");
    let calls: Vec<(String, Vec<(String, String)>)> = tools.calls().iter().map(|c| (c.args.join(" "), c.env.clone())).collect();
    let up = calls.iter().find(|(args, _)| args.starts_with("up")).expect("vagrant up");
    assert!(up.0.contains("--provider vmware_esxi"), "{up:?}");
    assert!(
        up.1.iter().any(|(k, v)| k == "ESXI_PASSWORD" && v == "s3cret"),
        "the password reaches the plugin"
    );
    assert!(calls.iter().any(|(args, _)| args.starts_with("destroy")), "{calls:?}");
}

#[tokio::test]
async fn on_esxi_a_vm_of_the_tests_name_already_there_fails_the_upload_and_the_test_still_cleans_up() {
    let (said, ran, tools, _) = on_esxi(Err("Error: Duplicate name 'test-vm-0123'\nThere was an OVF Tool error.")).await;
    assert_eq!(ran.passed, Err(Failure::NameTaken), "{said:?}");
    assert_eq!(ran.cleaned_up, Some(Ok(())));
    assert_eq!(ends(&said), ["Host:ok", "Prepare:ok", "Image:fail", "Down:ok"]);
    assert!(tools.calls().iter().any(|c| c.args.first().is_some_and(|a| a == "destroy")));
}
