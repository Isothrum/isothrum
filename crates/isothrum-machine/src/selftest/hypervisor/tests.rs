//! The hypervisor self-test on a modelled Vagrant: each step said in order, the VM taken down
//! after it (and after a failure), the networks it made removed, the ping skipped where no
//! private network is.

use std::sync::{Arc, Mutex, PoisonError};

use isothrum_runner::ProcessRunner;
use isothrum_runner::fake::{Call, FakeProcesses, Reply};

use super::*;
use crate::testing::linux_host;

/// What the modelled machine holds: whether the VM runs, and whether its boot fails.
#[derive(Default)]
struct Machine {
    up: bool,
    /// The provider it was started on.
    on: String,
    boot_fails: bool,
    /// VirtualBox's host-only adapters.
    ifs: Vec<&'static str>,
}

fn machine(m: &Mutex<Machine>, call: &Call) -> Reply {
    let mut m = m.lock().unwrap_or_else(PoisonError::into_inner);
    let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
    match (call.program.as_str(), args.as_slice()) {
        ("vagrant", ["--version"]) => Reply::ok("Vagrant 2.4.9"),
        ("vagrant", ["plugin", "list", "--machine-readable"]) => {
            Reply::ok("1,,plugin-name,vagrant-vmware-esxi\n1,vagrant-vmware-esxi,plugin-version,2.5.5%!(VAGRANT_COMMA) global")
        }
        ("vagrant", ["box", "list"]) => Reply::ok("generic/alpine319 (virtualbox, 4.3.12)\n"),
        ("vagrant", ["up", ..]) if m.boot_fails => Reply::fail(1, "VBoxManage: error: VT-x is not available (VERR_VMX_NO_VMX)"),
        ("vagrant", ["up", "--provider", on, ..]) => {
            m.up = true;
            m.on = (*on).to_owned();
            m.ifs.push("vboxnet1");
            Reply::ok("==> default: Booting VM...")
        }
        ("vagrant", ["status", "--machine-readable"]) => Reply::ok(&format!(
            "1,default,provider-name,{}\n1,default,state,{}",
            m.on,
            if m.up { "running" } else { "not_created" }
        )),
        ("vagrant", ["ssh", "default", "-c", ..]) => Reply::ok("Linux 6.6.4 x86_64"),
        ("vagrant", ["destroy", ..]) => {
            m.up = false;
            Reply::ok("")
        }
        // VMware's `vmrun` isn't on `PATH`: it is in its app only.
        ("vmrun", _) => Reply::missing(),
        ("VBoxManage", ["list", "hostonlyifs"]) => Reply::ok(&m.ifs.iter().map(|i| ["Name: ", i, "\n"].concat()).collect::<String>()),
        // Every test address is free until the VM boots on the first.
        ("ping", [.., address]) if m.up && *address == ADDRESSES[0] => Reply::ok(&format!("64 bytes from {address}: icmp_seq=1 ttl=64 time=0.412 ms")),
        ("ping", _) => Reply::fail(1, "1 packets transmitted, 0 received, 100% packet loss"),
        _ => Reply::ok(""),
    }
}

/// The caller's choices: VirtualBox, its box held.
struct Picks(Provider);

impl Choices for Picks {
    fn provider(&self) -> BoxFuture<'_, Result<Provider, String>> {
        let p = self.0;
        Box::pin(async move { Ok(p) })
    }

    fn fetch_box<'a>(&'a self, _: Provider, _: &'static str, line: &'a (dyn Fn(String) + Send + Sync)) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            line("held".to_owned());
            Ok(true)
        })
    }
}

/// The test on the modelled machine with `provider` picked: what it said, how it ran, the calls.
async fn tested(provider: Provider, boot_fails: bool) -> (Vec<StepProgress>, Ran, Arc<FakeProcesses>) {
    tested_on(linux_host(Path::new("/nonexistent/isothrum-machine-test")), provider, boot_fails).await
}

/// [`tested`] on `host`.
async fn tested_on(host: crate::Host, provider: Provider, boot_fails: bool) -> (Vec<StepProgress>, Ran, Arc<FakeProcesses>) {
    let state = Mutex::new(Machine {
        boot_fails,
        ifs: vec!["vboxnet0"],
        ..Machine::default()
    });
    let tools = Arc::new(FakeProcesses::model(move |call| machine(&state, call)));
    let runner: Arc<dyn ProcessRunner> = Arc::<FakeProcesses>::clone(&tools);
    let cx = Context::new(runner, host);
    let dir = tempfile::tempdir().expect("a folder");
    let said = Mutex::new(Vec::new());
    let sink = |p: StepProgress| said.lock().unwrap_or_else(PoisonError::into_inner).push(p);
    let vm = TestVm::new("Isothrum test VM", "isothrum-selftest");
    let ran = run(&cx, dir.path(), &vm, &Picks(provider), &sink).await;
    let said = said.into_inner().unwrap_or_else(PoisonError::into_inner);
    (said, ran, tools)
}

/// The steps that passed or were skipped, each with what it found, lines aside.
fn ended(said: &[StepProgress]) -> Vec<(Step, Found)> {
    said.iter()
        .filter_map(|p| match p {
            Progress::Passed(_, Found::Line(_)) => None,
            Progress::Passed(step, found) | Progress::Skipped(step, found) => Some((*step, found.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn every_step_passes_in_order_then_the_vm_and_its_networks_go() {
    let (said, ran, tools) = tested(Provider::Virtualbox, false).await;
    assert_eq!((&ran.passed, &ran.cleaned_up), (&Ok(()), &Some(Ok(()))), "{said:?}");
    assert_eq!(
        ended(&said),
        [
            (Step::Vagrant, Found::Version("Vagrant 2.4.9".into())),
            (Step::Provider, Found::Provider(Provider::Virtualbox)),
            (Step::Hypervisor, Found::Responds),
            (
                Step::Box,
                Found::Box {
                    name: "generic/alpine319",
                    held: true
                }
            ),
            (
                Step::Boot,
                Found::Booted {
                    name: "generic/alpine319",
                    provider: Provider::Virtualbox
                }
            ),
            (Step::Exec, Found::Uname("Linux 6.6.4 x86_64".into())),
            (Step::Network, Found::Reached { address: ADDRESSES[0] }),
            (Step::Networks, Found::NetworksRemoved(vec!["vboxnet1".into()])),
        ]
    );
    assert!(
        said.contains(&Progress::Passed(Step::Box, Found::Line("held".into()))),
        "the box's lines are said"
    );
    let calls: Vec<String> = tools.calls().iter().map(|c| format!("{} {}", c.program, c.args.join(" "))).collect();
    assert!(calls.iter().any(|c| c.starts_with("vagrant destroy")), "taken down: {calls:?}");
    assert!(calls.contains(&"VBoxManage hostonlyif remove vboxnet1".to_owned()), "{calls:?}");
    assert!(!calls.iter().any(|c| c.contains("remove vboxnet0")), "a network there before stays");
}

#[tokio::test]
async fn a_boot_that_fails_ends_the_test_and_the_vm_still_goes() {
    let (said, ran, tools) = tested(Provider::Virtualbox, true).await;
    assert!(
        matches!(said.iter().rev().find(|p| matches!(p, Progress::Failed(..))), Some(Progress::Failed(Step::Boot, Failure::Runner(e))) if e.to_string().contains("VT-x")),
        "{said:?}"
    );
    assert!(ran.passed.is_err());
    assert!(
        tools.calls().iter().any(|c| c.args.first().is_some_and(|a| a == "destroy")),
        "taken down all the same"
    );
}

#[tokio::test]
async fn a_hypervisor_with_no_private_network_skips_the_ping() {
    // ESXi's plugin gives no private network, and runs on every system the test runs on.
    let (said, ran, _) = tested(Provider::VmwareEsxi, false).await;
    assert_eq!(ran.passed, Ok(()), "{said:?}");
    assert!(
        said.contains(&Progress::Skipped(Step::Network, Found::NoPrivateNetwork(Provider::VmwareEsxi))),
        "{said:?}"
    );
}

#[tokio::test]
async fn utm_is_opened_hidden_and_asked_as_a_start_asks_it_never_by_utmctl() {
    // Only the hypervisor's step matters here.
    let mut mac = linux_host(Path::new("/nonexistent/isothrum-machine-test"));
    mac.os = isothrum_runner::HostOs::Macos;
    let (said, _, tools) = tested_on(mac, Provider::Utm, false).await;
    assert!(said.contains(&Progress::Passed(Step::Hypervisor, Found::Responds)), "{said:?}");
    let calls = tools.calls();
    assert!(
        calls.iter().any(|c| c.program == "open" && c.args.starts_with(&["-g".into(), "-j".into()])),
        "{calls:?}"
    );
    assert!(calls.iter().any(|c| c.program == "osascript"), "{calls:?}");
    assert!(
        !calls.iter().any(|c| c.program.ends_with("utmctl")),
        "never `utmctl list`, which opens UTM's window"
    );
}

#[test]
fn the_vagrantfile_names_the_vm_and_marks_it_as_the_tests() {
    let vm = TestVm::new("Isothrum test VM", "isothrum-selftest");
    let dir = Path::new("/data/selftest/vm");
    let v = vagrantfile(&vm, Provider::Virtualbox, "generic/alpine319", Some(ADDRESSES[0]), dir);
    assert!(
        v.contains("config.vm.box = \"generic/alpine319\"") && v.contains("config.vm.hostname = \"isothrum-selftest\""),
        "{v}"
    );
    assert!(v.contains("ip: \"192.168.56.250\"") && v.contains("h.name = \"Isothrum test VM\""), "{v}");
    assert!(v.contains("\"--description\", 'isothrum-selftest:/data/selftest/vm'"), "{v}");
    let hyperv = vagrantfile(&vm, Provider::Hyperv, "generic/alpine319", Some(ADDRESSES[0]), dir);
    assert!(
        !hyperv.contains("private_network") && hyperv.contains("h.vmname = \"Isothrum test VM\""),
        "{hyperv}"
    );
    // Nor a hostname, whose DHCP renewal takes the VM off its SSH session's address on Hyper-V.
    assert!(!hyperv.contains("config.vm.hostname"), "{hyperv}");
    // Never Vagrant's question of which switch, which nobody answers in a run.
    assert!(hyperv.contains("config.vm.network \"public_network\", bridge: \"Default Switch\""), "{hyperv}");
    assert!(!v.contains("public_network"), "{v}");
    assert_eq!(uname_line("\n Linux 6.6 x86_64\n"), "Linux 6.6 x86_64");
}

#[tokio::test]
async fn a_hypervisor_tool_off_path_is_asked_where_its_app_installs_it() {
    let dir = tempfile::tempdir().expect("a folder");
    let app = dir.path().join("vmrun");
    std::fs::write(&app, "").expect("the app's vmrun");
    let mut host = linux_host(Path::new("/nonexistent/isothrum-machine-test"));
    host.devices.vmrun = vec![app.clone()];
    let (said, _, tools) = tested_on(host, Provider::VmwareDesktop, false).await;
    assert!(ended(&said).contains(&(Step::Hypervisor, Found::Responds)), "{said:?}");
    assert_eq!(tools.made(&format!("{} list", app.display())).len(), 1);
}
