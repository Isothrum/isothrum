use std::sync::{Arc, Mutex};

use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::testing::answered_by;

#[test]
fn each_cloud_signed_in_through_a_cli_names_it_and_a_token_cloud_none() {
    assert_eq!(cli(Cloud::Aws), Some("aws"));
    assert_eq!(cli(Cloud::Azure), Some("az"));
    assert_eq!(cli(Cloud::Gcp), Some("gcloud"));
    assert_eq!(cli(Cloud::Oci), Some("oci"));
    assert_eq!(cli(Cloud::Digitalocean), None);
    assert_eq!(cli(Cloud::Linode), None);
}

#[test]
fn each_browser_sign_in_runs_its_clis_own_command() {
    assert_eq!(SignIn::Azure.command().line(), "az login");
    // One flow for gcloud's own sign-in and the application-default one Terraform reads.
    assert_eq!(SignIn::Gcp.command().line(), "gcloud auth login --update-adc");
    assert_eq!(SignIn::Aws { profile: None }.command().line(), "aws login");
    let spaced = SignIn::Aws {
        profile: Some("my profile".into()),
    }
    .command();
    assert_eq!(spaced.args, ["login", "--profile", "my profile"], "one argument, however it is spelled");
}

/// The lines a sign-in handed on, and its end.
async fn signing_in(tools: FakeProcesses, which: SignIn) -> (Vec<String>, Result<(), Failure>) {
    let cx = answered_by(tools);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let keep = Arc::clone(&seen);
    let mut on_line = move |l: &str| keep.lock().unwrap().push(l.to_owned());
    let end = sign_in(&cx, &which, &mut on_line).await;
    let lines = seen.lock().unwrap().clone();
    (lines, end)
}

#[tokio::test(start_paused = true)]
async fn a_sign_in_hands_on_its_command_then_every_line_the_cli_prints() {
    let (lines, end) = signing_in(
        FakeProcesses::script()
            .expect("az login", Reply::ok("A web browser has been opened.\n[]"))
            .build(),
        SignIn::Azure,
    )
    .await;
    assert_eq!(lines, ["$ az login", "A web browser has been opened.", "[]"]);
    assert_eq!(end, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn a_failed_or_missing_sign_in_says_why() {
    let (_, end) = signing_in(
        FakeProcesses::script()
            .expect(
                "gcloud auth login",
                Reply::fail(1, "ERROR: (gcloud.auth.login) There was a problem with web authentication.\n"),
            )
            .build(),
        SignIn::Gcp,
    )
    .await;
    assert_eq!(
        end.unwrap_err().to_string(),
        "ERROR: (gcloud.auth.login) There was a problem with web authentication."
    );
    let (_, missing) = signing_in(
        FakeProcesses::script().expect("aws login", Reply::missing()).build(),
        SignIn::Aws { profile: None },
    )
    .await;
    assert_eq!(missing, Err(Failure::Missing { program: "aws".into() }));
}
