use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::ErrorCode;
use crate::RunName;
use crate::process::fake::{Call as Made, FakeProcesses, Reply};
use crate::project::{Format, Project};

const SHOW: &str = include_str!("../../tool_output/fixtures/terraform/1.16-show.json");
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
struct Lab {
    module: tempfile::TempDir,
    state: tempfile::TempDir,
    project: Project,
    run: RunName,
    lines: Mutex<Vec<String>>,
    warnings: Mutex<Vec<(WarningCode, String)>>,
}

fn lab() -> Lab {
    let module = tempfile::tempdir().expect("a folder");
    std::fs::write(module.path().join("main.tf"), "resource \"terraform_data\" \"web\" {}\n").expect("a module");
    let project = Project::open(module.path(), Some(Format::Terraform)).expect("a project");
    Lab {
        module,
        state: tempfile::tempdir().expect("a state folder"),
        project,
        run: RunName::new("lab").expect("a name"),
        lines: Mutex::new(Vec::new()),
        warnings: Mutex::new(Vec::new()),
    }
}

impl Lab {
    fn state_file(&self) -> PathBuf {
        self.state.path().join(STATE_FILE)
    }

    /// `-state=<the run's state file>`.
    fn state_flag(&self) -> String {
        format!("-state={}", self.state_file().display())
    }

    /// The run's state file, written (its content is the fake's: `show` reads it).
    fn with_state(self) -> Lab {
        std::fs::write(self.state_file(), "{}").expect("a state");
        self
    }

    /// `init` chose the backend `name`.
    fn with_backend(self, name: &str) -> Lab {
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

/// Runs `f` with a context for `lab` on `fake`.
async fn with_cx<T>(lab: &Lab, fake: &FakeProcesses, f: impl AsyncFnOnce(&ToolCx<'_>) -> T) -> T {
    let sink = |_: Stream, l: &str| lab.lines.lock().expect("lines").push(l.to_owned());
    let warn = |c: WarningCode, m: &str| lab.warnings.lock().expect("warnings").push((c, m.to_owned()));
    let cx = ToolCx {
        project: &lab.project,
        run: &lab.run,
        key: "k3y9ab",
        state_dir: lab.state.path(),
        provider: None,
        env: vec![
            ("name".to_owned(), "web".to_owned()),
            ("token".to_owned(), SECRET.to_owned()),
            ("TF_VAR_name".to_owned(), "web".to_owned()),
            ("TF_VAR_token".to_owned(), SECRET.to_owned()),
            ("ISOTHRUM_RUN_KEY".to_owned(), "k3y9ab".to_owned()),
        ],
        processes: fake,
        sink: &sink,
        warn: &warn,
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

/// The calls made, as `program arg…` lines with the run's state folder written `<state>`.
fn made(fake: &FakeProcesses, lab: &Lab) -> Vec<String> {
    fake.lines()
        .iter()
        .map(|l| l.replace(&lab.state.path().display().to_string(), "<state>"))
        .collect()
}

#[tokio::test]
async fn up_inits_then_applies_on_the_runs_own_state_with_inputs_in_the_environment_only() {
    let lab = lab();
    std::fs::write(lab.state.path().join(LOCK_INFO), "{}").expect("a killed run's lock information");
    let fake = FakeProcesses::script()
        .expect(
            "terraform init -input=false -no-color",
            Reply::ok("Terraform has been successfully initialized!"),
        )
        .expect(
            &format!("terraform apply -auto-approve -input=false -json {}", lab.state_flag()),
            Reply::ok(APPLY),
        )
        .build();
    with_cx(&lab, &fake, async |cx| Terraform.up(cx).await).await.expect("up");
    let apply = fake.last("terraform apply").expect("an apply");
    let data = lab.state.path().join(DATA_DIR);
    assert_eq!(apply.env("TF_DATA_DIR"), Some(data.to_str().expect("a path")));
    assert_eq!(apply.env("TF_IN_AUTOMATION"), Some("1"));
    assert_eq!(apply.env("TF_VAR_token"), Some(SECRET));
    for call in fake.calls() {
        assert!(!call.args.iter().any(|a| a.contains(SECRET) || a.starts_with("-var")), "{call:?}");
    }
    assert!(!lab.state.path().join(LOCK_INFO).exists(), "the stale lock information is removed");
    assert!(
        std::fs::read_dir(lab.module.path()).expect("the module").count() == 1,
        "nothing written beside the module"
    );
    assert!(lab.lines().iter().any(|l| l == "Apply complete! Resources: 6 added, 0 changed, 0 destroyed."));
    assert_eq!(lab.warnings(), Vec::<WarningCode>::new());
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
    let lab = lab().with_backend("s3");
    let fake = sequenced(vec![failing(LOCKED), Reply::ok(APPLY)], |call| match call.args.first().map(String::as_str) {
        Some("init" | "force-unlock") => Some(Reply::ok("")),
        _ => None,
    });
    with_cx(&lab, &fake, async |cx| Terraform.up(cx).await).await.expect("up");
    assert_eq!(
        made(&fake, &lab),
        [
            "terraform init -input=false -no-color",
            "terraform apply -auto-approve -input=false -json",
            "terraform force-unlock -force ee74a1fa-ed2e-e1a4-161e-513a943007dd",
            "terraform apply -auto-approve -input=false -json",
        ]
    );
    assert_eq!(lab.warnings(), [WarningCode::SharedState, WarningCode::Retrying]);
}

#[tokio::test]
async fn a_local_states_lock_is_a_live_processs_and_is_never_forced() {
    let lab = lab();
    let fake = FakeProcesses::script()
        .expect("terraform init", Reply::ok(""))
        .expect_times("terraform apply", 1, failing(LOCKED))
        .build();
    let e = with_cx(&lab, &fake, async |cx| Terraform.up(cx).await).await.expect_err("locked");
    assert_eq!(e.code(), ErrorCode::ToolFailed);
    assert!(e.to_string().contains("another process holds the state's lock"), "{e}");
}

#[tokio::test]
async fn down_drops_what_a_provider_says_is_gone_destroys_again_then_forgets_the_runs_state() {
    let lab = lab().with_state();
    std::fs::write(lab.state.path().join(BACKUP_FILE), "{}").expect("a backup");
    let held = show_of(&[("terraform_data", "ipset", "98d6", false)]);
    let fake = sequenced(vec![failing(GONE), Reply::ok(DESTROYED_ONE)], move |call| {
        match call.args.first().map(String::as_str) {
            Some("init" | "state") => Some(Reply::ok("")),
            Some("show") => Some(Reply::ok(&held)),
            _ => None,
        }
    });
    with_cx(&lab, &fake, async |cx| Terraform.down(cx).await).await.expect("down");
    assert_eq!(
        made(&fake, &lab),
        [
            "terraform init -input=false -no-color",
            "terraform destroy -auto-approve -input=false -json -state=<state>/terraform.tfstate",
            "terraform show -json -no-color <state>/terraform.tfstate",
            "terraform state rm -state=<state>/terraform.tfstate terraform_data.ipset",
            "terraform destroy -auto-approve -input=false -json -state=<state>/terraform.tfstate",
        ]
    );
    assert_eq!(lab.warnings(), [WarningCode::AlreadyGone]);
    for gone in [STATE_FILE, BACKUP_FILE, DATA_DIR] {
        assert!(!lab.state.path().join(gone).exists(), "{gone} is forgotten");
    }
}

#[tokio::test]
async fn a_destroy_failing_for_another_reason_fails_and_keeps_the_state() {
    let lab = lab().with_state();
    let held = show_of(&[("terraform_data", "ipset", "98d6", false)]);
    let fake = FakeProcesses::script()
        .expect("terraform init", Reply::ok(""))
        .expect_times("terraform destroy", 1, failing(PROVISIONER_FAILED))
        .expect("terraform show -json", Reply::ok(&held))
        .build();
    let e = with_cx(&lab, &fake, async |cx| Terraform.down(cx).await)
        .await
        .expect_err("a provisioner's failure");
    assert_eq!(e.code(), ErrorCode::ToolFailed);
    assert!(e.to_string().contains("local-exec provisioner error (terraform_data.ipset)"), "{e}");
    assert!(lab.state_file().exists(), "what still exists keeps its state");
    assert_eq!(lab.warnings(), Vec::<WarningCode>::new());
}

#[tokio::test]
async fn observe_reads_the_vms_and_the_outputs_sensitivity_kept() {
    let lab = lab().with_state();
    let held = show_of(&[
        ("esxi_guest", "web", "42", false),
        ("esxi_guest", "db", "43", true),
        ("esxi_vswitch", "lab", "vs", false),
    ]);
    let fake = FakeProcesses::script()
        .expect("terraform show -json -no-color", Reply::ok(&held))
        .expect(&format!("terraform output -json -no-color {}", lab.state_flag()), Reply::ok(OUTPUT))
        .build();
    let (seen, state) = with_cx(&lab, &fake, async |cx| Terraform.observe(cx).await).await.expect("observed");
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
    assert!(seen.outputs["token"].sensitive);
    assert!(lab.lines().iter().all(|l| !l.contains(SECRET)));
}

#[tokio::test]
async fn a_run_with_no_state_yet_is_absent_and_nothing_runs() {
    let lab = lab();
    let fake = FakeProcesses::script().build();
    let (seen, state) = with_cx(&lab, &fake, async |cx| Terraform.observe(cx).await).await.expect("observed");
    assert_eq!(state, RunState::Absent);
    assert!(seen.machines.is_empty() && seen.outputs.is_empty());
    assert_eq!(
        with_cx(&lab, &fake, async |cx| Terraform.found(cx).await).await.expect("found"),
        Vec::<Found>::new()
    );
}

#[test]
fn every_instance_ready_is_a_ready_run() {
    let ready = tf::shown(&show_of(&[("aws_instance", "vm", "i-1", false)])).expect("a state");
    assert_eq!(run_state(&ready.managed), RunState::Ready);
}

#[tokio::test]
async fn found_is_every_managed_instance_by_address_and_id() {
    let lab = lab().with_state();
    let fake = FakeProcesses::script().expect("terraform show -json", Reply::ok(SHOW)).build();
    let found = with_cx(&lab, &fake, async |cx| Terraform.found(cx).await).await.expect("found");
    assert_eq!(found.len(), 6, "data sources are no resources: {found:?}");
    assert_eq!(
        found[4],
        Found {
            key: "module.lab.terraform_data.vm[\"a\"]".into(),
            kind: ResourceKind::Module,
            native_id: "e19f0c3c-3c9c-e4c2-659d-b357c4b36370".into(),
            lifetime: Lifetime::Run,
        }
    );
    let vm = tf::shown(&show_of(&[("aws_instance", "vm", "i-1", false)])).expect("a state");
    assert!(machines::is_vm(&vm.managed[0].kind), "a VM's row is a VM");
}

#[tokio::test]
async fn destroy_by_id_targets_an_instance_the_state_holds() {
    let lab = lab().with_state();
    let fake = FakeProcesses::script()
        .expect("terraform init", Reply::ok(""))
        .expect("terraform show -json", Reply::ok(&show_of(&[("aws_instance", "vm", "i-1", false)])))
        .expect(
            &format!(
                "terraform destroy -auto-approve -input=false -json {} -target=aws_instance.vm",
                lab.state_flag()
            ),
            Reply::ok(DESTROYED_ONE),
        )
        .build();
    with_cx(&lab, &fake, async |cx| {
        Terraform.destroy_by_id(cx, "aws_instance.vm", ResourceKind::Vm, "i-1").await
    })
    .await
    .expect("destroyed");
}

#[tokio::test]
async fn destroy_by_id_imports_an_instance_the_state_lost_first() {
    let lab = lab();
    let fake = FakeProcesses::script()
        .expect("terraform init", Reply::ok(""))
        .expect(
            &format!("terraform import -input=false -no-color {} aws_instance.vm i-1", lab.state_flag()),
            Reply::ok(""),
        )
        .expect("terraform destroy -auto-approve -input=false -json", Reply::ok(DESTROYED_ONE))
        .build();
    with_cx(&lab, &fake, async |cx| {
        Terraform.destroy_by_id(cx, "aws_instance.vm", ResourceKind::Vm, "i-1").await
    })
    .await
    .expect("destroyed");
}

#[tokio::test]
async fn destroy_by_id_of_an_object_already_gone_is_done() {
    let lab = lab();
    let fake = FakeProcesses::script()
        .expect("terraform init", Reply::ok(""))
        .expect("terraform import", Reply::fail(1, "Error: Cannot import non-existent remote object"))
        .build();
    with_cx(&lab, &fake, async |cx| {
        Terraform.destroy_by_id(cx, "aws_instance.vm", ResourceKind::Vm, "i-1").await
    })
    .await
    .expect("nothing there is fine");
}

#[tokio::test]
async fn destroy_by_id_of_an_object_the_state_replaced_is_refused() {
    let lab = lab().with_state();
    let fake = FakeProcesses::script()
        .expect("terraform init", Reply::ok(""))
        .expect("terraform show -json", Reply::ok(&show_of(&[("aws_instance", "vm", "i-2", false)])))
        .build();
    let e = with_cx(&lab, &fake, async |cx| {
        Terraform.destroy_by_id(cx, "aws_instance.vm", ResourceKind::Vm, "i-1").await
    })
    .await
    .expect_err("another object");
    assert_eq!(e.code(), ErrorCode::Unsupported);
}

#[tokio::test]
async fn machines_are_the_vms_the_state_keeps_and_those_the_plan_makes() {
    let lab = lab().with_state();
    let plan = [
        planned_line("esxi_guest", "attack", "create"),
        planned_line("esxi_guest", "old", "delete"),
        planned_line("terraform_data", "attack", "create"),
    ]
    .join("\n");
    let held = show_of(&[
        ("esxi_guest", "web", "1", false),
        ("esxi_guest", "old", "2", false),
        ("esxi_vswitch", "lab", "3", false),
    ]);
    let fake = FakeProcesses::script()
        .expect("terraform init", Reply::ok(""))
        .expect(&format!("terraform plan -input=false -json {}", lab.state_flag()), Reply::ok(&plan))
        .expect("terraform show -json", Reply::ok(&held))
        .build();
    let names = with_cx(&lab, &fake, async |cx| Terraform.machines(cx).await).await.expect("machines");
    assert_eq!(names, ["attack", "web"]);
}

#[tokio::test]
async fn reapply_plans_then_applies_and_writes_no_plan_file() {
    let lab = lab();
    let fake = FakeProcesses::script()
        .expect("terraform init", Reply::ok(""))
        .expect("terraform plan -input=false -json", Reply::ok(PLAN))
        .expect("terraform apply -auto-approve -input=false -json", Reply::ok(APPLY))
        .build();
    let change = Change {
        added: vec!["db".into()],
        ..Change::default()
    };
    with_cx(&lab, &fake, async |cx| Terraform.reapply(cx, &change).await).await.expect("reapplied");
    let calls = made(&fake, &lab);
    assert_eq!(calls.len(), 3);
    assert!(calls[1].starts_with("terraform plan") && calls[2].starts_with("terraform apply"), "{calls:?}");
    assert!(calls.iter().all(|c| !c.contains("-out")), "{calls:?}");
    assert!(lab.lines().iter().any(|l| l == "terraform_data.db: Plan to create"));
}

#[tokio::test]
async fn update_is_the_same_apply() {
    let lab = lab();
    let fake = FakeProcesses::script()
        .expect("terraform init", Reply::ok(""))
        .expect("terraform apply -auto-approve", Reply::ok(APPLY))
        .build();
    with_cx(&lab, &fake, async |cx| Terraform.update(cx).await).await.expect("updated");
}

#[tokio::test]
async fn inputs_are_masked_in_the_lines_and_the_errors_short_ones_left() {
    let lab = lab();
    let said = format!("Error: invalid token {SECRET} for web");
    let fake = FakeProcesses::script()
        .expect("terraform init", Reply::ok(&said))
        .expect("terraform apply", Reply::fail(1, &said))
        .build();
    let e = with_cx(&lab, &fake, async |cx| Terraform.up(cx).await).await.expect_err("refused");
    assert!(!e.to_string().contains(SECRET) && e.to_string().contains("invalid token *** for web"), "{e}");
    assert!(lab.lines().iter().any(|l| l == "Error: invalid token *** for web"), "{:?}", lab.lines());
    assert!(lab.lines().iter().all(|l| !l.contains(SECRET)));
}

#[tokio::test]
async fn what_a_module_cant_do_is_refused_and_nothing_runs() {
    let lab = lab();
    let fake = FakeProcesses::script().build();
    let refusals = with_cx(&lab, &fake, async |cx| {
        vec![
            Terraform.park(cx, Park::Pause).await.err(),
            Terraform.resume(cx).await.err(),
            Terraform.provision(cx, None).await.err(),
            Terraform.attach(cx, "web").await.err(),
            Terraform.retire(cx, "web").await.err(),
            Terraform.exec(cx, "web", &["true".to_owned()]).await.err(),
            Terraform.console(cx, "web", None).err(),
        ]
    })
    .await;
    for e in refusals {
        assert_eq!(e.map(|e| e.code()), Some(ErrorCode::Unsupported));
    }
}

#[tokio::test]
async fn the_probe_reads_the_version() {
    let lab = lab();
    let fake = FakeProcesses::script()
        .expect("terraform version -json", Reply::ok(r#"{"terraform_version":"0.13.7"}"#))
        .build();
    let e = with_cx(&lab, &fake, async |cx| Terraform.probe(cx).await).await.expect_err("too old");
    assert_eq!(e.code(), ErrorCode::ToolTooOld);
}

#[tokio::test]
async fn adopt_moves_a_deployments_state_in_and_refuses_a_second() {
    let first = lab();
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
    let other = lab();
    let none = with_cx(&other, &fake, async |cx| Terraform::adopt(cx, empty.path()))
        .await
        .expect_err("no state");
    assert_eq!(none.code(), ErrorCode::AdoptRefused);
}
