//! VMware's host adapter on Windows given its address after an `up` ([`vmnet`]).

use super::*;
use crate::formats::vagrant::vmnet::{address_host_adapters, unaddressed, update_script};
use crate::process::DECLINED;
use crate::project::platform::HostOs;

const VMWARE: Option<Provider> = Some(Provider::VmwareDesktop);

fn lines(s: &[&str]) -> Vec<String> {
    s.iter().map(|l| (*l).to_owned()).collect()
}

#[test]
fn only_a_vmnet_adapter_with_nothing_but_an_automatic_address_is_unaddressed() {
    let listing = lines(&[
        "VMware Network Adapter VMnet1\t192.168.80.1",
        "VMware Network Adapter VMnet8\t192.168.17.1",
        "VMware Network Adapter VMnet2\t169.254.33.7",
        "VMware Network Adapter VMnet3\t169.254.1.2",
        "VMware Network Adapter VMnet3\t192.168.212.1",
        "Ethernet\t169.254.9.9",
        "VMware Network Adapter VMnet9; rm -rf\t169.254.1.1",
        "garbage",
    ]);
    assert_eq!(unaddressed(&listing), ["vmnet2"]);
}

#[test]
fn the_elevated_script_names_vmnets_only() {
    let script = update_script(&["vmnet2".into(), "vmnet10".into(), "x'; evil".into()]);
    assert!(script.contains("foreach ($n in @('vmnet2','vmnet10'))"), "{script}");
    assert!(!script.contains("evil"), "{script}");
    assert!(script.contains("& $v -- update adapter $n"), "{script}");
}

/// The fake's answers: the listing before VMware's update, then after it.
fn adapters(before: &'static str, after: &'static str, update: Reply) -> FakeProcesses {
    let updated = std::sync::atomic::AtomicBool::new(false);
    let update = std::sync::Mutex::new(Some(update));
    FakeProcesses::model(move |c| {
        let script = c.args.last().map_or("", String::as_str);
        if script.contains("Get-NetIPAddress") {
            Reply::ok(if updated.load(std::sync::atomic::Ordering::SeqCst) { after } else { before })
        } else {
            updated.store(true, std::sync::atomic::Ordering::SeqCst);
            update.lock().unwrap().take().unwrap_or_else(|| Reply::fail(9, "called twice"))
        }
    })
}

fn elevations(fake: &FakeProcesses) -> usize {
    fake.calls().iter().filter(|c| c.args.last().is_some_and(|a| a.contains("-Verb RunAs"))).count()
}

#[tokio::test(start_paused = true)]
async fn an_adapter_left_without_its_address_is_updated_once_as_an_administrator() {
    let f = Fixture::new();
    let fake = adapters(
        "VMware Network Adapter VMnet2\t169.254.33.7\n",
        "VMware Network Adapter VMnet2\t192.168.212.1\n",
        Reply::ok(""),
    );
    address_host_adapters(&cx(&f, &fake, VMWARE), HostOs::Windows).await;
    assert_eq!(elevations(&fake), 1, "one prompt");
    assert_eq!(f.warnings(), []);
}

#[tokio::test(start_paused = true)]
async fn nothing_is_elevated_when_every_adapter_has_its_address_or_off_windows_or_vmware() {
    let f = Fixture::new();
    let fake = adapters("VMware Network Adapter VMnet2\t192.168.212.1\n", "", Reply::ok(""));
    address_host_adapters(&cx(&f, &fake, VMWARE), HostOs::Windows).await;
    assert_eq!(elevations(&fake), 0);

    let apipa = "VMware Network Adapter VMnet2\t169.254.33.7\n";
    for (host, provider) in [(HostOs::Linux, VMWARE), (HostOs::Macos, VMWARE), (HostOs::Windows, Some(Provider::Hyperv))] {
        let other = adapters(apipa, apipa, Reply::ok(""));
        address_host_adapters(&cx(&f, &other, provider), host).await;
        assert_eq!(other.calls().len(), 0, "{host:?} {provider:?}");
    }
    assert_eq!(f.warnings(), []);
}

#[tokio::test(start_paused = true)]
async fn a_declined_prompt_or_an_update_that_didnt_take_is_a_warning_and_the_start_stands() {
    let apipa = "VMware Network Adapter VMnet2\t169.254.33.7\n";
    for (update, said) in [
        (Reply::fail(DECLINED, ""), "declined"),
        (Reply::fail(2, ""), "vnetlib.exe"),
        (Reply::ok(""), "still has no address"),
    ] {
        let f = Fixture::new();
        let fake = adapters(apipa, apipa, update);
        address_host_adapters(&cx(&f, &fake, VMWARE), HostOs::Windows).await;
        let warned = f.warned.lock().unwrap().clone();
        assert_eq!(warned.len(), 1, "{said}: {warned:?}");
        assert_eq!(warned[0].0, WarningCode::HostAdapterUnaddressed);
        assert!(warned[0].1.contains(said) && warned[0].1.contains("vmnet2"), "{said}: {}", warned[0].1);
    }
}
