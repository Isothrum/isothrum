//! A resume and the services it unpauses: their health checks run again before its start.

use super::*;

/// A fake Compose engine for the park and resume tests: `ps` answers `before` until an `unpause`,
/// then each of `after` in turn (its last one from then on); the project's services are
/// `services` (`network_mode` from `config` as `config_json` says); anything else succeeds.
fn resuming_engine(before: String, after: Vec<String>, services: &'static str, config_json: &'static str) -> FakeProcesses {
    use std::sync::Mutex;
    let state = Mutex::new((false, 0_usize));
    FakeProcesses::model(move |call| {
        let line = call.line();
        let words: Vec<&str> = line.split_whitespace().collect();
        let mut st = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match words.get(6).copied() {
            Some("unpause") => {
                st.0 = true;
                Reply::ok("")
            }
            Some("ps") if !st.0 => Reply::ok(&before),
            Some("ps") => {
                let i = st.1.min(after.len() - 1);
                st.1 += 1;
                Reply::ok(&after[i])
            }
            Some("config") if words.contains(&"--services") => Reply::ok(services),
            Some("config") => Reply::ok(config_json),
            _ => Reply::ok(""),
        }
    })
}

/// The Compose verbs `fake` was asked, in order (`ps` and `config` reads included).
fn verbs(fake: &FakeProcesses) -> Vec<String> {
    fake.lines().iter().filter_map(|l| l.split_whitespace().nth(6).map(str::to_owned)).collect()
}

/// Regression: Docker reads a container it paused unhealthy, and still so once unpaused until its
/// health check next runs, so the resume's `up --wait` failed at once on a dependency that was fine
/// (`dependency failed to start: container … is unhealthy`). The resume now waits for the unpaused
/// services to read healthy again before its `up`.
#[tokio::test(start_paused = true)]
async fn a_resume_waits_for_the_unpaused_services_health_check_before_its_start() {
    let (_dir, project) = folder();
    let run = run();
    let listing = |state: &str, health: &str| [ps_line("web", state, health, 0, None), ps_line("routes-web", state, health, 0, None)].join("\n");
    let fake = resuming_engine(
        listing("paused", ""),
        vec![listing("running", "unhealthy"), listing("running", "unhealthy"), listing("running", "healthy")],
        "web\nroutes-web\n",
        r#"{"services": {"web": {}, "routes-web": {"network_mode": "service:web"}}}"#,
    );
    Compose.resume(&cx(&project, &run, &fake)).await.expect("resumed once the checks ran again");
    let v = verbs(&fake);
    let unpause = v.iter().position(|x| x == "unpause").expect("unpaused");
    let up = v.iter().position(|x| x == "up").expect("started");
    let reads = v[unpause..up].iter().filter(|x| *x == "ps").count();
    assert_eq!(reads, 3, "read until healthy, then started: {v:?}");
}

/// A run whose unpaused service stays unhealthy is waited for a minute at most, then its `up`
/// judges it.
#[tokio::test(start_paused = true)]
async fn a_resume_waits_a_minute_at_most_for_an_unhealthy_service() {
    let (_dir, project) = folder();
    let run = run();
    let fake = resuming_engine(
        ps_line("web", "paused", "", 0, None),
        vec![ps_line("web", "running", "unhealthy", 0, None)],
        "web\n",
        r#"{"services": {"web": {}}}"#,
    );
    Compose.resume(&cx(&project, &run, &fake)).await.expect("its up judges it");
    assert!(
        fake.lines().last().is_some_and(|l| l.ends_with(" up --detach --wait --no-recreate")),
        "{:?}",
        fake.lines()
    );
}
