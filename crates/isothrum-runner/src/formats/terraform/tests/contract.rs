//! The format contract, run against a model of Terraform applying a libvirt module.

use std::sync::Mutex;

use crate::formats::contract::{self, Model, Op, RunFormat as Format, RunProvider as Provider, ToolCall as Call, ToolReply as Reply, flag_path};

const VERSION: &str = include_str!("../../../tool_output/fixtures/terraform/1.9-version.json");
const TOO_OLD: &str = include_str!("../../../tool_output/fixtures/terraform/0.13-version.json");
const APPLY: &str = include_str!("../../../tool_output/fixtures/terraform/1.16-apply.jsonl");
const SHOW: &str = include_str!("../../../tool_output/fixtures/terraform/1.16-show-vm.json");
const OUTPUT: &str = include_str!("../../../tool_output/fixtures/terraform/1.16-output.json");
const DESTROY: &str = include_str!("../../../tool_output/fixtures/terraform/1.16-destroy.jsonl");
const DESTROYED_ONE: &str = include_str!("../../../tool_output/fixtures/terraform/1.16-destroy-target.jsonl");
const NOT_THERE: &str = include_str!("../../../tool_output/fixtures/terraform/1.16-import-non-existent.txt");

/// Terraform and the provider's objects: the managed instances of `SHOW` still there, by id.
pub(crate) struct LibvirtModule {
    old: bool,
    objects: Mutex<Vec<String>>,
}

/// Each managed instance of `SHOW`: its address and id.
fn instances() -> Vec<(String, String)> {
    let doc: serde_json::Value = serde_json::from_str(SHOW).expect("a state");
    doc["values"]["root_module"]["resources"]
        .as_array()
        .expect("resources")
        .iter()
        .map(|r| {
            (
                r["address"].as_str().expect("an address").to_owned(),
                r["values"]["id"].as_str().expect("an id").to_owned(),
            )
        })
        .collect()
}

impl LibvirtModule {
    /// `show -json`: `SHOW` with only the instances still there.
    fn show(&self) -> Reply {
        let there = self.objects.lock().expect("the world").clone();
        let mut doc: serde_json::Value = serde_json::from_str(SHOW).expect("a state");
        if let Some(resources) = doc["values"]["root_module"]["resources"].as_array_mut() {
            resources.retain(|r| r["values"]["id"].as_str().is_some_and(|id| there.iter().any(|t| t == id)));
        }
        Reply::ok(&doc.to_string())
    }

    /// `apply` (`-target=<address>` given: of that instance only) or `destroy`.
    fn change(&self, call: &Call, destroy: bool) -> Reply {
        let state = flag_path(call, "-state=").expect("the run's own state file");
        let target = call.args.iter().find_map(|a| a.strip_prefix("-target="));
        let mut objects = self.objects.lock().expect("the world");
        if !destroy {
            std::fs::write(state, "{}").expect("the state written");
            *objects = instances().into_iter().map(|(_, id)| id).collect();
            return Reply::ok(APPLY);
        }
        let Some(target) = target else {
            objects.clear();
            return Reply::ok(DESTROY);
        };
        let gone: Vec<String> = instances().into_iter().filter(|(at, _)| at == target).map(|(_, id)| id).collect();
        objects.retain(|id| !gone.contains(id));
        Reply::ok(DESTROYED_ONE)
    }
}

impl Model for LibvirtModule {
    const FORMAT: Format = Format::Terraform;
    const PROVIDER: Option<Provider> = None;
    const FILE: (&'static str, &'static str) = ("main.tf", "resource \"libvirt_domain\" \"web\" {}\nresource \"libvirt_network\" \"lab\" {}\n");
    const VERSION: &'static str = "terraform version -json";
    const MACHINES: &'static [&'static str] = &["web"];
    const REFUSES: &'static [Op] = &[Op::Park, Op::Resume, Op::Provision, Op::Attach, Op::Retire, Op::Exec, Op::Console];

    fn new(old: bool) -> LibvirtModule {
        LibvirtModule {
            old,
            objects: Mutex::new(Vec::new()),
        }
    }

    fn answer(&self, call: &Call) -> Reply {
        assert_eq!(call.program, "terraform", "{}", call.line());
        let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
        match args.as_slice() {
            ["version", "-json"] => Reply::ok(if self.old { TOO_OLD } else { VERSION }),
            ["init", "-input=false", "-no-color", ..] => Reply::ok("Terraform has been successfully initialized!"),
            ["apply", "-auto-approve", "-input=false", "-json", ..] => self.change(call, false),
            ["destroy", "-auto-approve", "-input=false", "-json", ..] => self.change(call, true),
            ["show", "-json", "-no-color", _] => self.show(),
            ["output", "-json", "-no-color", _] => Reply::ok(OUTPUT),
            ["import", "-input=false", "-no-color", _, _, id] => {
                assert!(
                    !self.objects.lock().expect("the world").iter().any(|o| o == id),
                    "an import of {id}, which the state holds"
                );
                Reply::fail(1, NOT_THERE)
            }
            other => panic!("a Terraform call the model doesn't know: {other:?}"),
        }
    }

    fn holds(&self) -> Vec<String> {
        let mut ids = self.objects.lock().expect("the world").clone();
        ids.sort();
        ids
    }
}

contract::suite!(LibvirtModule);
