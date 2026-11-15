//! What an operation's log holds of its tools: what a tool that acts said, never what a read
//! printed (a version, `ps`'s JSON, a status).

use super::*;

/// Each line `events` hold of the tools.
fn logged(events: &Recorder) -> Vec<String> {
    events
        .events()
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::Log { line, .. } => Some(line),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_ups_log_holds_none_of_what_its_reads_printed() {
    let (_root, project, runner, _engine, fake) = setup_seen();
    let run = runner.open(&project, None).expect("opened");
    let events = Recorder::new();
    run.up(&RunOptions::new(), &events).await.expect("up");
    let reads: Vec<String> = fake
        .calls()
        .iter()
        .filter(|c| c.args.iter().any(|a| a == "ps" || a == "version" || a == "config"))
        .map(Call::line)
        .collect();
    assert!(!reads.is_empty(), "the up read its project");
    let lines = logged(&events);
    assert!(
        !lines.iter().any(|l| l.contains("\"Service\"") || l == "2.29.7" || l.contains("\"services\"")),
        "{lines:?}"
    );
}
