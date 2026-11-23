//! A step the engine fails under: stopped or quit while it ran (`tool_unanswered`, the step
//! named), refusing this account (`tool_denied`), or a network's addresses another holds
//! (`address_pool_overlap`): each by its code, never read out of a failed step's words. Where the
//! words leave the engine's state open, the engine is asked: one that doesn't answer is down,
//! whatever Docker printed (Docker Desktop's socket answering 502 while it quits).

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

/// Docker Desktop quitting during an `up`: its socket answers the engine's API with `502 Bad
/// Gateway`, which reads as a cut pull. The engine, asked, doesn't answer: the step is
/// `tool_unanswered`. Where it answers, the failure stays what Docker said.
#[tokio::test]
async fn an_up_failing_in_open_words_asks_the_engine_and_a_silent_one_is_down() {
    let said = include_str!("../../../tool_output/fixtures/compose/29.9-engine-quitting-502-during-up.txt");
    for (engine, code) in [
        (
            Reply::fail(1, "Client: Docker Engine - Community\nfailed to connect to the docker API"),
            ErrorCode::ToolUnanswered,
        ),
        (Reply::ok("29.9.3"), ErrorCode::ToolFailed),
    ] {
        let (_dir, project) = folder();
        let run = run();
        let fake = FakeProcesses::script()
            .expect(UP, Reply::fail(1, said))
            .allow(PULL, Reply::ok(""))
            .expect(CONFIG, Reply::ok(NO_PORTS))
            .expect(ENGINE_ASKED, engine)
            .build();
        let e = Compose.up(&cx(&project, &run, &fake)).await.expect_err("failed");
        let report = e.report();
        assert_eq!((report.code, report.step.as_deref()), (code, Some("up")), "{report:?}");
    }
}

/// A failure that names what the engine refused came from an engine that answered: it isn't asked.
#[tokio::test]
async fn a_failure_naming_what_the_engine_refused_doesnt_ask_it() {
    let (_dir, project) = folder();
    let run = run();
    let fake = FakeProcesses::script()
        .expect(UP, Reply::fail(1, include_str!("../../../tool_output/fixtures/compose/2.29-pool-overlaps.txt")))
        .allow(PULL, Reply::ok(""))
        .expect(CONFIG, Reply::ok(NO_PORTS))
        .build();
    let e = Compose.up(&cx(&project, &run, &fake)).await.expect_err("failed");
    assert_eq!(e.report().code, ErrorCode::AddressPoolOverlap);
    assert!(fake.made(ENGINE_ASKED).is_empty(), "the engine wasn't asked");
}
