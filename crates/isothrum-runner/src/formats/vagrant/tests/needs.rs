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
