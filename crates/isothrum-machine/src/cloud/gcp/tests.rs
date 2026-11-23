use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::testing::answered_by;

#[test]
fn each_refusal_reads_as_gcloud_says_it() {
    for (said, expected) in [
        (
            include_str!("../fixtures/gcloud/502.0-adc-reauthentication-failed.txt"),
            Refusal::CredentialsExpired,
        ),
        (include_str!("../fixtures/gcloud/502.0-refresh-failed.txt"), Refusal::CredentialsExpired),
        // Not found, though it says "permission denied" too: Google's status is NOT_FOUND.
        (include_str!("../fixtures/gcloud/502.0-project-not-found.txt"), Refusal::NotFound),
        (include_str!("../fixtures/gcloud/502.0-project-no-permission.txt"), Refusal::Denied),
        (include_str!("../fixtures/gcloud/502.0-billing-api-disabled.txt"), Refusal::NotEnabled),
        (
            "ERROR: (gcloud.billing.projects.link) FAILED_PRECONDITION: Precondition check failed.",
            Refusal::QuotaExceeded,
        ),
        ("ERROR: something NOT_FOUNDATION", Refusal::Other),
    ] {
        assert_eq!(refusal(said), expected, "{said}");
    }
}

#[tokio::test(start_paused = true)]
async fn the_account_and_its_two_sign_ins_are_asked_apart() {
    let cx = answered_by(
        FakeProcesses::script()
            .expect(
                "gcloud auth list --filter=status:ACTIVE --format=value(account)",
                Reply::ok("ada@example.com\n"),
            )
            .expect("gcloud auth print-access-token --quiet", Reply::ok("ya29.token\n"))
            .expect(
                "gcloud auth application-default print-access-token",
                Reply::fail(1, include_str!("../fixtures/gcloud/502.0-adc-reauthentication-failed.txt")),
            )
            .build(),
    );
    assert_eq!(active_account(&cx).await.unwrap().as_deref(), Some("ada@example.com"));
    assert_eq!(access_token(&cx).await, Ok(()));
    let adc = signed_in(&cx).await.unwrap_err();
    assert_eq!(adc.refusal(), Some(Refusal::CredentialsExpired));
    assert!(adc.to_string().contains("Reauthentication failed."), "{adc}");
    let none = answered_by(FakeProcesses::script().expect("gcloud auth list", Reply::ok("\n")).build());
    assert_eq!(active_account(&none).await, Ok(None));
}

#[tokio::test(start_paused = true)]
async fn billing_accounts_and_organizations_drop_their_prefixes_and_nameless_entries() {
    let cx = answered_by(
        FakeProcesses::script()
            .expect(
                "gcloud billing accounts list --format json",
                Reply::ok(include_str!("../fixtures/gcloud/502.0-billing-accounts-list.json")),
            )
            .expect(
                "gcloud organizations list --format json",
                Reply::ok(include_str!("../fixtures/gcloud/502.0-organizations-list.json")),
            )
            .expect("gcloud billing accounts describe 0X0X0X-0X0X0X-0X0X0X", Reply::ok("My Billing Account\n"))
            .build(),
    );
    let accounts = billing_accounts(&cx).await.unwrap();
    assert_eq!(
        accounts,
        [
            BillingAccount {
                id: "0X0X0X-0X0X0X-0X0X0X".into(),
                name: "My Billing Account".into(),
                open: true
            },
            BillingAccount {
                id: "AAAAAA-BBBBBB-CCCCCC".into(),
                name: "Closed trial".into(),
                open: false
            }
        ]
    );
    assert_eq!(
        organizations(&cx).await.unwrap(),
        [Organization {
            id: "1234567890".into(),
            name: "example.com".into()
        }]
    );
    assert_eq!(billing_account_name(&cx, "0X0X0X-0X0X0X-0X0X0X").await.unwrap(), "My Billing Account");
    let loose = read_billing_accounts(
        &serde_json::from_str::<Vec<Value>>(r#"[{"name": "billingAccounts/A", "displayName": 7, "open": "true"}, {"name": "billingAccounts/"}, {}]"#).unwrap(),
    );
    assert_eq!(
        loose,
        [BillingAccount {
            id: "A".into(),
            name: String::new(),
            open: false
        }]
    );
    assert_eq!(read_organizations(&[serde_json::json!({})]), []);
}

#[tokio::test(start_paused = true)]
async fn a_list_gcloud_refused_or_answered_in_words_is_a_failure_never_none() {
    let cx = answered_by(
        FakeProcesses::script()
            .expect(
                "gcloud billing accounts list",
                Reply::fail(1, include_str!("../fixtures/gcloud/502.0-billing-api-disabled.txt")),
            )
            .expect("gcloud organizations list", Reply::ok("Listed 0 items."))
            .build(),
    );
    assert_eq!(billing_accounts(&cx).await.unwrap_err().refusal(), Some(Refusal::NotEnabled));
    assert!(matches!(organizations(&cx).await, Err(Failure::Unreadable { .. })));
}

#[tokio::test(start_paused = true)]
async fn a_project_is_gone_only_when_google_says_so_plainly() {
    for (answer, state) in [
        (Reply::ok("ACTIVE\n"), ProjectState::Active),
        (Reply::ok("DELETE_REQUESTED\n"), ProjectState::Gone),
        (Reply::ok("SUSPENDED\n"), ProjectState::Other("SUSPENDED".into())),
        (
            Reply::fail(1, include_str!("../fixtures/gcloud/502.0-project-not-found.txt")),
            ProjectState::Gone,
        ),
    ] {
        let cx = answered_by(
            FakeProcesses::script()
                .expect("gcloud projects describe example-1234 --format value(lifecycleState)", answer)
                .build(),
        );
        assert_eq!(project_state(&cx, "example-1234").await, Ok(state));
    }
    // Another account's "no permission (or it may not exist)", an expired sign-in: never gone.
    for said in [
        include_str!("../fixtures/gcloud/502.0-project-no-permission.txt"),
        include_str!("../fixtures/gcloud/502.0-refresh-failed.txt"),
    ] {
        let cx = answered_by(FakeProcesses::script().expect("gcloud projects describe", Reply::fail(1, said)).build());
        assert!(project_state(&cx, "example-1234").await.is_err(), "{said}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_project_is_created_linked_enabled_and_deleted_with_gclouds_own_commands() {
    let cx = answered_by(
        FakeProcesses::script()
            .expect("gcloud projects create example-1234 --name Example --organization=42", Reply::ok(""))
            .expect(
                "gcloud billing projects link example-1234 --billing-account 0X0X0X-0X0X0X-0X0X0X",
                Reply::ok(""),
            )
            .expect("gcloud services enable compute.googleapis.com --project example-1234", Reply::ok(""))
            .expect("gcloud projects delete example-1234 --quiet", Reply::ok(""))
            .build(),
    );
    assert_eq!(create_project(&cx, "example-1234", "Example", Some("42")).await, Ok(()));
    assert_eq!(link_billing(&cx, "example-1234", "0X0X0X-0X0X0X-0X0X0X").await, Ok(()));
    assert_eq!(enable_service(&cx, "example-1234", "compute.googleapis.com").await, Ok(()));
    assert_eq!(delete_project(&cx, "example-1234").await, Ok(()));
    assert_eq!(
        create_args("p", "Name with spaces", None),
        ["projects", "create", "p", "--name", "Name with spaces"]
    );
}

#[tokio::test(start_paused = true)]
async fn a_billing_account_with_no_free_project_slot_is_a_quota_exceeded() {
    let cx = answered_by(
        FakeProcesses::script()
            .expect(
                "gcloud billing projects link",
                Reply::fail(1, include_str!("../fixtures/gcloud/502.0-link-no-project-slot.txt")),
            )
            .build(),
    );
    let failure = link_billing(&cx, "example-1234", "0X0X0X-0X0X0X-0X0X0X").await.unwrap_err();
    assert_eq!(failure.refusal(), Some(Refusal::QuotaExceeded));
}
