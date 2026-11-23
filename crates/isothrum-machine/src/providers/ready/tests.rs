use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use isothrum_runner::ProcessRunner;
use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::testing::{Stuck, StuckOn, linux_host};

/// An Apple silicon Mac whose UTM is in `/Applications`, its tools answered by `runner`.
fn mac(runner: impl ProcessRunner + 'static) -> Context {
    let mut host = linux_host(Path::new("/nonexistent/isothrum-machine-test"));
    host.os = HostOs::Macos;
    host.arch = Arch::Arm64;
    host.devices.utm_app = PathBuf::from("/Applications/UTM.app");
    Context::new(Arc::new(runner), host)
}

const OPEN: &str = "open -g -j -a /Applications/UTM.app";
const ASK: &str = "osascript -e";

/// `utm` with a 60 s wait, and how many times it said it was waiting.
async fn ready(cx: &Context) -> (Result<(), NotReady>, usize) {
    let mut waits = 0;
    let got = utm(cx, UTM_WAIT, &mut || waits += 1).await;
    (got, waits)
}

#[tokio::test(start_paused = true)]
async fn utm_is_opened_hidden_then_asked_for_its_machines() {
    let cx = mac(FakeProcesses::script().expect(OPEN, Reply::ok("")).expect(ASK, Reply::ok("A1B2")).build());
    assert_eq!(ready(&cx).await, (Ok(()), 0));
}

#[tokio::test(start_paused = true)]
async fn a_utm_still_opening_is_asked_again_and_the_wait_said_once() {
    let asked = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&asked);
    let fake = FakeProcesses::model(move |call| match call.program.as_str() {
        "open" => Reply::ok(""),
        _ if count.fetch_add(1, Ordering::SeqCst) < 3 => Reply::fail(1, "execution error: UTM got an error: Application isn't running. (-600)"),
        _ => Reply::ok(""),
    });
    assert_eq!(ready(&mac(fake)).await, (Ok(()), 1));
    assert_eq!(asked.load(Ordering::SeqCst), 4);
}

#[tokio::test(start_paused = true)]
async fn a_refused_automation_is_the_providers_need_in_any_language_and_not_waited_on() {
    for said in [
        "execution error: Not authorized to send Apple events to UTM. (-1743)",
        "erreur d’exécution : Non autorisé à envoyer des Apple Events à UTM. (-1743)",
    ] {
        let cx = mac(FakeProcesses::script()
            .expect(OPEN, Reply::ok(""))
            .expect_times(ASK, 1, Reply::fail(1, said))
            .build());
        let (got, waits) = ready(&cx).await;
        assert_eq!(got, Err(NotReady::Needs(ProviderNeed::Automation)), "{said}");
        assert_eq!((got.unwrap_err().code(), waits), (ErrorCode::ProviderRefused, 0));
    }
}

#[tokio::test(start_paused = true)]
async fn a_utm_that_cant_be_opened_is_said_so() {
    let cx = mac(FakeProcesses::script()
        .expect(OPEN, Reply::fail(1, "Unable to find application named 'UTM'"))
        .build());
    let (got, _) = ready(&cx).await;
    let err = got.unwrap_err();
    assert!(
        matches!(&err, NotReady::NotOpened { said } if said.contains("Unable to find application")),
        "{err:?}"
    );
    assert_eq!(err.code(), ErrorCode::ToolMissing);
}

#[tokio::test(start_paused = true)]
async fn a_utm_that_never_answers_is_given_up_on_after_the_wait() {
    let cx = mac(StuckOn::new(
        &ProcessSpec::new("osascript", ["-e", UTM_PROBE]).line(),
        FakeProcesses::script().expect(OPEN, Reply::ok("")).build(),
    ));
    let (got, waits) = ready(&cx).await;
    let err = got.unwrap_err();
    assert!(
        matches!(&err, NotReady::NoAnswer { waited, said } if *waited == UTM_WAIT && said == "no answer within 15 s"),
        "{err:?}"
    );
    assert_eq!((err.code(), waits), (ErrorCode::Timeout, 1));
    assert!(err.to_string().starts_with("UTM didn't answer within 60 s"), "{err}");
}

#[tokio::test(start_paused = true)]
async fn utm_is_nothing_to_open_off_a_mac() {
    let cx = crate::testing::answered_by(Stuck);
    assert_eq!(ready(&cx).await, (Ok(()), 0));
}

#[tokio::test(start_paused = true)]
async fn each_foreign_cpu_needs_its_emulator_once_and_the_hosts_own_none() {
    let cx = mac(FakeProcesses::script()
        .expect_times("qemu-system-x86_64 --version", 1, Reply::ok("QEMU emulator version 9.2.0"))
        .build());
    assert_eq!(emulators(&cx, [Arch::Arm64, Arch::Amd64, Arch::Amd64]).await, Ok(vec![Arch::Amd64]));
    let none = mac(FakeProcesses::script().build());
    assert_eq!(emulators(&none, [Arch::Arm64]).await, Ok(vec![]));
}

#[tokio::test(start_paused = true)]
async fn a_missing_emulator_is_named_with_its_cpu() {
    let cx = mac(FakeProcesses::script().expect("qemu-system-x86_64 --version", Reply::missing()).build());
    let err = emulators(&cx, [Arch::Amd64]).await.unwrap_err();
    assert_eq!(
        err,
        NotReady::Emulator {
            arch: Arch::Amd64,
            program: "qemu-system-x86_64"
        }
    );
    assert_eq!(
        (err.code(), err.to_string().as_str()),
        (ErrorCode::ToolMissing, "QEMU's emulator qemu-system-x86_64 isn't installed")
    );
}

#[test]
fn the_last_apple_event_number_is_read() {
    assert_eq!(apple_event("UTM got an error: AppleEvent timed out. (-1712)\n(-1743)"), Some(-1743));
    assert_eq!(apple_event("Can’t get virtual machine \"x(-y)\". (-1728)"), Some(-1728));
    assert_eq!(apple_event("exited with code 1"), None);
}
