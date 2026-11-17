mod by_id;
mod changes;
mod console;
pub(crate) mod contract;
mod jump;
mod proxmox;
mod refusals;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::ErrorCode;
use crate::RunName;
use crate::process::fake::{Call as Made, FakeProcesses, Reply};
use crate::project::{Format, Project};
use crate::status::MachineState;

const SHOW: &str = include_str!("../../tool_output/fixtures/terraform/1.16-show.json");
const DATA_ONLY: &str = include_str!("../../tool_output/fixtures/terraform/1.16-show-data-only.json");
const OUTPUT: &str = include_str!("../../tool_output/fixtures/terraform/1.16-output.json");
const APPLY: &str = include_str!("../../tool_output/fixtures/terraform/1.16-apply.jsonl");
const PLAN: &str = include_str!("../../tool_output/fixtures/terraform/1.16-plan.jsonl");
const LOCKED: &str = include_str!("../../tool_output/fixtures/terraform/1.16-apply-locked.jsonl");
const GONE: &str = include_str!("../../tool_output/fixtures/terraform/1.16-destroy-gone.jsonl");
const PROVISIONER_FAILED: &str = include_str!("../../tool_output/fixtures/terraform/1.16-destroy-provisioner-failed.jsonl");
const DESTROYED_ONE: &str = include_str!("../../tool_output/fixtures/terraform/1.16-destroy-target.jsonl");

/// The secret input of every test's run.
const SECRET: &str = "s3cretTOKEN";

/// A module in a folder of its own, and the run's state folder apart.
struct Stack {
    module: tempfile::TempDir,
    state: tempfile::TempDir,
    project: Project,
    run: RunName,
    lines: Mutex<Vec<String>>,
    warnings: Mutex<Vec<(WarningCode, String)>>,
}

fn stack() -> Stack {
    let module = tempfile::tempdir().expect("a folder");
    std::fs::write(module.path().join("main.tf"), "resource \"terraform_data\" \"web\" {}\n").expect("a module");
    let project = Project::open(module.path(), Some(Format::Terraform)).expect("a project");
    Stack {
        module,
        state: tempfile::tempdir().expect("a state folder"),
        project,
        run: RunName::new("stack").expect("a name"),
        lines: Mutex::new(Vec::new()),
        warnings: Mutex::new(Vec::new()),
    }
}

impl Stack {
    fn state_file(&self) -> PathBuf {
        self.state.path().join(STATE_FILE)
    }

    /// `-state=<the run's state file>`.
    fn state_flag(&self) -> String {
        format!("-state={}", self.state_file().display())
    }

    /// The run's state file, written (its content is the fake's: `show` reads it).
    fn with_state(self) -> Stack {
        std::fs::write(self.state_file(), "{}").expect("a state");
        self
    }

    /// `init` chose the backend `name`.
    fn with_backend(self, name: &str) -> Stack {
        let data = self.state.path().join(DATA_DIR);
        std::fs::create_dir_all(&data).expect("a data folder");
        let doc = format!(r#"{{"version":3,"backend":{{"type":"{name}","config":{{}}}}}}"#);
        std::fs::write(data.join("terraform.tfstate"), doc).expect("a backend file");
        self
    }

    fn lines(&self) -> Vec<String> {
        self.lines.lock().expect("lines").clone()
    }

    fn warnings(&self) -> Vec<WarningCode> {
        self.warnings.lock().expect("warnings").iter().map(|(c, _)| *c).collect()
    }
}

/// Runs `f` with a context for `stack` on `fake`.
async fn with_cx<T>(stack: &Stack, fake: &FakeProcesses, f: impl AsyncFnOnce(&ToolCx<'_>) -> T) -> T {
    with_cx_given(stack, fake, &[], f).await
}

/// [`with_cx`], the run given `extra` values too.
async fn with_cx_given<T>(stack: &Stack, fake: &FakeProcesses, extra: &[(&str, &str)], f: impl AsyncFnOnce(&ToolCx<'_>) -> T) -> T {
    let sink = |_: Stream, l: &str| stack.lines.lock().expect("lines").push(l.to_owned());
    let warn = |c: WarningCode, m: &str| stack.warnings.lock().expect("warnings").push((c, m.to_owned()));
    let cx = ToolCx {
        project: &stack.project,
        run: &stack.run,
        key: "k3y9ab",
        state_dir: stack.state.path(),
        provider: None,
        env: vec![
            ("name".to_owned(), "web".to_owned()),
            ("token".to_owned(), SECRET.to_owned()),
            ("TF_VAR_name".to_owned(), "web".to_owned()),
            ("TF_VAR_token".to_owned(), SECRET.to_owned()),
            ("ISOTHRUM_RUN_KEY".to_owned(), "k3y9ab".to_owned()),
        ]
        .into_iter()
        .chain(extra.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())))
        .collect(),
        host_env: &crate::host_env::EMPTY,
        processes: fake,
        ssh_key: std::path::Path::new("/nowhere/id_ed25519"),
        ports: &crate::formats::NoPorts,
        sink: &sink,
        warn: &warn,
        made: crate::formats::NOTHING_MADE,
        fetched: None,
    };
    f(&cx).await
}

/// A state of one root `<kind>.<name>` instance of id `id` per entry, tainted when told.
fn show_of(resources: &[(&str, &str, &str, bool)]) -> String {
    let resources: Vec<serde_json::Value> = resources
        .iter()
        .map(|(kind, name, id, tainted)| {
            serde_json::json!({
                "address": format!("{kind}.{name}"), "mode": "managed", "type": kind, "name": name,
                "values": { "id": id }, "tainted": tainted
            })
        })
        .collect();
    serde_json::json!({ "format_version": "1.0", "values": { "root_module": { "resources": resources } } }).to_string()
}

/// A `planned_change` line for `<kind>.<name>`.
fn planned_line(kind: &str, name: &str, action: &str) -> String {
    serde_json::json!({
        "@level": "info", "@message": format!("{kind}.{name}: Plan to {action}"), "type": "planned_change",
        "change": { "resource": { "addr": format!("{kind}.{name}"), "resource_type": kind, "resource_name": name }, "action": action }
    })
    .to_string()
}

fn failing(stdout: &str) -> Reply {
    Reply { code: 1, ..Reply::ok(stdout) }
}

/// The calls made, as `program arg…` lines with the run's state folder written `<state>` and the
/// paths under it with `/` (Windows joins them with `\\`).
fn made(fake: &FakeProcesses, stack: &Stack) -> Vec<String> {
    let state = stack.state.path().display().to_string();
    let arg = |a: &String| {
        let a = a.replace(&state, "<state>");
        if a.contains("<state>") { a.replace('\\', "/") } else { a }
    };
    fake.calls()
        .iter()
        .map(|c| std::iter::once(c.program.clone()).chain(c.args.iter().map(arg)).collect::<Vec<_>>().join(" "))
        .collect()
}

#[tokio::test]
async fn up_inits_then_applies_on_the_runs_own_state_with_inputs_in_the_environment_only() {
    let stack = stack();
    std::fs::write(stack.state.path().join(LOCK_INFO), "{}").expect("a killed run's lock information");
    let fake = FakeProcesses::script()
        .expect(
            "terraform init -input=false -no-color",
            Reply::ok("Terraform has been successfully initialized!"),
        )
        .expect(
            &format!("terraform apply -auto-approve -input=false -json {}", stack.state_flag()),
            Reply::ok(APPLY),
        )
        .build();
    with_cx(&stack, &fake, async |cx| Terraform.up(cx).await).await.expect("up");
    let apply = fake.last("terraform apply").expect("an apply");
    let data = stack.state.path().join(DATA_DIR);
    assert_eq!(apply.env("TF_DATA_DIR"), Some(data.to_str().expect("a path")));
    assert_eq!(apply.env("TF_IN_AUTOMATION"), Some("1"));
    assert_eq!(apply.env("TF_VAR_token"), Some(SECRET));
    isothrum_test_support::assert_every!(fake.calls(), |c: &Made| !c.args.iter().any(|a| a.contains(SECRET) || a.starts_with("-var")));
    assert!(!stack.state.path().join(LOCK_INFO).exists(), "the stale lock information is removed");
    assert_eq!(
        std::fs::read_dir(stack.module.path()).expect("the module").count(),
        1,
        "nothing written beside the module"
    );
    assert!(stack.lines().iter().any(|l| l == "Apply complete! Resources: 6 added, 0 changed, 0 destroyed."));
    assert_eq!(stack.warnings(), Vec::<WarningCode>::new());
}

/// Answers `apply` from `applies` in turn, everything else by `other`.
fn sequenced(applies: Vec<Reply>, other: impl Fn(&Made) -> Option<Reply> + Send + Sync + 'static) -> FakeProcesses {
    let n = AtomicUsize::new(0);
    FakeProcesses::model(move |call| {
        if let Some(reply) = other(call) {
            return reply;
        }
        let verb = call.args.first().map(String::as_str);
        assert!(matches!(verb, Some("apply" | "destroy")), "unscripted call `{}`", call.line());
        applies.get(n.fetch_add(1, Ordering::SeqCst)).cloned().expect("no more applies scripted")
    })
}

#[tokio::test]
async fn a_lock_left_on_a_backends_state_is_released_once_and_the_apply_made_again() {
    let stack = stack().with_backend("s3");
    let fake = sequenced(vec![failing(LOCKED), Reply::ok(APPLY)], |call| match call.args.first().map(String::as_str) {
        Some("init" | "force-unlock") => Some(Reply::ok("")),
        _ => None,
    });
    with_cx(&stack, &fake, async |cx| Terraform.up(cx).await).await.expect("up");
    assert_eq!(
        made(&fake, &stack),
        [
            "terraform init -input=false -no-color",
            "terraform apply -auto-approve -input=false -json",
            "terraform force-unlock -force ee74a1fa-ed2e-e1a4-161e-513a943007dd",
            "terraform apply -auto-approve -input=false -json",
        ]
    );
    assert_eq!(stack.warnings(), [WarningCode::SharedState, WarningCode::Retrying]);
}

#[tokio::test]
async fn a_local_states_lock_is_a_live_processes_and_is_never_forced() {
    let stack = stack();
    let fake = FakeProcesses::script()
        .expect("terraform init", Reply::ok(""))
        .expect_times("terraform apply", 1, failing(LOCKED))
        .build();
    let e = with_cx(&stack, &fake, async |cx| Terraform.up(cx).await).await.expect_err("locked");
    assert_eq!(e.code(), ErrorCode::ToolFailed);
    assert!(e.to_string().contains("another process holds the state's lock"), "{e}");
}

#[tokio::test]
async fn down_drops_what_a_provider_says_is_gone_destroys_again_then_forgets_the_runs_state() {
    let stack = stack().with_state();
    std::fs::write(stack.state.path().join(BACKUP_FILE), "{}").expect("a backup");
    let held = show_of(&[("terraform_data", "ipset", "98d6", false)]);
    let fake = sequenced(vec![failing(GONE), Reply::ok(DESTROYED_ONE)], move |call| {
        match call.args.first().map(String::as_str) {
            Some("init" | "state") => Some(Reply::ok("")),
            Some("show") => Some(Reply::ok(&held)),
            _ => None,
        }
    });
    with_cx(&stack, &fake, async |cx| Terraform.down(cx).await).await.expect("down");
    assert_eq!(
        made(&fake, &stack),
        [
            "terraform init -input=false -no-color",
            "terraform destroy -auto-approve -input=false -json -state=<state>/terraform.tfstate",
            "terraform show -json -no-color <state>/terraform.tfstate",
            "terraform state rm -state=<state>/terraform.tfstate terraform_data.ipset",
            "terraform destroy -auto-approve -input=false -json -state=<state>/terraform.tfstate",
        ]
    );
    assert_eq!(stack.warnings(), [WarningCode::AlreadyGone]);
    for gone in [STATE_FILE, BACKUP_FILE, DATA_DIR] {
        assert!(!stack.state.path().join(gone).exists(), "{gone} is forgotten");
    }
}

#[tokio::test]
async fn a_destroy_failing_for_another_reason_fails_and_keeps_the_state() {
    let stack = stack().with_state();
    let held = show_of(&[("terraform_data", "ipset", "98d6", false)]);
    let fake = FakeProcesses::script()
        .expect("terraform init", Reply::ok(""))
        .expect_times("terraform destroy", 1, failing(PROVISIONER_FAILED))
        .expect("terraform show -json", Reply::ok(&held))
        .build();
    let e = with_cx(&stack, &fake, async |cx| Terraform.down(cx).await)
        .await
        .expect_err("a provisioner's failure");
    assert_eq!(e.code(), ErrorCode::ToolFailed);
    assert!(e.to_string().contains("local-exec provisioner error (terraform_data.ipset)"), "{e}");
    assert!(stack.state_file().exists(), "what still exists keeps its state");
    assert_eq!(stack.warnings(), Vec::<WarningCode>::new());
}

#[tokio::test]
async fn observe_reads_the_vms_and_the_outputs_sensitivity_kept() {
    let stack = stack().with_state();
    let held = show_of(&[
        ("esxi_guest", "web", "42", false),
        ("esxi_guest", "db", "43", true),
        ("esxi_vswitch", "stack", "vs", false),
    ]);
    let fake = FakeProcesses::script()
        .expect("terraform show -json -no-color", Reply::ok(&held))
        .expect(&format!("terraform output -json -no-color {}", stack.state_flag()), Reply::ok(OUTPUT))
        .build();
    let (seen, state) = with_cx(&stack, &fake, async |cx| Terraform.observe(cx).await).await.expect("observed");
    let machines: Vec<(&str, &MachineState)> = seen.machines.iter().map(|m| (m.name.as_str(), &m.state)).collect();
    assert_eq!(machines, [("web", &MachineState::Running), ("db", &MachineState::Unhealthy)]);
    assert_eq!(state, RunState::Failed, "a tainted instance");
    assert_eq!(
        seen.outputs["ip"],
        Output {
            value: "10.0.0.5".into(),
            sensitive: false
        }
    );
    assert_eq!(seen.outputs["machines"].value, r#"{"web":"10.0.0.10"}"#);
    let addresses: Vec<Option<&str>> = seen.machines.iter().map(|m| m.address.as_deref()).collect();
    assert_eq!(addresses, [Some("10.0.0.10"), None], "web at the address its `machines` output names");
    assert!(seen.outputs["token"].sensitive);
    assert!(stack.lines().iter().all(|l| !l.contains(SECRET)));
}

/// A VM the provider destroyed outside the run (deleted in the cloud's console) stays in the state
/// until a refresh: a status reads the state as it stands, with no refresh, no plan and no call of
/// the provider (a status is read often, and a refresh is the provider's API under the state's
/// lock); the next `up` or `update`, whose apply refreshes, makes it again.
#[tokio::test]
async fn a_vm_destroyed_outside_reads_as_the_state_holds_it_with_no_refresh_until_an_apply() {
    let stack = stack().with_state();
    let held = show_of(&[("aws_instance", "web", "i-gone", false)]);
    let fake = FakeProcesses::script()
        .expect("terraform show -json -no-color", Reply::ok(&held))
        .expect(&format!("terraform output -json -no-color {}", stack.state_flag()), Reply::ok("{}"))
        .build();
    let (seen, state) = with_cx(&stack, &fake, async |cx| Terraform.observe(cx).await).await.expect("observed");
    let machines: Vec<(&str, &MachineState)> = seen.machines.iter().map(|m| (m.name.as_str(), &m.state)).collect();
    assert_eq!((machines.as_slice(), state), ([("web", &MachineState::Running)].as_slice(), RunState::Ready));
    let programs: Vec<String> = fake.calls().iter().map(|c| c.args.first().cloned().unwrap_or_default()).collect();
    assert_eq!(programs, ["show", "output"], "a status reads the state only");
}

#[tokio::test]
async fn a_run_with_no_state_yet_is_absent_and_nothing_runs() {
    let stack = stack();
    let fake = FakeProcesses::script().build();
    let (seen, state) = with_cx(&stack, &fake, async |cx| Terraform.observe(cx).await).await.expect("observed");
    assert_eq!(state, RunState::Absent);
    assert!(seen.machines.is_empty() && seen.outputs.is_empty());
    assert_eq!(
        with_cx(&stack, &fake, async |cx| Terraform.found(cx).await).await.expect("found"),
        Vec::<Found>::new()
    );
}

#[test]
fn a_state_of_instances_none_tainted_is_a_ready_run_and_an_empty_one_absent() {
    let ready = tf::shown(&show_of(&[("aws_instance", "vm", "i-1", false)])).expect("a state");
    assert_eq!(run_state(&ready), RunState::Ready);
    assert_eq!(run_state(&tf::shown(r#"{"format_version":"1.0"}"#).expect("no state")), RunState::Absent);
}

/// A module that only reads (its state holds data sources and outputs, no managed instance) is
/// up once applied, has no machine, makes nothing to remove, and gives its outputs back.
#[tokio::test]
async fn a_state_of_data_sources_only_is_a_ready_run_with_its_outputs_and_nothing_to_remove() {
    let stack = stack().with_state();
    let fake = FakeProcesses::script()
        .expect_times("terraform show -json -no-color", 2, Reply::ok(DATA_ONLY))
        .expect(
            &format!("terraform output -json -no-color {}", stack.state_flag()),
            Reply::ok(r#"{"account_id":{"sensitive":false,"type":"string","value":"123456789012"}}"#),
        )
        .build();
    let (seen, state) = with_cx(&stack, &fake, async |cx| Terraform.observe(cx).await).await.expect("observed");
    assert_eq!((seen.machines.len(), state), (0, RunState::Ready));
    assert_eq!(seen.outputs["account_id"].value, "123456789012");
    let found = with_cx(&stack, &fake, async |cx| Terraform.found(cx).await).await.expect("found");
    assert_eq!(found, Vec::<Found>::new(), "a data source is no resource");
}

#[tokio::test]
async fn found_is_every_managed_instance_by_address_and_id() {
    let stack = stack().with_state();
    let fake = FakeProcesses::script().expect("terraform show -json", Reply::ok(SHOW)).build();
    let found = with_cx(&stack, &fake, async |cx| Terraform.found(cx).await).await.expect("found");
    assert_eq!(found.len(), 6, "data sources are no resources: {found:?}");
    assert_eq!(
        found[4],
        Found {
            key: "module.stack.terraform_data.vm[\"a\"]".into(),
            kind: ResourceKind::Module,
            native_id: "e19f0c3c-3c9c-e4c2-659d-b357c4b36370".into(),
            lifetime: Lifetime::Run,
        }
    );
    let vm = tf::shown(&show_of(&[("aws_instance", "vm", "i-1", false)])).expect("a state");
    assert!(machines::is_vm(&vm.managed[0].kind), "a VM's row is a VM");
}

#[tokio::test]
async fn adopt_moves_a_deployments_state_in_and_refuses_a_second() {
    let first = stack();
    let from = tempfile::tempdir().expect("a deployment");
    std::fs::write(from.path().join(STATE_FILE), "{\"version\":4}").expect("its state");
    std::fs::write(from.path().join(BACKUP_FILE), "{}").expect("its backup");
    let fake = FakeProcesses::script().build();
    with_cx(&first, &fake, async |cx| Terraform::adopt(cx, from.path())).await.expect("adopted");
    assert_eq!(std::fs::read_to_string(first.state_file()).expect("moved"), "{\"version\":4}");
    assert!(!from.path().join(STATE_FILE).exists() && first.state.path().join(BACKUP_FILE).exists());
    std::fs::write(from.path().join(STATE_FILE), "{}").expect("another state");
    let again = with_cx(&first, &fake, async |cx| Terraform::adopt(cx, from.path()))
        .await
        .expect_err("a state already");
    assert_eq!(again.code(), ErrorCode::AdoptRefused);
    let empty = tempfile::tempdir().expect("no deployment");
    let other = stack();
    let none = with_cx(&other, &fake, async |cx| Terraform::adopt(cx, empty.path()))
        .await
        .expect_err("no state");
    assert_eq!(none.code(), ErrorCode::AdoptRefused);
}

/// An adoption that moved the state but can't move its backup (a folder holds the backup's place
/// in the run's state folder) puts the state back: the deployment is left as it was found.
#[tokio::test]
async fn an_adoption_that_cant_move_every_file_puts_back_what_it_moved() {
    let stack = stack();
    let from = tempfile::tempdir().expect("a deployment");
    std::fs::write(from.path().join(STATE_FILE), "{\"version\":4}").expect("its state");
    std::fs::write(from.path().join(BACKUP_FILE), "{}").expect("its backup");
    let in_the_way = stack.state.path().join(BACKUP_FILE);
    std::fs::create_dir(&in_the_way).expect("a folder in the backup's place");
    std::fs::write(in_the_way.join("kept"), "").expect("a folder that isn't empty");
    let fake = FakeProcesses::script().build();
    let e = with_cx(&stack, &fake, async |cx| Terraform::adopt(cx, from.path()))
        .await
        .expect_err("the backup can't move");
    assert_eq!(e.code(), ErrorCode::Io, "{e}");
    assert_eq!(std::fs::read_to_string(from.path().join(STATE_FILE)).expect("put back"), "{\"version\":4}");
    assert!(!stack.state_file().exists(), "nothing adopted");
}
