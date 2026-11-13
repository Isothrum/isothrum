//! The reservations across runners and processes of one home.

use std::io::{BufRead as _, BufReader};
use std::process::{Command, Stdio};

use super::*;
use crate::net::FakePorts;

/// The variable that makes [`a_reserving_child`] reserve a port in the home it names, and hold it
/// until it is killed.
const CHILD_HOME: &str = "ISOTHRUM_TEST_RESERVING_CHILD_HOME";

/// The port the child reserves.
const CHILD_PORT: u16 = 8080;

fn reservations(dir: &Path, free: impl IntoIterator<Item = u16>) -> PortReservations {
    PortReservations::new(Arc::new(FakePorts::new(free)), dir.to_path_buf())
}

fn run(name: &str) -> RunName {
    RunName::new(name).unwrap()
}

#[test]
fn two_runners_of_one_home_cant_both_reserve_a_port() {
    // Regression: the reservations lived in one runner's memory, so two starts in two processes
    // both took a preferred 8080 and the second failed at the hypervisor. Two runners stand in
    // for two processes: each its own handles on the lock files.
    let home = tempfile::tempdir().unwrap();
    let (a, b) = (reservations(home.path(), []), reservations(home.path(), []));
    let (one, two) = (run("one"), run("two"));
    assert!(a.reserve(&one, 8080, Protocol::Tcp));
    assert!(!b.reserve(&two, 8080, Protocol::Tcp), "a holds it");
    assert!(b.reserved_elsewhere(&two, 8080, Protocol::Tcp));
    assert_eq!(b.ensure_free(&two, [(8080, Protocol::Tcp)]).unwrap_err().code(), crate::ErrorCode::PortInUse);
    assert!(!a.reserved_elsewhere(&one, 8080, Protocol::Tcp), "its own");
    assert!(a.reserve(&one, 8080, Protocol::Tcp), "its own reservation again");
    assert!(b.reserve(&two, 8080, Protocol::Udp), "another protocol");
    a.release(&one);
    assert!(!b.reserved_elsewhere(&two, 8080, Protocol::Tcp), "released");
    assert!(b.reserve(&two, 8080, Protocol::Tcp));
    assert!(!a.reserve(&one, 8080, Protocol::Tcp), "b holds it now");
}

#[test]
fn a_free_port_another_runner_reserved_is_skipped() {
    let home = tempfile::tempdir().unwrap();
    let (a, b) = (reservations(home.path(), []), reservations(home.path(), [41000, 41001]));
    assert!(a.reserve(&run("one"), 41000, Protocol::Tcp));
    let picked = b.pick_reserved(&run("two"), Protocol::Tcp).unwrap().unwrap();
    assert_eq!(picked.port, 41001, "41000 is reserved in the other runner");
}

#[test]
fn a_reservation_file_that_cant_be_locked_refuses_the_port() {
    // Fails closed: the reservations' folder isn't there.
    let home = tempfile::tempdir().unwrap();
    let r = reservations(&home.path().join("gone"), []);
    assert!(!r.reserve(&run("one"), 8080, Protocol::Tcp));
}

/// Not a test of its own: the child process of
/// [`a_reservation_whose_process_dies_is_freed`], run only when that test sets [`CHILD_HOME`].
#[test]
fn a_reserving_child() {
    let Some(home) = std::env::var_os(CHILD_HOME) else { return };
    let r = reservations(Path::new(&home), []);
    assert!(r.reserve(&run("child"), CHILD_PORT, Protocol::Tcp));
    println!("reserved");
    loop {
        std::thread::park();
    }
}

#[test]
fn a_reservation_whose_process_dies_is_freed() {
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "net::reservations::tests::a_reserving_child", "--nocapture", "--test-threads=1"])
        .env(CHILD_HOME, home.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let reserved = lines.by_ref().map_while(Result::ok).any(|line| line.contains("reserved"));
    let here = reservations(home.path(), []);
    let me = run("me");
    let refused = !here.reserve(&me, CHILD_PORT, Protocol::Tcp);
    let reserved_elsewhere = here.reserved_elsewhere(&me, CHILD_PORT, Protocol::Tcp);
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(reserved, "the child reserved the port");
    assert!(refused && reserved_elsewhere, "the child's reservation holds while it lives");
    assert!(!here.reserved_elsewhere(&me, CHILD_PORT, Protocol::Tcp), "freed by its death");
    assert!(here.reserve(&me, CHILD_PORT, Protocol::Tcp));
}
