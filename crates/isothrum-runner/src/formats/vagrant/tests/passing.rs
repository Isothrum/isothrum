//! Failures that pass: UTM's Apple events, VMware's host-only network said not enabled. An `up`
//! they cut short is made once more.

use super::*;

const UTM: Option<Provider> = Some(Provider::Utm);
const VMWARE: Option<Provider> = Some(Provider::VmwareDesktop);

/// What VMware desktop's plugin said (3.0.5, utility 1.0.24, Fusion on macOS 26) when its first
/// private network's host-only device was made but reported not enabled.
const VMNET_NOT_ENABLED: &str = "vagrant/vmware-desktop-3.0.5-vmnet-not-enabled.txt";

#[tokio::test]
async fn a_utm_up_an_apple_event_cut_short_in_passing_runs_once_more_and_says_so() {
    for said in [
        "osascript/vagrant_utm-forwarded-ports-not-running.txt",
        "osascript/vagrant_utm-forwarded-ports-timed-out.txt",
    ] {
        let f = Fixture::new();
        let fake = failing_first(1, said);
        Vagrant.up(&cx(&f, &fake, UTM)).await.expect("through the second time");
        assert_eq!(verbs(&fake), ["vagrant up", "vagrant up"], "{said}");
        assert_eq!(f.warnings(), [WarningCode::Retrying], "{said}");
    }
    // An update is the same `up`, provisioning.
    let f = Fixture::new();
    let fake = failing_first(1, "osascript/vagrant_utm-forwarded-ports-not-running.txt");
    Vagrant.update(&cx(&f, &fake, UTM)).await.expect("through the second time");
    assert_eq!(verbs(&fake), ["vagrant up", "vagrant up"]);
}

#[tokio::test]
async fn a_second_passing_failure_or_one_on_another_provider_is_the_up_s_failure() {
    let f = Fixture::new();
    let fake = failing_first(2, "osascript/vagrant_utm-forwarded-ports-not-running.txt");
    let e = Vagrant.up(&cx(&f, &fake, UTM)).await.expect_err("tried once more only");
    assert_eq!(verbs(&fake), ["vagrant up", "vagrant up"]);
    assert!(
        matches!(&e, Error::ToolFailed { reason, .. } if reason.contains("(-600)")),
        "the tool's words: {e}"
    );
    let other = Fixture::new();
    let on_libvirt = failing_first(1, "osascript/vagrant_utm-forwarded-ports-not-running.txt");
    Vagrant.up(&cx(&other, &on_libvirt, LIBVIRT)).await.expect_err("not UTM: no retry");
    assert_eq!(verbs(&on_libvirt), ["vagrant up"]);
    assert_eq!(other.warnings(), []);
}

#[tokio::test]
async fn a_vmware_up_whose_new_host_only_network_was_said_not_enabled_runs_once_more_and_says_so() {
    let f = Fixture::new();
    let fake = failing_first(1, VMNET_NOT_ENABLED);
    Vagrant.up(&cx(&f, &fake, VMWARE)).await.expect("through the second time: the network is there");
    // Vagrant's calls only: on a Windows host the up then reads VMware's host adapters too.
    let vagrant: Vec<String> = verbs(&fake).into_iter().filter(|v| v.starts_with("vagrant")).collect();
    assert_eq!(vagrant, ["vagrant up", "vagrant up"]);
    assert_eq!(f.warnings(), [WarningCode::Retrying]);
}

#[tokio::test]
async fn a_host_only_network_said_not_enabled_twice_or_on_another_provider_is_the_up_s_failure() {
    let f = Fixture::new();
    let fake = failing_first(2, VMNET_NOT_ENABLED);
    let e = Vagrant.up(&cx(&f, &fake, VMWARE)).await.expect_err("tried once more only");
    assert_eq!(verbs(&fake), ["vagrant up", "vagrant up"]);
    assert!(
        matches!(&e, Error::ToolFailed { reason, .. } if reason.contains("Failed to enable device")),
        "the tool's words: {e}"
    );
    // Only VMware's own: the same words through another provider (and UTM's passing failures
    // on VMware) aren't retried.
    for (said, provider) in [(VMNET_NOT_ENABLED, UTM), ("osascript/vagrant_utm-forwarded-ports-not-running.txt", VMWARE)] {
        let other = Fixture::new();
        let once = failing_first(1, said);
        Vagrant.up(&cx(&other, &once, provider)).await.expect_err("not the provider's own: no retry");
        assert_eq!(verbs(&once), ["vagrant up"], "{said}");
        assert_eq!(other.warnings(), [], "{said}");
    }
}
