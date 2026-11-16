//! What a module is refused before Terraform runs, and what is only warned about.

use super::*;

#[tokio::test]
async fn an_up_of_a_module_whose_required_secret_has_no_value_is_refused_before_terraform_runs() {
    let stack = stack();
    std::fs::write(
        stack.module.path().join("main.tf"),
        "variable \"password\" {\n  sensitive = true\n}\nvariable \"token\" {}\n",
    )
    .expect("a module");
    let stack = Stack {
        project: Project::open(stack.module.path(), Some(Format::Terraform)).expect("a project"),
        ..stack
    };
    let nothing_runs = FakeProcesses::script().build();
    let e = with_cx(&stack, &nothing_runs, async |cx| Terraform.up(cx).await)
        .await
        .expect_err("no password");
    assert!(matches!(&e, Error::SecretMissing { name } if name == "password"), "{e:?}");
    assert_eq!(e.code(), ErrorCode::SecretMissing);
}

/// A Terraform whose version can't be read is used, with a warning, never refused (an owner's
/// decision: its minimum is then unchecked).
#[tokio::test]
async fn a_terraform_whose_version_cant_be_read_is_used_with_a_warning() {
    let stack = stack();
    let fake = FakeProcesses::script()
        .expect("terraform version -json", Reply::ok("a build of its own"))
        .build();
    with_cx(&stack, &fake, async |cx| Terraform.probe(cx).await).await.expect("used");
    assert_eq!(stack.warnings(), [WarningCode::VersionUnknown]);
    let read = FakeProcesses::script()
        .expect("terraform version -json", Reply::ok(r#"{"terraform_version": "1.16.0"}"#))
        .build();
    with_cx(&stack, &read, async |cx| Terraform.probe(cx).await).await.expect("used");
    assert_eq!(stack.warnings(), [WarningCode::VersionUnknown], "a version read: nothing more said");
}
