//! What the runner last saw of a run (`seen.json`): what a status shows while an operation holds
//! the run, and what a down forgets.

use super::*;

/// A status while another process holds the run, as that process's lock says.
async fn held_status(f: &Fixture, run: &crate::Running<'_>) -> (crate::Result<crate::Status>, String) {
    let other = std::fs::File::create(f.runner.store.lock_path(run.name())).unwrap();
    other.lock().unwrap();
    crate::testing::traced(run.status()).await
}

/// A status held by an operation shows the run as last read: its machines, and where their
/// published ports answer (`${published…}`, `${host…}`), kept in `seen.json`.
#[tokio::test]
async fn a_held_status_shows_the_machines_and_ports_as_last_read() {
    let mut m: serde_json::Value = serde_json::from_str(WEB_PORTAL).unwrap();
    m["outputs"][0]["format"] = "http://${host.web}:${value}/".into();
    let f = fixture_with(&m.to_string());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts().with_param("ISOLOOM_PUBLISH_ADDRESS", "192.0.2.7"), &Recorder::new())
        .await
        .unwrap();
    let live = run.status().await.unwrap();
    assert_eq!(live.outputs[0].value.as_deref(), Some("http://192.0.2.7:41873/"));
    let (held, _) = held_status(&f, &run).await;
    let held = held.unwrap();
    assert!(!held.live);
    assert_eq!(held.machines, live.machines);
    assert_eq!(held.outputs, live.outputs);
}

/// Regression: a down forgot every read cache of the run but `seen.json`, so a status while the
/// next start held the run showed the gone run's machines, running.
#[tokio::test]
async fn a_held_status_after_a_down_shows_no_machine_of_the_gone_run() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    run.status().await.unwrap();
    run.down(&RunOptions::default(), &Recorder::new()).await.unwrap();
    let (held, _) = held_status(&f, &run).await;
    let held = held.unwrap();
    assert!(!held.live);
    assert!(held.machines.is_empty(), "{:?}", held.machines);
}

/// A `seen.json` that can't be read shows no machine, and says why in the traces.
#[tokio::test]
async fn a_seen_file_that_cant_be_read_shows_no_machine_and_is_traced() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let seen = f.runner.store.run_dir(run.name()).unwrap().join("seen.json");
    std::fs::write(&seen, "not json").unwrap();
    let (held, traced) = held_status(&f, &run).await;
    assert_eq!(held.unwrap().machines, []);
    assert!(traced.contains("WARN") && traced.contains("last saw of the run can't be read"), "{traced}");
}
