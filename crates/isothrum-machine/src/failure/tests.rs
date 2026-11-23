use std::time::Duration;

use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::testing::{Stuck, answered_by};

const LIMIT: Duration = Duration::from_secs(60);

/// Every refusal read as `Denied`.
fn denied(_: &str) -> Refusal {
    Refusal::Denied
}

#[tokio::test(start_paused = true)]
async fn an_answer_is_the_output_and_a_refusal_its_last_line_read_as_the_tool_says() {
    let cx = answered_by(
        FakeProcesses::script()
            .expect("tool ok", Reply::ok("one\ntwo\n"))
            .expect("tool no", Reply::fail(1, "WARNING: x\nERROR: not allowed\n\n"))
            .build(),
    );
    assert_eq!(answer(&cx, LIMIT, &ProcessSpec::new("tool", ["ok"]), denied).await.unwrap(), "one\ntwo");
    let refused = answer(&cx, LIMIT, &ProcessSpec::new("tool", ["no"]), denied).await.unwrap_err();
    assert_eq!(
        refused,
        Failure::Refused {
            program: "tool".into(),
            refusal: Refusal::Denied,
            said: "ERROR: not allowed".into()
        }
    );
    assert_eq!(refused.to_string(), "ERROR: not allowed");
    assert_eq!(refused.code(), ErrorCode::ToolDenied);
}

#[tokio::test(start_paused = true)]
async fn a_silent_refusal_is_said_by_its_output_else_its_exit() {
    let mut on_stdout = Reply::ok("refused: log in first");
    on_stdout.code = 2;
    let cx = answered_by(FakeProcesses::script().expect("tool a", on_stdout).expect("tool b", Reply::fail(3, "")).build());
    let said = |f: Failure| match f {
        Failure::Refused { said, .. } => said,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        said(answer(&cx, LIMIT, &ProcessSpec::new("tool", ["a"]), denied).await.unwrap_err()),
        "refused: log in first"
    );
    assert_eq!(
        said(answer(&cx, LIMIT, &ProcessSpec::new("tool", ["b"]), denied).await.unwrap_err()),
        "exited with code 3"
    );
}

#[tokio::test(start_paused = true)]
async fn a_missing_or_stuck_tool_is_said_as_such_never_as_a_refusal() {
    let cx = answered_by(FakeProcesses::script().expect("tool", Reply::missing()).build());
    let missing = answer(&cx, LIMIT, &ProcessSpec::new("tool", [""; 0]), denied).await.unwrap_err();
    assert_eq!(missing, Failure::Missing { program: "tool".into() });
    assert_eq!((missing.code(), missing.refusal()), (ErrorCode::ToolMissing, None));
    let stuck = answer(&answered_by(Stuck), LIMIT, &ProcessSpec::new("tool", [""; 0]), denied)
        .await
        .unwrap_err();
    assert_eq!(stuck.to_string(), "`tool` gave no answer within 60 s");
    assert_eq!(stuck.code(), ErrorCode::Timeout);
}

#[test]
fn an_expired_sign_in_has_the_runners_code() {
    let f = |refusal| Failure::Refused {
        program: "cli".into(),
        refusal,
        said: String::new(),
    };
    assert_eq!(f(Refusal::CredentialsExpired).code(), ErrorCode::CredentialsExpired);
    assert_eq!(f(Refusal::NotFound).code(), ErrorCode::ToolFailed);
    assert_eq!(f(Refusal::QuotaExceeded).program(), "cli");
}

#[tokio::test(start_paused = true)]
async fn asking_a_tool_reads_its_answer_within_the_probe_time() {
    let cx = answered_by(FakeProcesses::script().expect("terraform version", Reply::ok("Terraform v1.9.8")).build());
    assert_eq!(ask(&cx, ("terraform", &["version"])).await.unwrap(), "Terraform v1.9.8");
    let broken = answered_by(
        FakeProcesses::script()
            .expect("terraform version", Reply::fail(1, "broken plugin cache"))
            .build(),
    );
    assert_eq!(ask(&broken, ("terraform", &["version"])).await.unwrap_err().refusal(), Some(Refusal::Other));
}
