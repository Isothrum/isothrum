//! Requests cut short, and a broker that ends mid-request.

use super::*;

/// Regression: a request cut short (a probe's timeout) ended the broker's whole tree and the
/// session: every later call of the operation failed with `elevation`.
#[tokio::test]
async fn a_request_cut_short_cancels_itself_alone_and_the_session_goes_on() {
    let (_home, fake, e) = session("", Reply::ok(""));
    let root = e.root.clone();
    let broker = tokio::spawn(broker(root.clone(), "ran", 0));
    let cut = tokio::time::timeout(Duration::from_millis(600), lines_of(&e, &ProcessSpec::new("vagrant", ["hang"]))).await;
    assert!(cut.is_err(), "nobody answered");
    let dir = session_dir(&root).await;
    // The broker ends request 1 alone and deletes its files.
    let (exit, lines) = lines_of(&e, &ProcessSpec::new("vagrant", ["halt"])).await;
    assert!(exit.unwrap().success(), "{lines:?}");
    assert_eq!(lines[0], "vagrant halt");
    assert!(!dir.join(protocol::cancel(1)).exists() && !dir.join(protocol::output(1)).exists());
    assert_eq!(fake.lines().iter().filter(|l| l.contains("Start-Process")).count(), 1, "one prompt");
    drop(e);
    assert!(!root.exists(), "the session's files (its values) are gone");
    broker.await.unwrap();
}

/// Two requests at once in one session: the one waiting behind the other is cut short (its
/// step's limit), and neither the one running nor any later one is touched.
#[tokio::test]
async fn two_requests_at_once_one_cut_short_leave_the_other_answered() {
    let (_home, fake, e) = session("", Reply::ok(""));
    let root = e.root.clone();
    let broker = tokio::spawn(broker(root.clone(), "ran", 0));
    let (opened, _) = lines_of(&e, &ProcessSpec::new("vagrant", ["status"])).await;
    assert!(opened.unwrap().success());
    let (slow, ssh) = (ProcessSpec::new("vagrant", ["slow"]), ProcessSpec::new("vagrant", ["ssh"]));
    let running = lines_of(&e, &slow);
    let behind = tokio::time::timeout(Duration::from_millis(200), lines_of(&e, &ssh));
    let ((ran, lines), cut) = tokio::join!(running, behind);
    assert!(ran.unwrap().success(), "{lines:?}");
    assert_eq!(lines[..2], ["vagrant slow".to_owned(), "ran".to_owned()]);
    assert!(cut.is_err(), "the request behind was cut short");
    let (later, later_lines) = lines_of(&e, &ProcessSpec::new("vagrant", ["halt"])).await;
    assert!(later.unwrap().success(), "{later_lines:?}");
    assert_eq!(later_lines[0], "vagrant halt");
    let dir = session_dir(&root).await;
    let left = files_left(&dir);
    assert_eq!(left, [protocol::READY], "the cut request's files went too");
    assert_eq!(fake.lines().iter().filter(|l| l.contains("Start-Process")).count(), 1, "one prompt");
    drop(e);
    broker.await.unwrap();
}

/// A broker that starts, then sets `obstacle` (given the session's folder) instead of answering.
pub(super) async fn started_then(root: PathBuf, obstacle: fn(&Path)) {
    let dir = session_dir(&root).await;
    obstacle(&dir);
    std::fs::write(dir.join(protocol::READY), "").unwrap();
}

/// What a call through `e` gives within 5 s (a call that waits longer fails the test).
pub(super) async fn within_5s(e: &Elevated, spec: &ProcessSpec) -> Result<Exit, ProcessError> {
    let call = tokio::time::timeout(Duration::from_secs(5), lines_of(e, spec)).await;
    call.expect("the call waited for an answer that won't come").0
}

#[tokio::test]
async fn a_broker_that_ends_mid_request_fails_it_at_once_and_the_session_with_it() {
    // Regression: a request waited for its `exit-<n>` with no sign of a broker gone, until its
    // step's limit (a `timeout`, after up to hours).
    let (_home, _fake, e) = session("", Reply::ok(""));
    let root = e.root.clone();
    let broker = tokio::spawn(async move {
        let dir = session_dir(&root).await;
        std::fs::write(dir.join(protocol::READY), "").unwrap();
        while !dir.join(protocol::request(1)).exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        std::fs::write(dir.join(protocol::ENDED), "the elevated broker stopped\r\n").unwrap();
    });
    let spec = ProcessSpec::new("vagrant", ["up"]);
    let err = within_5s(&e, &spec).await.unwrap_err();
    assert_eq!(err.to_string(), "elevation: the elevated session ended: the elevated broker stopped");
    broker.await.unwrap();
    let later = within_5s(&e, &spec).await.unwrap_err();
    assert!(
        matches!(&later, ProcessError::Elevation { reason } if reason.contains("broker stopped")),
        "{later}"
    );
}

#[tokio::test]
async fn a_request_that_cant_be_written_cancels_itself_alone() {
    // Regression: request 1 failed to be written, and the broker went on waiting for it: every
    // later request (2, 3, …) waited for an answer that never came. Then its failure ended the
    // whole session instead.
    let (_home, _fake, e) = session("", Reply::ok(""));
    let broker = tokio::spawn(broker_after(
        e.root.clone(),
        |dir| std::fs::create_dir_all(dir.join(protocol::request(1)).join("in-the-way")).unwrap(),
        "ran",
        0,
    ));
    let first = within_5s(&e, &ProcessSpec::new("vagrant", ["up"])).await.unwrap_err();
    assert!(matches!(first, ProcessError::Io { .. }), "{first}");
    let second = within_5s(&e, &ProcessSpec::new("vagrant", ["halt"])).await;
    assert!(second.unwrap().success(), "the broker skipped request 1 and took request 2");
    drop(e);
    broker.await.unwrap();
}
