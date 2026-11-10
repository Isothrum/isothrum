//! A run's life through the fake engine: up, status, park, resume, down, with their events.

use super::*;
use crate::{Op, Park, RunState, StepState};

#[tokio::test]
async fn a_run_comes_up_with_its_entry_and_message_then_goes() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    let started = run.up(&opts(), &events).await.unwrap();
    assert_eq!(f.containers(), Containers::Up);
    assert_eq!(started.outputs[0].value.as_deref(), Some("http://127.0.0.1:41873/"), "the pinned port");
    assert_eq!(started.message.as_deref(), Some("The portal answers at http://localhost:41873/.\n"));
    assert_eq!(started.record.state, RunState::Ready);
    assert_eq!(started.record.params.get("ISOLOOM_PUBLISH_ADDRESS").map(String::as_str), Some("127.0.0.1"));
    let k = kinds(&events);
    assert_eq!(
        k.first(),
        Some(&EventKind::State {
            from: RunState::Absent,
            to: RunState::Starting
        })
    );
    assert!(matches!(k.last(), Some(EventKind::Result { ok: true, .. })));
    assert!(
        k.iter()
            .any(|e| matches!(e, EventKind::Output { name, value: Some(v), .. } if name == "web__portal" && v.ends_with(":41873/")))
    );
    assert_eq!(f.runner.store.record(run.name()).unwrap().unwrap().state, RunState::Ready);

    let status = run.status().await.unwrap();
    assert_eq!(status.state, RunState::Ready);
    assert!(status.message.is_some() && status.networks[0].native.as_deref() == Some("web-portal_lab"));

    let down = Recorder::new();
    run.down(&RunOptions::default(), &down).await.unwrap();
    assert_eq!(f.containers(), Containers::None);
    assert_eq!(run.status().await.unwrap().state, RunState::Absent);
    assert!(matches!(kinds(&down).last(), Some(EventKind::Result { ok: true, .. })));
}

#[tokio::test]
async fn the_flag_reaches_only_the_steps_that_create_things_and_never_an_event() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    for call in f.fake.calls() {
        let has = call.env("DEMO_SECRET").is_some();
        let creates = call.line().contains(" up --detach ") || call.line().contains("--exit-code-from isoloom-init-web-1");
        assert_eq!(has, creates, "{}", call.line());
    }
    let text = serde_json::to_string(&events.events()).unwrap();
    assert!(!text.contains(SECRET) && text.contains("seeding ***"), "{text}");
    let log = std::fs::read_to_string(f.runner.store.run_dir(run.name()).unwrap().join("events.jsonl")).unwrap();
    assert!(!log.contains(SECRET));
    assert!(
        !std::fs::read_to_string(f.runner.store.run_dir(run.name()).unwrap().join("record.json"))
            .unwrap()
            .contains(SECRET)
    );
}

#[tokio::test]
async fn a_slot_without_a_value_gets_its_default_so_a_resume_needs_no_inputs() {
    // Isoloom delivers an input to `start` too, with `default: ""`: Compose reads an
    // environment-sourced secret on every start. A resume re-creates nothing, and its embedder
    // has no values to give.
    let mut m: serde_json::Value = serde_json::from_str(WEB_PORTAL).unwrap();
    m["secrets"][0]["deliver"][0]["steps"].as_array_mut().unwrap().push("start".into());
    let f = fixture_with(&m.to_string());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&RunOptions::default(), &Recorder::new()).await.unwrap();
    let up = f.fake.last(" up --detach ").unwrap();
    assert_eq!(up.env("DEMO_SECRET"), Some(""), "an optional input left out is set, empty");
    run.park(Park::Shutdown, &RunOptions::new(), &Recorder::new()).await.unwrap();
    let events = Recorder::new();
    run.resume(&RunOptions::default(), &events).await.unwrap();
    let start = f.fake.calls().into_iter().rev().find(|c| c.line().ends_with(" start")).unwrap();
    assert_eq!(start.env("DEMO_SECRET"), Some(""));
    assert!(matches!(kinds(&events).last(), Some(EventKind::Result { ok: true, .. })));
}

#[tokio::test]
async fn a_slot_without_a_value_or_a_default_is_not_delivered() {
    let mut m: serde_json::Value = serde_json::from_str(WEB_PORTAL).unwrap();
    m["secrets"][0].as_object_mut().unwrap().remove("default");
    let f = fixture_with(&m.to_string());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&RunOptions::default(), &Recorder::new()).await.unwrap();
    assert_eq!(f.fake.last(" up --detach ").unwrap().env("DEMO_SECRET"), None);
}

#[tokio::test]
async fn every_operation_ends_with_exactly_one_result_or_error_and_is_logged() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    let ends = |r: &Recorder| {
        r.events()
            .iter()
            .filter(|e| matches!(e.kind, EventKind::Result { .. } | EventKind::Error { .. }))
            .count()
    };
    let (a, b) = (Recorder::new(), Recorder::new());
    run.up(&opts(), &a).await.unwrap();
    assert!(run.up(&opts(), &b).await.is_err(), "already running");
    assert_eq!((ends(&a), ends(&b)), (1, 1));
    let seqs: Vec<u64> = a.events().iter().map(|e| e.seq).collect();
    assert_eq!(seqs, (1..=u64::try_from(seqs.len()).unwrap()).collect::<Vec<_>>());
    let logged = f.runner.events(run.name()).unwrap().events;
    assert_eq!(logged.len(), a.events().len() + b.events().len());
    assert!(logged.iter().all(|e| e.time == "2026-10-15T12:00:00Z" && e.v == 0));
}

#[tokio::test]
async fn a_run_already_up_is_refused_and_stays_ready() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let ups = f.called(" up --detach ");
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::AlreadyRunning);
    assert_eq!(f.called(" up --detach "), ups, "nothing started again");
    assert_eq!(f.runner.store.record(run.name()).unwrap().unwrap().state, RunState::Ready);
}

#[tokio::test]
async fn a_parked_run_resumes_without_being_re_created() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let parked = Recorder::new();
    run.park(Park::Shutdown, &RunOptions::new(), &parked).await.unwrap();
    assert_eq!(f.containers(), Containers::Stopped);
    assert_eq!(run.status().await.unwrap().state, RunState::Parked);
    assert!(kinds(&parked).contains(&EventKind::State {
        from: RunState::Parking,
        to: RunState::Parked
    }));
    let ups = f.called(" up --detach ");
    let started = run.resume(&RunOptions::default(), &Recorder::new()).await.unwrap();
    assert_eq!((f.containers(), f.called(" up --detach ")), (Containers::Up, ups), "`start`, never `up`");
    assert_eq!(started.outputs[0].value.as_deref(), Some("http://127.0.0.1:41873/"), "the same address");
    assert_eq!(f.runner.store.record(run.name()).unwrap().unwrap().state, RunState::Ready);
}

#[tokio::test]
async fn up_on_a_parked_run_resumes_it() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    run.park(Park::Pause, &RunOptions::new(), &Recorder::new()).await.unwrap();
    let ups = f.called(" up --detach ");
    run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!((f.containers(), f.called(" up --detach ")), (Containers::Up, ups));
}

#[tokio::test]
async fn nothing_left_to_resume_drops_the_parked_mark() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    run.park(Park::Shutdown, &RunOptions::new(), &Recorder::new()).await.unwrap();
    f.set(Containers::None);
    let err = run.resume(&RunOptions::default(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::NothingToResume);
    assert_eq!(f.runner.store.record(run.name()).unwrap().unwrap().state, RunState::Absent);
}

/// `ps` output listing no container the runner can read fails the resume: read as no container,
/// it would drop the parked mark of a run whose containers are there.
#[tokio::test]
async fn unreadable_ps_output_fails_a_resume_and_keeps_the_run_parked() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    run.park(Park::Shutdown, &RunOptions::new(), &Recorder::new()).await.unwrap();
    f.engine.lock().unwrap().unreadable_ps = true;
    let err = run.resume(&RunOptions::default(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::ToolFailed, "{err}");
    assert!(err.to_string().contains("printed no container this runner can read"), "{err}");
    assert_eq!(f.runner.store.record(run.name()).unwrap().unwrap().state, RunState::Parked);
    assert_eq!(f.containers(), Containers::Stopped, "nothing started");
}

#[tokio::test]
async fn leftovers_are_removed_before_a_start() {
    let f = fixture();
    f.set(Containers::Stopped);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    let lines = f.fake.lines();
    let down = lines.iter().position(|l| l.contains(" down --volumes")).unwrap();
    let up = lines.iter().position(|l| l.contains(" up --detach ")).unwrap();
    assert!(down < up);
    assert!(
        kinds(&events)
            .iter()
            .any(|k| matches!(k, EventKind::Warning { code, .. } if *code == crate::WarningCode::LeftoversRemoved))
    );
}

#[tokio::test]
async fn a_park_of_a_run_that_isnt_there_is_refused() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.park(Park::Shutdown, &RunOptions::new(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::NotRunning);
}

#[tokio::test]
async fn a_failed_start_leaves_the_run_failed_with_the_steps_error() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    let fail = Arc::new(FakeProcesses::rules(&[
        (" config --format json", Reply::ok(r#"{"services":{}}"#)),
        (" up --detach ", Reply::fail(1, "Error: pull access denied")),
    ]));
    let mut config = f.runner.config.clone();
    config.processes = fail;
    let runner = Runner::new(config).unwrap();
    let lab2 = runner.open(run.path(), None).unwrap();
    let err = lab2.up(&opts(), &events).await.unwrap_err();
    assert_eq!(err.to_string(), "step `up` failed: Error: pull access denied (exited with code 1)");
    assert!(kinds(&events).contains(&EventKind::Step {
        step: "job-isoloom-init-web-1".into(),
        state: StepState::Skipped,
        note: None
    }));
    assert!(matches!(kinds(&events).last(), Some(EventKind::Error { step: Some(s), .. }) if s == "up"));
    assert_eq!(runner.store.record(lab2.name()).unwrap().unwrap().state, RunState::Failed);
    assert_eq!(runner.store.record(lab2.name()).unwrap().unwrap().last.unwrap().op, Op::Up);
}

#[tokio::test]
async fn a_stopped_engine_fails_the_start_before_anything_is_created() {
    let f = fixture();
    let mut config = f.runner.config.clone();
    let fake = Arc::new(FakeProcesses::rules(&[("docker info", Reply::fail(1, "Cannot connect to the Docker daemon"))]));
    config.processes = crate::testing::processes(&fake);
    let runner = Runner::new(config).unwrap();
    let run = runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "step `tools` failed: `docker` doesn't answer: Cannot connect to the Docker daemon (exited with code 1)"
    );
    assert_eq!(fake.lines(), ["docker info --format {{.ServerVersion}}"], "nothing but the probe ran");
}

#[tokio::test]
async fn a_lab_without_provision_steps_isnt_provisioned_again() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let err = run.provision(None, &opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::Unsupported);
    assert!(err.to_string().contains("no provision steps"), "{err}");
}

/// The fixture regenerated with a service added (`extra`, started with the others) and one taken
/// out (`isoloom-routes-db`).
fn regenerated() -> String {
    let mut m: serde_json::Value = serde_json::from_str(WEB_PORTAL).unwrap();
    let machines = m["machines"].as_array_mut().unwrap();
    let mut extra = machines[0].clone();
    machines.retain(|x| x["name"] != "isoloom-routes-db");
    extra["name"] = "extra".into();
    extra["native"]["service"] = "extra".into();
    machines.push(extra);
    let up = m["lifecycle"]["up"].as_array_mut().unwrap().iter_mut().find(|s| s["id"] == "up").unwrap();
    let argv = up["argv"].as_array_mut().unwrap();
    argv.retain(|a| a != "isoloom-routes-db");
    argv.push("extra".into());
    m.to_string()
}

/// An update of a running Compose run runs its start again, so a service added comes up with the
/// others; a service taken out goes at its retire, before the update, which then removes nothing
/// of its own accord beyond what its start step says.
#[tokio::test]
async fn an_update_starts_an_added_service_and_a_service_taken_out_goes_at_its_retire() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    run.retire("isoloom-routes-db", &RunOptions::new(), &Recorder::new()).await.unwrap();
    let rm = f.fake.last(" rm -sf").expect("the service taken out is removed");
    assert!(rm.line().ends_with(" rm -sf isoloom-routes-db"), "{}", rm.line());

    std::fs::write(&f.manifest, regenerated()).unwrap();
    let before = f.fake.calls().len();
    let events = Recorder::new();
    let updated = f.runner.open(&f.manifest, None).unwrap().update(&opts(), &events).await.unwrap();
    assert_eq!(updated.record.state, RunState::Ready);
    assert!(events.events().iter().all(|e| e.op == Some(Op::Update)));
    let start = f.fake.last(" up --detach ").unwrap();
    assert!(
        start.line().ends_with(" db web isoloom-routes-web isoloom-probe-amd64 extra"),
        "{}",
        start.line()
    );
    let during: Vec<String> = f.fake.calls()[before..].iter().map(Call::line).collect();
    assert!(during.iter().all(|l| !l.contains(" rm ")), "{during:?}");
}

/// An update records the manifest it ran: the file a later operation that has only the record
/// (a sweep's `down`) reopens is the regenerated one, not the one the run started from.
#[tokio::test]
async fn an_update_records_the_manifest_it_ran() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(f.runner.store.record(run.name()).unwrap().unwrap().manifest, f.manifest);

    let regenerated_at = f.manifest.with_file_name("run-regenerated.json");
    std::fs::write(&regenerated_at, regenerated()).unwrap();
    let again = f.runner.open(&regenerated_at, Some(run.name().clone())).unwrap();
    let updated = again.update(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(updated.record.manifest, regenerated_at);
    assert_eq!(f.runner.store.record(run.name()).unwrap().unwrap().manifest, regenerated_at);
}

/// Sets the fake engine's project networks and whether its config can be read.
fn engine_with(f: &Fixture, networks: &[&str], unreadable_config: bool) {
    let mut e = f.engine.lock().unwrap_or_else(PoisonError::into_inner);
    e.networks = networks.iter().map(|&n| n.to_owned()).collect();
    e.unreadable_config = unreadable_config;
}

/// A config Compose prints that can't be read fails the free-ports check: its ports would
/// otherwise pass unchecked. Nothing starts.
#[tokio::test]
async fn an_unreadable_config_fails_the_free_ports_check_before_anything_starts() {
    let f = fixture_answering(WEB_PORTAL, unreadable_once_pinned);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert!(matches!(&err, crate::Error::ToolFailed { step, .. } if step == "ports-free"), "{err}");
    assert_eq!(f.called(" up --detach "), 0, "nothing started");
}

/// The run's Docker, its config unreadable once the run's ports are pinned.
fn unreadable_once_pinned(engine: &Mutex<Engine>, call: &Call) -> Reply {
    if call.line().contains(" config --format json") && pinned(call).is_some() {
        return Reply::ok("not json");
    }
    answer(engine, call)
}

/// A fresh run whose config can't be read fails the pinning step rather than start on ephemeral
/// ports a resume would not get back. Nothing starts.
#[tokio::test]
async fn an_unreadable_config_fails_the_pinning_of_a_fresh_run() {
    let f = fixture();
    engine_with(&f, &[], true);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert!(matches!(&err, crate::Error::ToolFailed { step, .. } if step == "pin-ports"), "{err}");
    assert_eq!(f.called(" up --detach "), 0, "nothing started");
}

/// A fresh run with no free host port left for an ephemeral publish fails the pinning step,
/// naming the port, rather than start on an ephemeral one.
#[tokio::test]
async fn no_free_host_port_fails_the_pinning_of_a_fresh_run() {
    let f = fixture_with_ports(WEB_PORTAL, []);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert!(
        matches!(&err, crate::Error::ToolFailed { step, reason } if step == "pin-ports" && reason.contains("no free host port over tcp")),
        "{err}"
    );
    assert_eq!(f.called(" up --detach "), 0, "nothing started");
}

/// The networks the config no longer defines go; one it defines stays.
#[tokio::test]
async fn a_stale_network_goes_and_a_defined_one_stays() {
    let f = fixture();
    engine_with(&f, &["web-portal_lab", "web-portal_old"], false);
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(f.called("docker network rm web-portal_old"), 1);
    assert_eq!(f.called("docker network rm web-portal_lab"), 0);
}

/// An update whose config can't be read removes no network: read as empty, every live network of
/// the project would be stale. The step fails (tolerated by the manifest, so a warning).
#[tokio::test]
async fn an_update_with_an_unreadable_config_removes_no_network() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    engine_with(&f, &["web-portal_lab", "web-portal_old"], true);
    let events = Recorder::new();
    run.update(&opts(), &events).await.unwrap();
    assert_eq!(f.called("docker network rm"), 0, "no network removed");
    assert!(kinds(&events).iter().any(|k| matches!(
        k,
        EventKind::Warning { code: crate::WarningCode::StepTolerated, message } if message.contains("networks it defines")
    )));
}

/// A manifest whose hosts leave this one out is refused at open, its target named as the manifest
/// writes it.
#[test]
fn a_target_not_run_on_this_host_is_refused_by_its_name() {
    use isoloom_manifest::HostOs;
    let here = crate::runner::this_host();
    let others: Vec<String> = [HostOs::Linux, HostOs::Macos, HostOs::Windows]
        .iter()
        .filter(|h| **h != here)
        .map(crate::encoding::wire_name)
        .collect();
    let mut m: serde_json::Value = serde_json::from_str(WEB_PORTAL).unwrap();
    m["hosts"] = serde_json::json!(others);
    let f = fixture_with(&m.to_string());
    let err = f.runner.open(&f.manifest, None).unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::Unsupported);
    assert_eq!(err.to_string(), "target `docker` on this host isn't supported");
}

#[tokio::test]
async fn a_verdict_that_isnt_a_regular_expression_is_a_malformed_manifest() {
    let mut m: serde_json::Value = serde_json::from_str(WEB_PORTAL).unwrap();
    m["lifecycle"]["up"].as_array_mut().unwrap().iter_mut().find(|s| s["id"] == "up").unwrap()["verdict"] = "(".into();
    let f = fixture_with(&m.to_string());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::ManifestInvalid, "{err}");
    assert!(err.to_string().starts_with("lifecycle.up.up.verdict: `(` isn't a regular expression"), "{err}");
    assert!(f.fake.last(" up --detach ").is_none(), "the step never ran");
}

/// Regression: only the Terraform driver forgot the run's pinned host keys, so a Compose or
/// Vagrant run brought up again refused its recreated machines' new keys at the same addresses.
#[tokio::test]
async fn a_down_forgets_the_runs_ssh_files_whatever_the_driver() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let state = f.runner.store.run_dir(run.name()).unwrap();
    for file in ["known_hosts", "ssh_config"] {
        std::fs::write(state.join(file), "10.0.0.9 ssh-ed25519 AAAAold\n").unwrap();
    }
    run.down(&RunOptions::default(), &Recorder::new()).await.unwrap();
    for file in ["known_hosts", "ssh_config"] {
        assert!(!state.join(file).exists(), "{file} is forgotten");
    }
}
