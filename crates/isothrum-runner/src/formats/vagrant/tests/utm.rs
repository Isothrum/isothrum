//! UTM's passing Apple-event failures: an `up` they cut short is made once more.

use super::*;

const UTM: Option<Provider> = Some(Provider::Utm);

/// Vagrant whose `up` fails with `fixture` its first `failing` times, then succeeds: an answer
/// that changes over time, which a script doesn't give.
fn failing_first(failing: usize, fixture_name: &str) -> FakeProcesses {
    let said = fixture(fixture_name);
    let tries = std::sync::atomic::AtomicUsize::new(0);
    FakeProcesses::model(move |_| {
        if tries.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < failing {
            Reply::fail(1, &said)
        } else {
            Reply::ok("")
        }
    })
}

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
