use std::sync::Arc;

use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::testing::{answered_by, shared};

fn fixture(name: &str) -> String {
    match name {
        "sts-identity" => include_str!("../fixtures/aws/2.31-sts-identity.txt"),
        "sts-invalid-token" => include_str!("../fixtures/aws/2.31-sts-invalid-token.txt"),
        "sts-no-credentials" => include_str!("../fixtures/aws/2.31-sts-no-credentials.txt"),
        "sts-sso-expired" => include_str!("../fixtures/aws/2.31-sts-sso-expired.txt"),
        "sts-expired-token" => include_str!("../fixtures/aws/2.31-sts-expired-token.txt"),
        "list-profiles" => include_str!("../fixtures/aws/2.31-list-profiles.txt"),
        "ce-amount" => include_str!("../fixtures/aws/2.31-ce-amount.txt"),
        "ce-not-enabled" => include_str!("../fixtures/aws/2.31-ce-not-enabled.txt"),
        "ssm-latest-image" => include_str!("../fixtures/aws/2.31-ssm-latest-image.txt"),
        "ec2-dry-run-succeeded" => include_str!("../fixtures/aws/2.31-ec2-dry-run-succeeded.txt"),
        "ec2-unauthorized" => include_str!("../fixtures/aws/2.31-ec2-unauthorized.txt"),
        "ec2-image-not-found" => include_str!("../fixtures/aws/2.31-ec2-image-not-found.txt"),
        "ec2-no-default-vpc" => include_str!("../fixtures/aws/2.31-ec2-no-default-vpc.txt"),
        "ec2-throttled" => include_str!("../fixtures/aws/2.31-ec2-throttled.txt"),
        "ec2-image-malformed" => include_str!("../fixtures/aws/2.31-ec2-image-malformed.txt"),
        other => panic!("no fixture {other}"),
    }
    .to_owned()
}

fn cli_account(profile: Option<&str>) -> Account {
    Account::new(
        Credentials::Cli {
            profile: profile.map(str::to_owned),
        },
        Some("eu-west-3".into()),
    )
}

fn keys_account() -> Account {
    Account::new(
        Credentials::Keys {
            id: "AKIAIOSFODNN7EXAMPLE".into(),
            secret: "s3cret".into(),
        },
        Some("eu-west-3".into()),
    )
}

#[test]
fn a_call_runs_on_the_host_cli_or_in_the_clis_container_with_the_key_in_its_environment() {
    let (program, args, env) = cli_account(Some("work")).invocation(&["sts", "get-caller-identity"]);
    assert_eq!((program, args), ("aws", vec!["sts".to_owned(), "get-caller-identity".to_owned()]));
    assert_eq!(
        env,
        [("AWS_REGION".to_owned(), "eu-west-3".to_owned()), ("AWS_PROFILE".to_owned(), "work".to_owned())]
    );
    let (docker, in_container, container_env) = keys_account().invocation(&["sts", "get-caller-identity"]);
    assert_eq!(docker, "docker");
    assert_eq!(in_container[..2], ["run", "--rm"]);
    assert!(in_container.contains(&CLI_IMAGE.to_owned()));
    assert_eq!(in_container[in_container.len() - 2..], ["sts", "get-caller-identity"]);
    // The key is in the environment the container is handed, never an argument.
    assert!(!in_container.iter().any(|a| a.contains("s3cret")));
    assert!(container_env.contains(&("AWS_SECRET_ACCESS_KEY".to_owned(), "s3cret".to_owned())));
    assert!(!format!("{:?}", keys_account()).contains("s3cret"), "the secret is masked");
}

#[test]
fn each_refusal_reads_as_aws_says_it() {
    for (case, refusal) in [
        ("sts-invalid-token", Refusal::CredentialsExpired),
        ("sts-no-credentials", Refusal::CredentialsExpired),
        ("sts-sso-expired", Refusal::CredentialsExpired),
        ("sts-expired-token", Refusal::CredentialsExpired),
        ("ce-not-enabled", Refusal::NotEnabled),
        ("ec2-unauthorized", Refusal::Denied),
        ("ec2-throttled", Refusal::Other),
    ] {
        assert_eq!(super::refusal(&fixture(case)), refusal, "{case}");
    }
}

#[tokio::test(start_paused = true)]
async fn the_identity_is_the_arn_and_an_expired_sign_in_is_said_so() {
    let tools = FakeProcesses::script()
        .expect("aws sts get-caller-identity --query Arn --output text", Reply::ok(&fixture("sts-identity")))
        .build();
    let cx = answered_by(tools);
    assert_eq!(identity(&cx, &cli_account(None)).await.unwrap(), "arn:aws:iam::123456789012:user/ada");
    let refused = answered_by(
        FakeProcesses::script()
            .expect("docker run --rm", Reply::fail(254, &fixture("sts-expired-token")))
            .build(),
    );
    let failure = identity(&refused, &keys_account()).await.unwrap_err();
    assert_eq!(failure.refusal(), Some(Refusal::CredentialsExpired));
    assert_eq!(failure.code(), isothrum_runner::ErrorCode::CredentialsExpired);
    assert!(failure.to_string().starts_with("An error occurred (ExpiredToken)"), "{failure}");
}

#[tokio::test(start_paused = true)]
async fn the_profiles_are_the_clis_lines_and_the_spend_a_number_of_this_month() {
    let tools = FakeProcesses::script()
        .expect("aws configure list-profiles", Reply::ok(&fixture("list-profiles")))
        .expect("aws ce get-cost-and-usage --time-period", Reply::ok(&fixture("ce-amount")))
        .build();
    let cx = answered_by(tools);
    assert_eq!(profiles(&cx).await.unwrap(), ["default", "work"]);
    let spent = month_to_date_cost(&cx, &cli_account(Some("work"))).await.unwrap();
    assert!((spent - 12.345_678_9).abs() < 1e-9, "{spent}");
}

#[tokio::test(start_paused = true)]
async fn a_spend_that_is_not_a_finite_number_is_unreadable_and_cost_explorer_off_is_said() {
    for answer in ["", "None", "NaN", "inf", "12,5"] {
        let cx = answered_by(FakeProcesses::script().expect("aws ce", Reply::ok(answer)).build());
        let failure = month_to_date_cost(&cx, &cli_account(None)).await.unwrap_err();
        assert!(matches!(failure, Failure::Unreadable { .. }), "{answer:?}: {failure:?}");
    }
    let cx = answered_by(FakeProcesses::script().expect("aws ce", Reply::fail(254, &fixture("ce-not-enabled"))).build());
    assert_eq!(
        month_to_date_cost(&cx, &cli_account(None)).await.unwrap_err().refusal(),
        Some(Refusal::NotEnabled)
    );
}

#[test]
fn the_period_is_this_month_to_tomorrow_across_year_ends() {
    let at = |secs: u64| month_period(std::time::UNIX_EPOCH + Duration::from_secs(secs));
    // 2025-11-17T12:00:00Z.
    assert_eq!(at(1_763_380_800), "Start=2025-11-01,End=2025-11-18");
    // 2024-12-31T23:00:00Z: tomorrow is in the next year.
    assert_eq!(at(1_735_686_000), "Start=2024-12-01,End=2025-01-01");
    // 2024-02-29: a leap day.
    assert_eq!(at(1_709_208_000), "Start=2024-02-01,End=2024-03-01");
}

/// The launch permission when SSM answers `latest` and the dry run `dry`, and the dry run made.
async fn permission_with(latest: Reply, dry: Reply) -> (LaunchPermission, isothrum_runner::fake::Call) {
    let tools = Arc::new(
        FakeProcesses::script()
            .expect("aws ssm get-parameter", latest)
            .expect("aws ec2 run-instances", dry)
            .build(),
    );
    let said = launch_permission(&shared(Arc::<FakeProcesses>::clone(&tools)), &cli_account(None)).await;
    (said, tools.last("run-instances").unwrap())
}

/// The launch permission when the region's image is `ami-1` and the dry run fails saying `said`.
async fn dry_run_refused(said: &str) -> LaunchPermission {
    permission_with(Reply::ok("ami-1"), Reply::fail(254, said)).await.0
}

#[tokio::test(start_paused = true)]
async fn the_dry_run_uses_the_regions_latest_image_and_reads_what_aws_said_of_the_permission() {
    let latest = || Reply::ok(&fixture("ssm-latest-image"));
    let (allowed, dry) = permission_with(latest(), Reply::fail(254, &fixture("ec2-dry-run-succeeded"))).await;
    assert_eq!(allowed, LaunchPermission::Allowed);
    assert_eq!(
        dry.line(),
        "aws ec2 run-instances --dry-run --instance-type t3.micro --image-id ami-07dcdf1066ae9dad3 --output text"
    );
    assert_eq!(dry.env("AWS_REGION"), Some("eu-west-3"));
    let (denied, _) = permission_with(latest(), Reply::fail(254, &fixture("ec2-unauthorized"))).await;
    assert_eq!(denied, LaunchPermission::Denied);
}

#[tokio::test(start_paused = true)]
async fn an_error_after_authorization_allows_and_one_before_it_confirms_nothing() {
    let denied_ssm = Reply::fail(
        254,
        "An error occurred (AccessDeniedException) when calling the GetParameter operation: not authorized",
    );
    let (unknown_image, dry) = permission_with(denied_ssm, Reply::fail(254, &fixture("ec2-image-not-found"))).await;
    assert_eq!(
        unknown_image,
        LaunchPermission::AllowedThen {
            code: "InvalidAMIID.NotFound".into()
        }
    );
    assert!(dry.args.contains(&UNKNOWN_IMAGE.to_owned()), "the unknown well-formed image");
    assert_eq!(
        dry_run_refused(&fixture("ec2-no-default-vpc")).await,
        LaunchPermission::AllowedThen {
            code: "VPCIdNotSpecified".into()
        }
    );
    for case in ["ec2-throttled", "sts-expired-token", "ec2-image-malformed"] {
        let unconfirmed = dry_run_refused(&fixture(case)).await;
        assert!(
            matches!(&unconfirmed, LaunchPermission::Unconfirmed(Failure::Refused { said, .. }) if fixture(case).contains(said.as_str())),
            "{case}: {unconfirmed:?}"
        );
    }
    let offline = dry_run_refused("Could not connect to the endpoint URL: \"https://ec2.eu-west-3.amazonaws.com/\"").await;
    assert!(matches!(offline, LaunchPermission::Unconfirmed(_)), "no AWS code: the network");
    assert_eq!(permission_with(Reply::ok("ami-1"), Reply::ok("")).await.0, LaunchPermission::NoAnswer);
}

#[test]
fn the_probe_image_is_the_one_ssm_printed_else_the_unknown_well_formed_one() {
    assert_eq!(probe_image(Some("ami-07dcdf1066ae9dad3\n")), "ami-07dcdf1066ae9dad3");
    for other in [Some(""), Some("None\n"), Some("ami-1 ami-2"), Some("ami-"), None] {
        assert_eq!(probe_image(other), UNKNOWN_IMAGE, "{other:?}");
    }
    assert_eq!(
        error_code("x\nAn error occurred (InvalidAMIID.NotFound) when calling"),
        Some("InvalidAMIID.NotFound")
    );
    assert_eq!(error_code("An error occurred (broken"), None);
    assert!(before_authorization("RequestLimitExceeded") && before_authorization("InvalidAMIID.Malformed"));
    assert!(!before_authorization("InvalidAMIID.NotFound") && !before_authorization("VPCIdNotSpecified"));
}
