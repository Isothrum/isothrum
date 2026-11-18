//! A machine still booting: an `up` that provisions it before its SSH server answers is made
//! again once it may.

use super::*;

const SAID: &str = "vagrant/2.4.9-ssh-version-negotiation-timeout.txt";

#[tokio::test(start_paused = true)]
async fn an_up_provisioning_a_machine_that_still_boots_runs_again_once_it_may_answer() {
    let f = Fixture::new();
    let fake = failing_first(2, SAID);
    Vagrant.up(&cx(&f, &fake, LIBVIRT)).await.expect("through once the machine answers");
    assert_eq!(verbs(&fake), ["vagrant up", "vagrant up", "vagrant up"]);
    assert_eq!(f.warnings(), [WarningCode::Retrying], "said once");
}

#[tokio::test(start_paused = true)]
async fn a_machine_that_never_answers_is_the_up_s_failure_in_the_tool_s_words() {
    let f = Fixture::new();
    let fake = failing_first(usize::MAX, SAID);
    let e = Vagrant.up(&cx(&f, &fake, LIBVIRT)).await.expect_err("it never answers");
    assert_eq!(verbs(&fake).len(), 9, "the first try and eight more");
    assert!(
        matches!(&e, Error::ToolFailed { reason, .. } if reason.contains("server version negotiating")),
        "{e}"
    );
}

/// Regression: a start again took over the VM a killed start had made, still booting, and
/// Vagrant, which marks a machine provisioned before its provisioners run, skipped its set-up on
/// the next try: the run came up without it. Once a VM of the run is recorded, a start provisions
/// (`--provision`) on every try; a fresh run's start provisions as Vagrant does by default.
#[tokio::test(start_paused = true)]
async fn a_start_taking_over_an_earlier_starts_vm_provisions_it_on_every_try() {
    let provisions = |fake: &FakeProcesses| -> Vec<bool> {
        fake.calls()
            .iter()
            .filter(|c| c.args.first().is_some_and(|a| a == "up"))
            .map(|c| c.args.iter().any(|a| a == "--provision"))
            .collect()
    };
    let fresh = Fixture::new();
    let first = failing_first(0, SAID);
    Vagrant.up(&cx(&fresh, &first, LIBVIRT)).await.expect("a fresh start");
    assert_eq!(provisions(&first), [false], "Vagrant's default");

    let f = Fixture::new();
    f.made("web", "libvirt", "dom-web");
    let fake = failing_first(1, SAID);
    Vagrant.up(&cx(&f, &fake, LIBVIRT)).await.expect("through once it answers");
    assert_eq!(provisions(&fake), [true, true], "provisioned on the try that went through");
}
