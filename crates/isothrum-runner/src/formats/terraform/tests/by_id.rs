//! A module's resource removed by its recorded id.

use super::*;

/// Removes the module's `aws_instance.vm`, recorded with id `i-1`.
async fn destroy_the_vm(lab: &Lab, fake: &FakeProcesses) -> Result<()> {
    with_cx(lab, fake, async |cx| {
        Terraform.destroy_by_id(cx, "aws_instance.vm", ResourceKind::Vm, "i-1").await
    })
    .await
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
    destroy_the_vm(&lab, &fake).await.expect("destroyed");
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
    destroy_the_vm(&lab, &fake).await.expect("destroyed");
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
