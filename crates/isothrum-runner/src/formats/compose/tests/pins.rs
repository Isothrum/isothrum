//! A Compose run's pinned host ports: written at its first start, kept by an update (a publish
//! added since gets one of its own), passed on every call, dropped at `down`.

use std::sync::Mutex;

use super::*;
use crate::formats::PickPorts;
use crate::net::HeldPort;
use crate::project::platform::Protocol;

/// Compose 5.6's config of a service publishing `"80"`, `"127.0.0.1::443"`, `"8080:8080"` and
/// `"[::1]::53/udp"`.
const CONFIG_PORTS: &str = include_str!("../../../tool_output/fixtures/compose/5.6-config-ports.json");

/// The `up` of a run with its override: the project's file, then the run's.
const UP_PINNED: &str = "docker compose -p lab -f * -f * up --detach --wait --remove-orphans";

/// Hands out 41001, 41002, … in turn.
struct Picks(Mutex<u16>);

impl PickPorts for Picks {
    fn pick(&self, _protocol: Protocol) -> std::io::Result<Option<HeldPort>> {
        let mut next = self.0.lock().expect("the next port");
        *next += 1;
        Ok(Some(HeldPort::new(crate::port::HostPort::new(*next), ())))
    }
}

/// `CONFIG_PORTS` with a service `api` publishing `"3000"` added.
fn with_api() -> String {
    let mut config: serde_json::Value = serde_json::from_str(CONFIG_PORTS).expect("a config");
    config["services"]["api"] = serde_json::json!({"ports": [{"mode": "ingress", "target": 3000, "protocol": "tcp"}]});
    config.to_string()
}

#[tokio::test]
async fn ephemeral_publishes_are_pinned_at_the_first_up_kept_by_an_update_and_dropped_at_down() {
    let (_dir, project) = folder();
    let run = run();
    let state = tempfile::tempdir().expect("a state folder");
    let picks = Picks(Mutex::new(41000));
    let first = FakeProcesses::script()
        .expect(CONFIG, Reply::ok(CONFIG_PORTS))
        .expect(UP_PINNED, Reply::ok(""))
        .build();
    Compose.up(&cx_in(&project, &run, &first, state.path(), &picks)).await.expect("up");
    let pins = state.path().join("compose-pins.yaml");
    let written = std::fs::read_to_string(&pins).expect("the override");
    assert!(written.ends_with(
        "  \"web\":\n    ports: !override\n      - \"41001:80/tcp\"\n      - \"127.0.0.1:41002:443/tcp\"\n      - \"8080:8080/tcp\"\n      - \"[::1]:41003:53/udp\"\n"
    ), "{written}");
    let up = first.last("up").expect("an up");
    assert!(
        up.has_run(&["-f", &pins.display().to_string()]),
        "the override after the project's files: {:?}",
        up.args
    );
    let config = first.last("config").expect("the config read");
    assert!(
        !config.args.iter().any(|a| a.ends_with("compose-pins.yaml")),
        "read without the pins: {:?}",
        config.args
    );

    let update = FakeProcesses::script()
        .expect(CONFIG, Reply::ok(&with_api()))
        .expect(UP_PINNED, Reply::ok(""))
        .build();
    Compose.update(&cx_in(&project, &run, &update, state.path(), &picks)).await.expect("updated");
    let kept = std::fs::read_to_string(&pins).expect("the override");
    assert!(kept.contains("\"41001:80/tcp\"") && kept.contains("\"[::1]:41003:53/udp\""), "kept: {kept}");
    assert!(
        kept.contains("  \"api\":\n    ports: !override\n      - \"41004:3000/tcp\"\n"),
        "the added publish's own: {kept}"
    );

    let down = FakeProcesses::script()
        .expect("docker compose -p lab -f * -f * down --volumes --remove-orphans", Reply::ok(""))
        .allow("docker", Reply::ok(""))
        .build();
    Compose.down(&cx_in(&project, &run, &down, state.path(), &picks)).await.expect("down");
    assert!(!pins.exists() && !state.path().join("compose-pins.json").exists(), "dropped with the run");
}
