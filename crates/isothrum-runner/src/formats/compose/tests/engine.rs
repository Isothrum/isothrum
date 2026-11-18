//! A step the engine fails under: stopped or quit while it ran (`tool_unanswered`, the step
//! named), refusing this account (`tool_denied`), or a network's addresses another holds
//! (`address_pool_overlap`): each by its code, never read out of a failed step's words.

use super::*;
use crate::ErrorCode;

#[tokio::test]
async fn an_up_the_engine_fails_under_is_said_by_the_engines_code() {
    for (said, code) in [
        (
            include_str!("../../../tool_output/fixtures/compose/27.1-engine-down.txt"),
            ErrorCode::ToolUnanswered,
        ),
        (
            include_str!("../../../tool_output/fixtures/compose/27.1-windows-engine-gone.txt"),
            ErrorCode::ToolUnanswered,
        ),
        (
            include_str!("../../../tool_output/fixtures/compose/29.9-engine-stopped-during-up.txt"),
            ErrorCode::ToolUnanswered,
        ),
        (
            include_str!("../../../tool_output/fixtures/compose/28.0-socket-denied.txt"),
            ErrorCode::ToolDenied,
        ),
        (
            include_str!("../../../tool_output/fixtures/compose/2.29-pool-overlaps.txt"),
            ErrorCode::AddressPoolOverlap,
        ),
    ] {
        let (_dir, project) = folder();
        let run = run();
        let fake = FakeProcesses::script()
            .expect(UP, Reply::fail(1, said))
            .allow(PULL, Reply::ok(""))
            .expect(CONFIG, Reply::ok(NO_PORTS))
            .build();
        let e = Compose.up(&cx(&project, &run, &fake)).await.expect_err("failed");
        let report = e.report();
        assert_eq!((report.code, report.step.as_deref()), (code, Some("up")), "{said}");
        assert!(
            match report.detail.map(|d| d.data) {
                Some(crate::Detail::ToolUnanswered { tool } | crate::Detail::ToolDenied { tool }) => tool == "docker",
                Some(crate::Detail::AddressPoolOverlap { reason }) => reason.contains("Pool overlaps"),
                _ => false,
            },
            "{said}"
        );
    }
}
