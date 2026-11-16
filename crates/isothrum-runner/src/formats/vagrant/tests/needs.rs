//! A hypervisor that finds nothing this computer must have for the run, or that the system
//! refuses: `provider_refused`, naming what it needs.

use super::*;

/// The detail of the error `vagrant up` on `provider` fails with when it prints fixture `said`.
async fn refusal(provider: Provider, said: &str) -> Option<crate::Detail> {
    let f = Fixture::new();
    let fake = FakeProcesses::script().expect("vagrant up", Reply::fail(1, &fixture(said))).build();
    let e = Vagrant.up(&cx(&f, &fake, Some(provider))).await.expect_err(said);
    assert_eq!(e.code(), crate::ErrorCode::ProviderRefused, "{e}");
    e.detail().map(|d| d.data)
}

/// `provider_refused` by `provider`, needing `need`.
fn needs(provider: Provider, need: crate::ProviderNeed) -> Option<crate::Detail> {
    Some(crate::Detail::ProviderRefused {
        provider: provider.id().to_owned(),
        need: Some(need),
    })
}

#[tokio::test]
async fn a_hypervisor_finding_no_virtiofsd_refuses_the_provider_naming_it() {
    for (provider, said) in [
        (Provider::Libvirt, "vagrant/vagrant-libvirt-no-virtiofsd.txt"),
        (Provider::Qemu, "vagrant/vagrant-qemu-0.8-no-virtiofsd.txt"),
    ] {
        assert_eq!(refusal(provider, said).await, needs(provider, crate::ProviderNeed::Virtiofsd), "{said}");
    }
}

/// UTM answering the VM's start with the system's `Operation not permitted`.
#[tokio::test]
async fn a_hypervisor_app_refused_by_the_system_refuses_the_provider_needing_a_permission() {
    let said = "vagrant/vagrant_utm-0.2-start-not-permitted.txt";
    assert_eq!(refusal(Provider::Utm, said).await, needs(Provider::Utm, crate::ProviderNeed::Permission));
}
