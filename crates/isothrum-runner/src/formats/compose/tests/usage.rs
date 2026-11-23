//! What a run's services use now, as the engine samples them.

use super::*;

/// What a run's services use: `ps` names each running container's service, the engine's `stats`
/// (every running container of the engine, another project's too) its figures, a service's
/// containers summed; a stopped container has none, and nothing running reads nothing.
#[tokio::test]
async fn usage_is_each_running_services_containers_as_the_engine_samples_them() {
    const PS: &str = include_str!("../../../tool_output/fixtures/compose/5.6-ps.jsonl");
    const STATS: &str = include_str!("../../../tool_output/fixtures/compose/29.9-stats.jsonl");
    const LIST: &str = "docker compose -p stack -f * ps --all --format json";
    // Its one argument, `{{json .}}`, has a space the script's words don't.
    const SAMPLE: &str = "docker stats --no-stream --format *";
    let (_dir, project) = folder();
    let run = run();
    let stopped = r#"{"ID":"0123456789ab","Service":"idle","State":"exited","ExitCode":0,"Labels":""}"#;
    let fake = FakeProcesses::script()
        .expect(LIST, Reply::ok(&format!("{PS}{stopped}\n")))
        .expect(SAMPLE, Reply::ok(STATS))
        .build();
    let used = Compose.usage(&cx(&project, &run, &fake)).await.expect("read");
    assert_eq!(
        used,
        [
            crate::MachineUsage::new("idle", Some(0.0), Some(1_499_463)),
            crate::MachineUsage::new("web", Some(1.78), Some(10_768_875)),
        ]
    );
    let none_running = FakeProcesses::script().expect(LIST, Reply::ok(stopped)).build();
    assert_eq!(Compose.usage(&cx(&project, &run, &none_running)).await.expect("read"), []);
}
