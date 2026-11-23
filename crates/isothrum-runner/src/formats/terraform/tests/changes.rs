//! The machines a module declares, its re-apply and update, and its inputs kept out of its lines.

use super::*;

#[tokio::test]
async fn machines_are_the_vms_the_state_keeps_and_those_the_plan_makes() {
    let stack = stack().with_state();
    let plan = [
        planned_line("esxi_guest", "attack", "create"),
        planned_line("esxi_guest", "old", "delete"),
        planned_line("terraform_data", "attack", "create"),
    ]
    .join("\n");
    let held = show_of(&[
        ("esxi_guest", "web", "1", false),
        ("esxi_guest", "old", "2", false),
        ("esxi_vswitch", "stack", "3", false),
    ]);
    let fake = FakeProcesses::script()
        .expect("terraform init", Reply::ok(""))
        .expect(&format!("terraform plan -input=false -json {}", stack.state_flag()), Reply::ok(&plan))
        .expect("terraform show -json", Reply::ok(&held))
        .build();
    let names = with_cx(&stack, &fake, async |cx| Terraform.machines(cx).await).await.expect("machines");
    assert_eq!(names, ["attack", "web"]);
}

#[tokio::test]
async fn reapply_plans_then_applies_and_writes_no_plan_file() {
    let stack = stack();
    let fake = FakeProcesses::script()
        .expect("terraform init", Reply::ok(""))
        .expect("terraform plan -input=false -json", Reply::ok(PLAN))
        .expect("terraform apply -auto-approve -input=false -json", Reply::ok(APPLY))
        .build();
    let change = Change {
        added: vec!["db".into()],
        ..Change::default()
    };
    with_cx(&stack, &fake, async |cx| Terraform.reapply(cx, &change).await)
        .await
        .expect("reapplied");
    let verbs: Vec<String> = fake.calls().iter().map(|c| c.args[0].clone()).collect();
    assert_eq!(verbs, ["init", "plan", "apply"]);
    isothrum_test_support::assert_every!(fake.calls(), |c: &Made| !c.args.iter().any(|a| a.starts_with("-out")));
    assert!(stack.lines().iter().any(|l| l == "terraform_data.db: Plan to create"));
}

#[tokio::test]
async fn update_is_the_same_apply() {
    let stack = stack();
    let fake = FakeProcesses::script()
        .expect("terraform init", Reply::ok(""))
        .expect("terraform apply -auto-approve", Reply::ok(APPLY))
        .build();
    with_cx(&stack, &fake, async |cx| Terraform.update(cx).await).await.expect("updated");
}

#[tokio::test]
async fn inputs_are_masked_in_the_lines_and_the_errors_short_ones_left() {
    let stack = stack();
    let said = format!("Error: invalid token {SECRET} for web");
    let fake = FakeProcesses::script()
        .expect("terraform init", Reply::ok(&said))
        .expect("terraform apply", Reply::fail(1, &said))
        .build();
    let e = with_cx(&stack, &fake, async |cx| Terraform.up(cx).await).await.expect_err("refused");
    assert!(!e.to_string().contains(SECRET) && e.to_string().contains("invalid token *** for web"), "{e}");
    assert!(stack.lines().iter().any(|l| l == "Error: invalid token *** for web"), "{:?}", stack.lines());
    isothrum_test_support::assert_every!(stack.lines(), |l: &String| !l.contains(SECRET));
}
