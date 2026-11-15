//! The machines a module declares, its re-apply and update, and its inputs kept out of its lines.

use super::*;

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
    assert!(fake.calls().iter().all(|c| !c.args.iter().any(|a| a.starts_with("-out"))), "{calls:?}");
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
