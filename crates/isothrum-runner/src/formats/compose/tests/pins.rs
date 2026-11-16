//! A Compose run's pinned host ports: written at its first start, kept by an update (a publish
//! added since gets one of its own), passed on every call, dropped at `down`.

use std::sync::Mutex;

use super::*;
use crate::formats::PickPorts;
use crate::net::HeldPort;
use crate::port::HostPort;
use crate::project::platform::Protocol;

/// Compose 5.6's config of a service publishing `"80"`, `"127.0.0.1::443"`, `"8080:8080"` and
/// `"[::1]::53/udp"`.
const CONFIG_PORTS: &str = include_str!("../../../tool_output/fixtures/compose/5.6-config-ports.json");

/// The `up` of a run with its override: the project's file, then the run's.
const UP_PINNED: &str = "docker compose -p lab -f * -f * up --detach --wait --remove-orphans";
/// The pull a start makes first, the pins written.
const PULL_PINNED: &str = "docker compose -p lab -f * -f * pull --ignore-buildable";

/// Hands out 41001, 41002, … in turn; claims every fixed port but `taken`, and keeps the claims.
#[derive(Default)]
struct Picks {
    next: Mutex<u16>,
    taken: Option<HostPort>,
    claimed: Mutex<Vec<(u16, Protocol)>>,
}

impl Picks {
    fn from_41001() -> Picks {
        Picks {
            next: Mutex::new(41000),
            ..Picks::default()
        }
    }

    fn claimed(&self) -> Vec<(u16, Protocol)> {
        self.claimed.lock().expect("the claims").clone()
    }
}

impl PickPorts for Picks {
    fn pick(&self, _protocol: Protocol) -> std::io::Result<Option<HeldPort>> {
        let mut next = self.next.lock().expect("the next port");
        *next += 1;
        Ok(Some(HeldPort::new(HostPort::new(*next), ())))
    }

    fn claim(&self, ports: &[(HostPort, Protocol)]) -> crate::Result<()> {
        if let Some(&(port, protocol)) = ports.iter().find(|(p, _)| Some(*p) == self.taken) {
            return Err(crate::error::Error::PortInUse {
                holder: None,
                port,
                protocol: crate::encoding::wire_name(&protocol),
            });
        }
        self.claimed
            .lock()
            .expect("the claims")
            .extend(ports.iter().map(|(p, protocol)| (p.get(), *protocol)));
        Ok(())
    }
}

/// A Compose project, its run, a state folder of its own, and ports handed out from 41001.
fn bench() -> (tempfile::TempDir, Folder, RunName, tempfile::TempDir, Picks) {
    let (dir, project) = folder();
    let state = tempfile::tempdir().expect("a state folder");
    (dir, project, run(), state, Picks::from_41001())
}

/// `CONFIG_PORTS` with a service `api` publishing `"3000"` added.
fn with_api() -> String {
    let mut config: serde_json::Value = serde_json::from_str(CONFIG_PORTS).expect("a config");
    config["services"]["api"] = serde_json::json!({"ports": [{"mode": "ingress", "target": 3000, "protocol": "tcp"}]});
    config.to_string()
}

#[tokio::test]
async fn ephemeral_publishes_are_pinned_at_the_first_up_kept_by_an_update_and_dropped_at_down() {
    let (_dir, project, run, state, picks) = bench();
    let first = FakeProcesses::script()
        .expect(CONFIG, Reply::ok(CONFIG_PORTS))
        .expect(UP_PINNED, Reply::ok(""))
        .allow(PULL_PINNED, Reply::ok(""))
        .build();
    Compose.up(&cx_in(&project, &run, &first, state.path(), &picks)).await.expect("up");
    let pins = state.path().join("compose-pins.yaml");
    let written = std::fs::read_to_string(&pins).expect("the override");
    assert!(written.ends_with(
        "  \"web\":\n    ports: !override\n      - \"41001:80/tcp\"\n      - \"127.0.0.1:41002:443/tcp\"\n      - \"8080:8080/tcp\"\n      - \"[::1]:41003:53/udp\"\n"
    ), "{written}");
    let [up] = first.made(UP_PINNED).try_into().expect("one up");
    let files: Vec<&Path> = up.args.windows(2).filter(|w| w[0] == "-f").map(|w| Path::new(&w[1])).collect();
    assert_eq!(files.last(), Some(&pins.as_path()), "the override after the project's files: {:?}", up.args);
    // The config is read without the pins: the script's `CONFIG` names one file, and a read with
    // two would be a call it doesn't answer.
    assert_eq!(picks.claimed(), [(8080, Protocol::Tcp)], "the first start claims the port the project fixes");

    let update = FakeProcesses::script()
        .expect(CONFIG, Reply::ok(&with_api()))
        .expect(UP_PINNED, Reply::ok(""))
        .allow(PULL_PINNED, Reply::ok(""))
        .expect("docker compose -p lab -f * -f * config --services", Reply::ok("web\napi\n"))
        .expect(
            "docker compose -p lab -f * -f * ps --all --format json",
            Reply::ok(&ps_line("web", "running", "", 0, None)),
        )
        .build();
    Compose.update(&cx_in(&project, &run, &update, state.path(), &picks)).await.expect("updated");
    assert_eq!(
        picks.claimed(),
        [(8080, Protocol::Tcp)],
        "an update claims nothing: the run's own containers hold its ports"
    );
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

#[tokio::test]
async fn an_attached_services_ephemeral_port_is_pinned_kept_by_a_start_and_dropped_at_its_retire() {
    let (_dir, project, run, state, picks) = bench();
    let attackbox = r#"{"services": {"attackbox": {"ports": [{"mode": "ingress", "target": 7681, "protocol": "tcp"}]}}}"#;
    let attached = format!("{CONFIG} attackbox");
    let fake = FakeProcesses::script()
        .expect_times(CONFIG, 2, Reply::ok(NO_PORTS))
        .expect_times(&attached, 2, Reply::ok(attackbox))
        .expect("docker compose -p lab -f * -f * up --detach --wait attackbox", Reply::ok(""))
        .expect(UP_PINNED, Reply::ok(""))
        .allow(PULL_PINNED, Reply::ok(""))
        .expect("docker compose -p lab -f * -f * rm --stop --force --volumes attackbox", Reply::ok(""))
        .build();
    let cx = cx_in(&project, &run, &fake, state.path(), &picks);
    let pins = state.path().join("compose-pins.yaml");
    Compose.attach(&cx, "attackbox").await.expect("attached");
    let pinned = std::fs::read_to_string(&pins).expect("the override");
    assert!(
        pinned.ends_with("  \"attackbox\":\n    ports: !override\n      - \"41001:7681/tcp\"\n"),
        "{pinned}"
    );
    Compose.up(&cx).await.expect("a start keeps it");
    assert_eq!(std::fs::read_to_string(&pins).expect("the override"), pinned, "the same port");
    Compose.retire(&cx, "attackbox").await.expect("retired");
    assert!(!pins.exists(), "its pin went with it, and nothing else is pinned");
}

#[tokio::test]
async fn a_first_start_whose_fixed_host_port_is_taken_is_refused_before_anything_is_made() {
    let (_dir, project, run, state, _) = bench();
    let picks = Picks {
        taken: Some(HostPort::new(8080)),
        ..Picks::from_41001()
    };
    // Only the config is scripted: a pull, an `up` or a `down` would be a call the script
    // doesn't answer.
    let fake = FakeProcesses::script().expect(CONFIG, Reply::ok(CONFIG_PORTS)).build();
    let refused = Compose.up(&cx_in(&project, &run, &fake, state.path(), &picks)).await.expect_err("taken");
    assert_eq!(
        refused.detail().map(|d| d.data),
        Some(crate::Detail::PortInUse {
            port: 8080,
            protocol: "tcp".to_owned(),
            holder: None,
        }),
        "{refused}"
    );
    assert!(!state.path().join("compose-pins.yaml").exists(), "nothing pinned either");
}
