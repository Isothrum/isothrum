//! What a Terraform run reads of its state: its VMs and outputs, absent or ready, and what it holds by id.

use super::*;

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
