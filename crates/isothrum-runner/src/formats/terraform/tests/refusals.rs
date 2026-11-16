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

/// A cloud refusing the sign-in its provider uses (run out, or never made) is
/// `credentials_expired`, naming the cloud by the provider the failure names, before anything is
/// applied: signing in again is the fix, not the module.
#[tokio::test]
async fn an_apply_a_cloud_refused_the_sign_in_of_is_credentials_expired_naming_the_cloud() {
    let stack = stack();
    let said = "Error: failed to refresh cached credentials, create oauth2 token: login session has expired, please reauthenticate\n  with provider[\"registry.terraform.io/hashicorp/aws\"],";
    let fake = FakeProcesses::script()
        .expect("terraform init", Reply::ok(""))
        .expect("terraform apply", Reply::fail(1, said))
        .build();
    let e = with_cx(&stack, &fake, async |cx| Terraform.up(cx).await).await.expect_err("refused");
    let report = e.report();
    assert_eq!((report.code, report.changed), (ErrorCode::CredentialsExpired, false), "{e}");
    assert!(
        matches!(report.detail.map(|d| d.data), Some(crate::Detail::CredentialsExpired { cloud: Some(crate::Cloud::Aws), reason }) if reason.contains("login session has expired")),
        "{e:?}"
    );
    // Any other failure is the step's.
    let other = FakeProcesses::script()
        .expect("terraform init", Reply::ok(""))
        .expect("terraform apply", Reply::fail(1, "Error: creating EC2 Instance: InsufficientInstanceCapacity"))
        .build();
    let failed = with_cx(&stack, &other, async |cx| Terraform.up(cx).await).await.expect_err("failed");
    assert_eq!(failed.code(), ErrorCode::ToolFailed, "{failed}");
}
