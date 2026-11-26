//! The steps an `up` says: each machine brought up, then provisioned, from what Vagrant prints.

use super::*;
use crate::formats::steps_said;

#[tokio::test]
async fn an_up_says_each_machine_up_then_provisioned() {
    let f = Fixture::new();
    let printed = "==> k3y9ab-web: Importing base box 'bento/debian-12'...
==> k3y9ab-web: Booting VM...
==> k3y9ab-web: Running provisioner: shell...
    k3y9ab-web: Running: inline script
";
    let fake = FakeProcesses::script()
        .expect_times("vagrant up --provider libvirt", 1, Reply::ok(printed))
        .build();
    let said = steps_said(cx(&f, &fake, LIBVIRT), async |cx| {
        Vagrant.up(cx).await.expect("up");
    })
    .await;
    assert_eq!(
        said,
        [
            "up:k3y9ab-web started",
            "up:k3y9ab-web done",
            "provision:k3y9ab-web started",
            "provision:k3y9ab-web done"
        ]
    );
}

#[tokio::test]
async fn a_failed_provisioning_fails_that_machines_step() {
    let f = Fixture::new();
    let failed = "==> k3y9ab-web: Booting VM...
==> k3y9ab-web: Running provisioner: shell...
The SSH command responded with a non-zero exit status. Vagrant
An error occurred while executing the action on the 'k3y9ab-web' machine. Please handle this error then try again:
";
    let fake = FakeProcesses::script()
        .allow("vagrant up --provider libvirt", Reply::fail(1, failed))
        .allow("vagrant", Reply::ok(""))
        .build();
    let said = steps_said(cx(&f, &fake, LIBVIRT), async |cx| {
        Vagrant.up(cx).await.expect_err("provisioning failed");
        cx.steps.end(false);
    })
    .await;
    assert_eq!(
        said[..4],
        [
            "up:k3y9ab-web started",
            "up:k3y9ab-web done",
            "provision:k3y9ab-web started",
            "provision:k3y9ab-web failed"
        ]
    );
}
