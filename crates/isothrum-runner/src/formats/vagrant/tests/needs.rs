//! A hypervisor that finds nothing this computer must have for the run: `provider_refused`,
//! naming what it needs.

use super::*;

#[tokio::test]
async fn a_hypervisor_finding_no_virtiofsd_refuses_the_provider_naming_it() {
    for (provider, said) in [
        (Provider::Libvirt, "vagrant/vagrant-libvirt-no-virtiofsd.txt"),
        (Provider::Qemu, "vagrant/vagrant-qemu-0.8-no-virtiofsd.txt"),
    ] {
        let f = Fixture::new();
        let fake = FakeProcesses::script().expect("vagrant up", Reply::fail(1, &fixture(said))).build();
        let e = Vagrant.up(&cx(&f, &fake, Some(provider))).await.expect_err("no virtiofsd");
        assert_eq!(e.code(), crate::ErrorCode::ProviderRefused, "{e}");
        assert_eq!(
            e.detail().map(|d| d.data),
            Some(crate::Detail::ProviderRefused {
                provider: provider.id().to_owned(),
                need: Some(crate::ProviderNeed::Virtiofsd),
            }),
            "{said}"
        );
    }
}

#[tokio::test]
async fn a_hypervisor_app_refused_by_the_system_refuses_the_provider_needing_a_permission() {
    let f = Fixture::new();
    let said = fixture("vagrant/vagrant_utm-0.2-start-not-permitted.txt");
    let fake = FakeProcesses::script().expect("vagrant up", Reply::fail(1, &said)).build();
    let e = Vagrant.up(&cx(&f, &fake, Some(Provider::Utm))).await.expect_err("UTM refused the start");
    assert_eq!(e.code(), crate::ErrorCode::ProviderRefused, "{e}");
    assert_eq!(
        e.detail().map(|d| d.data),
        Some(crate::Detail::ProviderRefused {
            provider: Provider::Utm.id().to_owned(),
            need: Some(crate::ProviderNeed::Permission),
        })
    );
    assert!(e.to_string().contains("Operation not permitted"), "what UTM said is kept: {e}");
}
