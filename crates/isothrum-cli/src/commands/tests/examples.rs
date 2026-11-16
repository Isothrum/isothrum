//! The projects in the repository's `examples/` folder, run through the command line exactly as
//! their READMEs show, on a model of each one's tool: an example the runner no longer runs, or a
//! README whose commands no longer do what it says, fails here. Each model reads the example's
//! own file for what it holds (the services, the machine), so the commands meet the names the
//! file gives. The Compose example also runs on the real Docker: `tests/real_run.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use clap::Parser;
use isothrum_runner::fake::{Call, FakeProcesses, Reply};
use isothrum_runner::{FakePorts, HostOs, Runner, RunnerConfig};
use serde_json::{Value, json};

use crate::args::Cli;
use crate::commands;
use crate::exit::Verdict;
use crate::term::{Term, Trail};
use crate::testing::Buf;

/// What a README command does, as its `--json` output says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Said {
    /// An operation whose result leaves the run in this state.
    Result(&'static str),
    /// `status`: the run in this state.
    Status(&'static str),
    /// `exec`: the command succeeded, printing this line.
    Printed(&'static str),
    /// Refused with this error code.
    Refused(&'static str),
}

/// An example copied to a folder of its own (named as it is, so the run is too), and a runner
/// whose tools are `fake`.
struct Example {
    _home: tempfile::TempDir,
    _copy: tempfile::TempDir,
    dir: PathBuf,
    runner: Runner,
    term: Term,
    out: Buf,
}

/// The repository's `examples/<name>`.
fn source(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples").join(name)
}

impl Example {
    fn new(name: &str, fake: Arc<FakeProcesses>) -> Example {
        let (home, copy) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let dir = copy.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        for file in std::fs::read_dir(source(name)).unwrap() {
            let file = file.unwrap();
            std::fs::copy(file.path(), dir.join(file.file_name())).unwrap();
        }
        let mut config = RunnerConfig::new(home.path());
        config.processes = fake;
        config.ports = Arc::new(FakePorts::new([41001, 41002]));
        config.host = HostOs::Linux;
        let out = Buf::default();
        Example {
            runner: Runner::new(config).unwrap(),
            term: Term::new(Box::new(out.clone()), Box::new(Buf::default())),
            _home: home,
            _copy: copy,
            dir,
            out,
        }
    }

    /// `isothrum <words>` as the README writes it, run on the example's folder with `--json`: its
    /// verdict and its JSON lines.
    async fn isothrum(&self, words: &[&str]) -> (Verdict, Vec<Value>) {
        let split = words.iter().position(|w| *w == "--").unwrap_or(words.len());
        let dir = self.dir.to_str().unwrap();
        let line = [&["isothrum"][..], &words[..split], &[dir, "--json"], &words[split..]].concat();
        let cli = Cli::try_parse_from(&line).unwrap_or_else(|e| panic!("{line:?}: {e}"));
        let verdict = commands::run(&cli, || Ok(&self.runner), &self.term, &Trail::default(), &|_| None).await;
        let verdict = verdict.unwrap_or_else(|e| panic!("{line:?}: {e}"));
        let lines = self.out.take().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        (verdict, lines)
    }

    /// Runs each of `steps` in order, each doing what it says.
    async fn runs(&self, steps: &[(&str, Said)]) {
        for (command, said) in steps {
            let words: Vec<&str> = command.split_whitespace().skip(1).collect();
            let (verdict, lines) = self.isothrum(&words).await;
            let last = lines.last().cloned().unwrap_or_default();
            let (pass, seen) = match said {
                Said::Result(state) => (true, last["event"] == "result" && last["data"]["state"] == *state),
                Said::Status(state) => (true, lines.len() == 1 && last["state"] == *state),
                Said::Printed(text) => (
                    true,
                    last["event"] == "result" && lines.iter().any(|e| e["event"] == "log" && e["stream"] == "stdout" && e["line"] == *text),
                ),
                Said::Refused(code) => (false, last["event"] == "error" && last["code"] == *code),
            };
            assert_eq!(verdict, Verdict::of(pass), "{command}: {lines:#?}");
            assert!(seen, "{command}: expected {said:?}, got {lines:#?}");
        }
    }
}

/// The end of a README whose run parks: park, resume and down, the status read after each but
/// the resume.
const PARKED_RESUMED_REMOVED: [(&str, Said); 5] = [
    ("isothrum park", Said::Result("parked")),
    ("isothrum status", Said::Status("parked")),
    ("isothrum resume", Said::Result("ready")),
    ("isothrum down", Said::Result("absent")),
    ("isothrum status", Said::Status("absent")),
];

/// The `isothrum` commands of the example's README, in order.
fn readme_commands(name: &str) -> Vec<String> {
    let readme = std::fs::read_to_string(source(name).join("README.md")).unwrap();
    readme.lines().filter(|l| l.starts_with("isothrum ")).map(str::to_owned).collect()
}

/// Asserts the README of `name` gives `steps`' commands, in their order, and runs them on `fake`.
async fn as_its_readme_says(name: &str, fake: Arc<FakeProcesses>, steps: &[(&str, Said)]) {
    let commands: Vec<&str> = steps.iter().map(|(c, _)| *c).collect();
    assert_eq!(readme_commands(name), commands, "the README of `{name}` and this test name the same commands");
    Example::new(name, fake).runs(steps).await;
}

/// The arguments of `call` after the `-p <name>` and `-f <file>` pairs a Compose call starts with.
fn compose_rest(call: &Call) -> Vec<&str> {
    let mut args = call.args.iter().map(String::as_str).skip(1).peekable();
    while args.next_if(|a| matches!(*a, "-p" | "-f" | "--project-directory" | "--env-file")).is_some() {
        args.next();
    }
    args.collect()
}

/// Docker with Compose, holding the services the example's `compose.yaml` names, each running,
/// exited or gone; `web` publishes port 80, as the file has it.
fn docker(services: Vec<String>) -> FakeProcesses {
    let held: Mutex<BTreeMap<String, &str>> = Mutex::new(BTreeMap::new());
    FakeProcesses::model(move |call| {
        assert_eq!(call.program, "docker", "{}", call.line());
        let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
        let mut held = held.lock().unwrap();
        let set = |engine: &mut BTreeMap<String, &str>, state| {
            for service in &services {
                engine.insert(service.clone(), state);
            }
        };
        match (args.as_slice(), compose_rest(call).as_slice()) {
            (["compose", "version", "--short"], _) => Reply::ok("2.29.7"),
            ([kind, "ls", ..], _) if *kind != "compose" => Reply::ok(""),
            (_, ["config", "--format", "json"]) => {
                let ports = json!([{ "mode": "ingress", "host_ip": "127.0.0.1", "target": 80, "protocol": "tcp" }]);
                let config: serde_json::Map<String, Value> = services
                    .iter()
                    .map(|s| (s.clone(), if s == "web" { json!({ "ports": ports }) } else { json!({}) }))
                    .collect();
                Reply::ok(&json!({ "services": config }).to_string())
            }
            (_, ["config", "--services"]) => Reply::ok(&services.join("\n")),
            (_, ["pull" | "create", ..]) => Reply::ok(""),
            (_, ["up" | "start", ..]) => {
                set(&mut held, "running");
                Reply::ok("")
            }
            (_, ["stop", ..]) => {
                set(&mut held, "exited");
                Reply::ok("")
            }
            (_, ["down", ..]) => {
                held.clear();
                Reply::ok("")
            }
            (_, ["ps", "--all", "--format", "json"]) => {
                let lines: Vec<String> = held
                    .iter()
                    .map(|(service, state)| {
                        let publishers = if service == "web" {
                            json!([{ "URL": "127.0.0.1", "TargetPort": 80, "PublishedPort": 41001, "Protocol": "tcp" }])
                        } else {
                            json!([])
                        };
                        json!({ "ID": format!("{service:0>12}"), "Service": service, "State": state, "Health": "", "ExitCode": 0, "Publishers": publishers, "Labels": "" }).to_string()
                    })
                    .collect();
                Reply::ok(&lines.join("\n"))
            }
            (_, ["exec", "-T", .., service, "redis-cli", "ping"]) if held.get(*service) == Some(&"running") => Reply::ok("PONG"),
            (_, rest) => panic!("a Docker call the model doesn't answer: {rest:?} ({})", call.line()),
        }
    })
}

/// The services `compose.yaml` names: its keys two spaces in under `services:`.
fn services_of(compose: &str) -> Vec<String> {
    let mut lines = compose.lines().skip_while(|l| *l != "services:").skip(1);
    let mut services = Vec::new();
    for line in lines.by_ref().take_while(|l| l.is_empty() || l.starts_with(' ') || l.starts_with('#')) {
        if let Some(name) = line.strip_prefix("  ").and_then(|l| l.strip_suffix(':')).filter(|n| !n.starts_with([' ', '#'])) {
            services.push(name.to_owned());
        }
    }
    services
}

#[tokio::test]
async fn the_compose_example_runs_as_its_readme_says() {
    let compose = std::fs::read_to_string(source("compose-web-cache").join("compose.yaml")).unwrap();
    let services = services_of(&compose);
    assert_eq!(services, ["web", "cache"], "the services the README names");
    as_its_readme_says(
        "compose-web-cache",
        Arc::new(docker(services)),
        &[
            ("isothrum up", Said::Result("ready")),
            ("isothrum status", Said::Status("ready")),
            ("isothrum exec cache -- redis-cli ping", Said::Printed("PONG")),
        ]
        .iter()
        .chain(&PARKED_RESUMED_REMOVED)
        .copied()
        .collect::<Vec<_>>(),
    )
    .await;
}

/// Vagrant on VirtualBox, holding the one machine the example's `Vagrantfile` defines: its state
/// as `vagrant status` writes it (`not_created`, `running`, `poweroff`).
fn vagrant(machine: String) -> FakeProcesses {
    let state = Mutex::new("not_created");
    FakeProcesses::model(move |call| {
        assert_eq!(call.program, "vagrant", "{}", call.line());
        let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
        let mut state = state.lock().unwrap();
        let mut to = |s| {
            *state = s;
            Reply::ok("")
        };
        match args.as_slice() {
            ["--version"] => Reply::ok("Vagrant 2.4.9"),
            ["up", "--provider", "virtualbox", ..] => to("running"),
            ["halt", ..] => to("poweroff"),
            ["destroy", "-f", ..] => to("not_created"),
            ["status", "--machine-readable"] => Reply::ok(&format!(
                "1794305819,{machine},metadata,provider,virtualbox\n1794305819,{machine},provider-name,virtualbox\n1794305819,{machine},state,{state}"
            )),
            ["port", m, "--machine-readable"] if *m == machine => Reply::ok(&format!("1794305819,{machine},forwarded_port,22,2222")),
            ["ssh", m, "-c", "cat /etc/greeting", ..] if *m == machine && *state == "running" => Reply::ok("set up by Vagrant's shell provisioner"),
            _ => panic!("a Vagrant call the model doesn't answer: {}", call.line()),
        }
    })
}

/// The machine the `Vagrantfile` defines (`config.vm.define "<name>"`).
fn machine_of(vagrantfile: &str) -> String {
    let defined = vagrantfile.lines().find_map(|l| l.trim().strip_prefix("config.vm.define \""));
    defined.and_then(|l| l.split('"').next()).unwrap().to_owned()
}

#[tokio::test]
async fn the_vagrant_example_runs_as_its_readme_says() {
    let vagrantfile = std::fs::read_to_string(source("vagrant-one-vm").join("Vagrantfile")).unwrap();
    let machine = machine_of(&vagrantfile);
    assert_eq!(machine, "server", "the machine the README names");
    as_its_readme_says(
        "vagrant-one-vm",
        Arc::new(vagrant(machine)),
        &[
            ("isothrum up --provider virtualbox", Said::Result("ready")),
            ("isothrum status", Said::Status("ready")),
            (
                "isothrum exec server -- cat /etc/greeting",
                Said::Printed("set up by Vagrant's shell provisioner"),
            ),
        ]
        .iter()
        .chain(&PARKED_RESUMED_REMOVED)
        .copied()
        .collect::<Vec<_>>(),
    )
    .await;
}

/// Terraform applying the example's module: its state file written at `-state=` and removed by
/// `destroy`, and its outputs from the `greeting` the run gives it (`TF_VAR_greeting`), as
/// `main.tf` makes them.
fn terraform() -> FakeProcesses {
    FakeProcesses::model(|call| {
        assert_eq!(call.program, "terraform", "{}", call.line());
        let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
        let state = args.iter().find_map(|a| a.strip_prefix("-state=")).map(PathBuf::from);
        let greeting = call.env("TF_VAR_greeting").unwrap_or("hello");
        let message = format!("{greeting} from a Terraform module");
        let outputs = json!({
            "length": { "sensitive": false, "type": "number", "value": message.len() },
            "message": { "sensitive": false, "type": "string", "value": message },
        });
        match args.as_slice() {
            ["version", "-json"] => Reply::ok(r#"{"terraform_version": "1.9.8", "platform": "linux_amd64", "provider_selections": {}}"#),
            ["init", ..] => Reply::ok(""),
            ["apply", ..] => {
                std::fs::write(state.unwrap(), "{}").unwrap();
                Reply::ok("")
            }
            ["destroy", ..] => {
                std::fs::remove_file(state.unwrap()).unwrap();
                Reply::ok("")
            }
            ["show", "-json", ..] => {
                let resource = |name: &str, deps: Value| {
                    json!({ "address": format!("terraform_data.{name}"), "mode": "managed", "type": "terraform_data", "name": name,
                        "provider_name": "terraform.io/builtin/terraform", "values": { "id": format!("{name}-1") }, "depends_on": deps })
                };
                let resources = [resource("message", json!([])), resource("stamp", json!(["terraform_data.message"]))];
                Reply::ok(&json!({ "format_version": "1.0", "values": { "outputs": outputs, "root_module": { "resources": resources } } }).to_string())
            }
            ["output", "-json", ..] => Reply::ok(&outputs.to_string()),
            _ => panic!("a Terraform call the model doesn't answer: {}", call.line()),
        }
    })
}

#[tokio::test]
async fn the_terraform_example_runs_as_its_readme_says() {
    as_its_readme_says(
        "terraform-local",
        Arc::new(terraform()),
        &[
            ("isothrum up", Said::Result("ready")),
            ("isothrum status", Said::Status("ready")),
            ("isothrum update --env greeting=hi", Said::Result("ready")),
            ("isothrum status", Said::Status("ready")),
            ("isothrum exec vm -- uname -a", Said::Refused("machine_unknown")),
            ("isothrum park", Said::Refused("unsupported")),
            ("isothrum down", Said::Result("absent")),
            ("isothrum status", Said::Status("absent")),
        ],
    )
    .await;
}
