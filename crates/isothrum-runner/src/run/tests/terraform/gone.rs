//! A destroy that finds resources the server no longer has: the fake Terraform's state holds the
//! IP set a Proxmox host's reset lost, and its destroy fails as the real one did.

use super::*;
use crate::testing::IPSET_LEFT;

/// What the real destroy printed for that IP set, its lines as they came.
const IPSET_GONE: &str = "\nError: error getting IPSet content: received an HTTP 500 response - Reason: no such IPSet 'iso1n0'\n";

/// The fake Terraform's destroy and `state rm` over a state holding [`IPSET_LEFT`]: while the
/// state holds the IP set, the destroy fails printing `error`; `state rm` drops what it names
/// unless `rm_drops` is false (a state rm that leaves it, to see the retries end).
fn destroy_over_left_ipset(error: &'static str, rm_drops: bool) -> impl Fn(&Call) -> Option<Reply> + Send + Sync + 'static {
    move |call| {
        let line = call.line();
        let state = state_dir(call)?.join("terraform.tfstate");
        if line.contains(" destroy ") && std::fs::read_to_string(&state).is_ok_and(|s| s.contains("iso1n0")) {
            return Some(Reply::fail(1, error));
        }
        if line.contains(" state rm ") {
            let mut tf: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
            let named = |r: &serde_json::Value| call.args.contains(&format!("{}.{}", r["type"].as_str().unwrap(), r["name"].as_str().unwrap()));
            if rm_drops {
                tf["resources"].as_array_mut().unwrap().retain(|r| !named(r));
            }
            std::fs::write(&state, tf.to_string()).unwrap();
            return Some(Reply::ok(
                "Removed proxmox_virtual_environment_firewall_ipset.test\nSuccessfully removed 1 resource instance(s).",
            ));
        }
        None
    }
}

/// A run whose state holds only [`IPSET_LEFT`], the fake destroy and `state rm` as
/// [`destroy_over_left_ipset`] says.
async fn with_ipset_left(error: &'static str, rm_drops: bool) -> Server {
    let s = scripted(HostOs::Linux, SOLO_WEB_PROXMOX, destroy_over_left_ipset(error, rm_drops));
    let run = s.runner.open(&s.manifest, None).unwrap();
    run.up(&opts("pve-run"), &Recorder::new()).await.unwrap();
    std::fs::write(s.runner.store.run_dir(run.name()).unwrap().join("terraform.tfstate"), IPSET_LEFT).unwrap();
    s
}

/// The warnings of `events` with `code`, their messages.
fn warnings(events: &Recorder, code: crate::WarningCode) -> Vec<String> {
    events
        .events()
        .into_iter()
        .filter_map(|e| match e.kind {
            crate::EventKind::Warning { code: c, message } if c == code => Some(message),
            _ => None,
        })
        .collect()
}

/// Regression: a Proxmox host reset after a cut-short apply had lost the network's IP set, and
/// every destroy of the run failed on it (the provider's delete reads the set first, and the
/// destroy runs without the refresh whose read would have dropped it), so the run could never go.
#[tokio::test(start_paused = true)]
async fn a_destroy_that_failed_on_a_resource_already_gone_drops_it_and_destroys_again() {
    let s = with_ipset_left(IPSET_GONE, true).await;
    let run = s.runner.open(&s.manifest, None).unwrap();
    let events = Recorder::new();
    run.down(&opts("pve-run"), &events).await.unwrap();
    let rm: Vec<Call> = s.fake.calls().into_iter().filter(|c| c.line().contains(" state rm ")).collect();
    assert_eq!(rm.len(), 1, "{:?}", s.fake.lines());
    assert_eq!(rm[0].args, ["state", "rm", "proxmox_virtual_environment_firewall_ipset.test"]);
    let destroy = s.fake.last("terraform destroy").unwrap();
    assert_eq!(rm[0].env("TF_DATA_DIR"), destroy.env("TF_DATA_DIR"), "the destroy's own state");
    assert_eq!(rm[0].cwd, destroy.cwd);
    assert_eq!(
        s.fake.calls().iter().filter(|c| c.line().contains(" destroy ")).count(),
        2,
        "then destroyed again"
    );
    assert_eq!(
        warnings(&events, crate::WarningCode::AlreadyGone),
        ["`proxmox_virtual_environment_firewall_ipset.test` is already gone (no such IPSet 'iso1n0'): dropped from the state, then destroyed again"]
    );
    assert_eq!(run.status().await.unwrap().state, RunState::Absent);
}

#[tokio::test(start_paused = true)]
async fn a_destroy_that_failed_otherwise_drops_nothing() {
    // Another IP set's, the error without the set's name, and another failure: none names the
    // state's IP set, which may still exist.
    for error in [
        "Error: error getting IPSet content: received an HTTP 500 response - Reason: no such IPSet 'iso1n00'",
        "Error: error getting IPSet content: received an HTTP 500 response - Reason: no such IPSet",
        "Error: error getting IPSet content: Get \"https://10.0.0.50:8006/api2/json/cluster/firewall/ipset/iso1n0\": dial tcp 10.0.0.50:8006: connect: connection refused",
    ] {
        let s = with_ipset_left(error, true).await;
        let run = s.runner.open(&s.manifest, None).unwrap();
        let err = run.down(&opts("pve-run"), &Recorder::new()).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::ToolFailed, "{error}");
        assert!(s.fake.last(" state rm ").is_none(), "{error}");
        assert_eq!(s.fake.calls().iter().filter(|c| c.line().contains(" destroy ")).count(), 1, "{error}");
        let state = std::fs::read_to_string(s.runner.store.run_dir(run.name()).unwrap().join("terraform.tfstate")).unwrap();
        assert!(state.contains("iso1n0"), "the state keeps it: {error}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_destroy_that_keeps_failing_on_a_resource_already_gone_gives_up() {
    // A state rm that leaves the IP set: the destroy fails the same way every time.
    let s = with_ipset_left(IPSET_GONE, false).await;
    let run = s.runner.open(&s.manifest, None).unwrap();
    let err = run.down(&opts("pve-run"), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::ToolFailed);
    assert!(err.to_string().contains("no such IPSet 'iso1n0'"), "{err}");
    let count = |text: &str| s.fake.calls().iter().filter(|c| c.line().contains(text)).count();
    assert_eq!((count(" state rm "), count(" destroy ")), (3, 4));
}

#[tokio::test(start_paused = true)]
async fn the_leftovers_cleared_before_a_start_drop_what_is_already_gone_too() {
    // An embedder's case: a failed run's state is kept, and the next start clears it first.
    let s = scripted(HostOs::Linux, SOLO_WEB_PROXMOX, destroy_over_left_ipset(IPSET_GONE, true));
    s.set(Guest::Unreachable);
    let run = s.runner.open(&s.manifest, None).unwrap();
    run.up(&opts("pve-run"), &Recorder::new()).await.unwrap_err();
    std::fs::write(s.runner.store.run_dir(run.name()).unwrap().join("terraform.tfstate"), IPSET_LEFT).unwrap();
    s.set(Guest::Ready);
    let events = Recorder::new();
    run.up(&opts("pve-run"), &events).await.unwrap();
    assert_eq!(warnings(&events, crate::WarningCode::LeftoversRemoved).len(), 1);
    assert_eq!(warnings(&events, crate::WarningCode::AlreadyGone).len(), 1);
    assert!(s.fake.last(" state rm ").is_some());
    assert_eq!(run.status().await.unwrap().state, RunState::Ready);
}
