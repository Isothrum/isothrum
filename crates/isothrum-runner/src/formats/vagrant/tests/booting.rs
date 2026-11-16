//! A machine still booting: an `up` that provisions it before its SSH server answers is made
//! again once it may.

use super::*;

const SAID: &str = "vagrant/2.4.9-ssh-version-negotiation-timeout.txt";

/// Vagrant whose `up` fails with `SAID` its first `failing` times, then succeeds.
fn failing_first(failing: usize) -> FakeProcesses {
    let said = fixture(SAID);
    let tries = std::sync::atomic::AtomicUsize::new(0);
    FakeProcesses::model(move |_| {
        if tries.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < failing {
            Reply::fail(1, &said)
        } else {
            Reply::ok("")
        }
    })
}

#[tokio::test(start_paused = true)]
async fn an_up_provisioning_a_machine_that_still_boots_runs_again_once_it_may_answer() {
    let f = Fixture::new();
    let fake = failing_first(2);
    Vagrant.up(&cx(&f, &fake, LIBVIRT)).await.expect("through once the machine answers");
    assert_eq!(verbs(&fake), ["vagrant up", "vagrant up", "vagrant up"]);
    assert_eq!(f.warnings(), [WarningCode::Retrying], "said once");
}

#[tokio::test(start_paused = true)]
async fn a_machine_that_never_answers_is_the_up_s_failure_in_the_tool_s_words() {
    let f = Fixture::new();
    let fake = failing_first(usize::MAX);
    let e = Vagrant.up(&cx(&f, &fake, LIBVIRT)).await.expect_err("it never answers");
    assert_eq!(verbs(&fake).len(), 9, "the first try and eight more");
    assert!(
        matches!(&e, Error::ToolFailed { reason, .. } if reason.contains("server version negotiating")),
        "{e}"
    );
}
