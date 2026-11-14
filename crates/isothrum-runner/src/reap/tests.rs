use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use super::*;
use crate::name::RunName;
use crate::process::fake::FakeProcesses;
use crate::project::Format;
use crate::project::platform::HostOs;
use crate::testing::{TEST_TIME, no_ports, processes, runner_config};

/// A runner with no tool, its home a temporary folder.
fn runner() -> (tempfile::TempDir, Runner) {
    let home = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeProcesses::script().build());
    let r = Runner::new(runner_config(home.path(), processes(&fake), no_ports(), HostOs::Linux)).unwrap();
    (home, r)
}

/// A recorded run in `state`, expiring at `expires_at`.
fn record(r: &Runner, name: &str, state: RunState, expires_at: Option<u64>) {
    let mut rec = RunRecord::fresh(RunName::new(name).unwrap(), "/l".into(), Format::Terraform);
    rec.state = state;
    rec.expires_at = expires_at;
    r.store.save(&rec).unwrap();
}

#[test]
fn a_run_is_expired_once_its_time_passed_while_something_of_it_is_left() {
    let (_home, r) = runner();
    record(&r, "past", RunState::Ready, Some(TEST_TIME - 1));
    record(&r, "failed-past", RunState::Failed, Some(TEST_TIME));
    record(&r, "future", RunState::Ready, Some(TEST_TIME + 60));
    record(&r, "gone", RunState::Absent, Some(TEST_TIME - 600));
    record(&r, "no-expiry", RunState::Ready, None);
    let now = UNIX_EPOCH + Duration::from_secs(TEST_TIME);
    let names: Vec<String> = r.expired(now).unwrap().iter().map(|r| r.name.to_string()).collect();
    assert_eq!(names, ["failed-past", "past"]);
}
