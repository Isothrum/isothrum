//! The format contract, run against a model of Docker and Compose.

use std::sync::Mutex;

use crate::formats::contract::{self, Model, Op, RunFormat as Format, RunProvider as Provider, ToolCall as Call, ToolReply as Reply};

const VERSION: &str = include_str!("../../../tool_output/fixtures/compose/2.29-version-short.txt");
const TOO_OLD: &str = include_str!("../../../tool_output/fixtures/compose/2.16-version-short.txt");
const PS: &str = include_str!("../../../tool_output/fixtures/compose/2.29-ps.jsonl");
const CONTAINER_GONE: &str = include_str!("../../../tool_output/fixtures/compose/27.1-container-rm-gone.txt");
const NETWORK_GONE: &str = include_str!("../../../tool_output/fixtures/compose/27.1-network-rm-gone.txt");
const VOLUME_GONE: &str = include_str!("../../../tool_output/fixtures/compose/27.1-volume-rm-gone.txt");

/// The short ids an `up` makes, as `ls --quiet` prints them: the containers `PS` lists, the
/// project's network and its volume.
const CONTAINERS: [&str; 2] = ["4b0f1d7e2c3a", "9a8b7c6d5e4f"];
const NETWORK: &str = "3f2e1d0c9b8a";
const VOLUME: &str = "stack_data";

/// Docker with Compose: the objects of project `stack`, by kind.
pub(super) struct Docker {
    old: bool,
    held: Mutex<Vec<(&'static str, String)>>,
}

impl Docker {
    fn holding(&self, kind: &str) -> Vec<String> {
        let held = self.held.lock().expect("the world");
        held.iter().filter(|(k, _)| *k == kind).map(|(_, id)| id.clone()).collect()
    }

    /// `docker <kind> rm <ids>`: each removed, or Docker's "no such" for one it doesn't hold.
    fn remove(&self, kind: &str, ids: &[&str]) -> Reply {
        let mut held = self.held.lock().expect("the world");
        for id in ids {
            let Some(at) = held.iter().position(|(k, i)| *k == kind && i == id) else {
                let (gone, recorded) = match kind {
                    "container" => (CONTAINER_GONE, CONTAINERS[0]),
                    "network" => (NETWORK_GONE, NETWORK),
                    _ => (VOLUME_GONE, VOLUME),
                };
                return Reply::fail(1, &gone.replace(recorded, id));
            };
            held.remove(at);
        }
        Reply::ok(&ids.join("\n"))
    }

    /// `compose -p stack -f <file> <rest>`.
    fn compose(&self, rest: &[&str]) -> Reply {
        match rest {
            ["up", "--detach", "--wait", "--remove-orphans"] => {
                let mut held = self.held.lock().expect("the world");
                held.clear();
                held.extend(CONTAINERS.iter().map(|id| ("container", (*id).to_owned())));
                held.push(("network", NETWORK.to_owned()));
                held.push(("volume", VOLUME.to_owned()));
                Reply::ok("")
            }
            ["ps", "--all", "--format", "json"] => {
                let running = self.holding("container");
                let lines: Vec<&str> = PS.lines().filter(|l| running.iter().any(|id| l.contains(&format!(r#""ID":"{id}"#)))).collect();
                Reply::ok(&lines.join("\n"))
            }
            ["config", "--services"] => Reply::ok("web\ndb\n"),
            // The images are there: nothing to pull.
            ["pull", "--ignore-buildable"] => Reply::ok(""),
            ["config", "--format", "json"] => Reply::ok(r#"{"services": {"web": {}, "db": {}}}"#),
            ["down", "--volumes", "--remove-orphans"] => {
                self.held.lock().expect("the world").clear();
                Reply::ok("")
            }
            other => panic!("a Compose call the model doesn't know: {other:?}"),
        }
    }
}

impl Model for Docker {
    const FORMAT: Format = Format::Compose;
    const PROVIDER: Option<Provider> = None;
    const FILE: (&'static str, &'static str) = (
        "compose.yaml",
        "services:\n  web:\n    image: nginx:1.27\n    ports: [\"127.0.0.1::80\"]\n  db:\n    image: postgres:16\n",
    );
    const VERSION: &'static str = "docker compose version --short";
    const MACHINES: &'static [&'static str] = &["db", "web"];
    const REFUSES: &'static [Op] = &[Op::Provision];

    fn new(old: bool) -> Docker {
        Docker {
            old,
            held: Mutex::new(Vec::new()),
        }
    }

    fn answer(&self, call: &Call) -> Reply {
        assert_eq!(call.program, "docker", "{}", call.line());
        let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
        match args.as_slice() {
            ["compose", "version", "--short"] => Reply::ok(if self.old { TOO_OLD } else { VERSION }),
            ["compose", "-p", "stack", "-f", _, rest @ ..] => self.compose(rest),
            [kind, "ls", "--quiet", "--filter", "label=com.docker.compose.project=stack", ..] => Reply::ok(&self.holding(kind).join("\n")),
            ["container", "rm", "--force", "--volumes", ids @ ..] => self.remove("container", ids),
            [kind @ ("network" | "volume"), "rm", ids @ ..] => self.remove(kind, ids),
            other => panic!("a Docker call the model doesn't know: {other:?}"),
        }
    }

    fn holds(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.held.lock().expect("the world").iter().map(|(_, id)| id.clone()).collect();
        ids.sort();
        ids
    }
}

contract::suite!(Docker);
