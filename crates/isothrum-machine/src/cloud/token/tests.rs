use isothrum_runner::fake_https::{self as https, Request};

use super::*;

/// A token check against the test's server, its certificate pinned.
async fn checked(status: u16) -> (Result<(), TokenError>, Vec<Request>) {
    let server = https::Server::start(move |_: &Request| (status, "{}".to_owned())).await.unwrap();
    let url = format!("{}/v2/account", server.endpoint);
    let outcome = account(&url, "tok-123", Trust::Pinned(Some(https::fingerprint()))).await;
    (outcome, server.requests())
}

#[tokio::test]
async fn a_token_the_account_endpoint_accepts_signs_in() {
    let (outcome, requests) = checked(200).await;
    assert_eq!(outcome, Ok(()));
    assert_eq!(requests.len(), 1);
    assert_eq!((requests[0].method.as_str(), requests[0].target.as_str()), ("GET", "/v2/account"));
    assert_eq!(requests[0].header("authorization"), Some("Bearer tok-123"));
}

#[tokio::test]
async fn a_refused_token_and_another_status_are_told_apart() {
    assert_eq!(checked(401).await.0, Err(TokenError::Rejected));
    assert_eq!(checked(403).await.0, Err(TokenError::Status(403)));
    assert_eq!(checked(503).await.0, Err(TokenError::Status(503)));
}

#[tokio::test]
async fn no_server_is_unreachable_and_an_unpinned_certificate_gets_no_token() {
    let refused = account("https://127.0.0.1:1/v2/account", "t", Trust::System).await;
    assert!(matches!(refused, Err(TokenError::Unreachable(_))), "{refused:?}");
    let server = https::Server::start(|_: &Request| (200, "{}".to_owned())).await.unwrap();
    let url = format!("{}/v4/account", server.endpoint);
    assert!(matches!(account(&url, "t", Trust::System).await, Err(TokenError::Unreachable(_))));
    assert!(server.requests().is_empty(), "the token never sent");
}

#[test]
fn the_endpoints_are_the_providers_account_ones() {
    assert_eq!(digitalocean::ACCOUNT_URL, "https://api.digitalocean.com/v2/account");
    assert_eq!(linode::ACCOUNT_URL, "https://api.linode.com/v4/account");
}

/// The real providers, over the network: a made-up token is rejected by each.
#[tokio::test]
#[ignore = "asks DigitalOcean's and Linode's APIs over the network"]
async fn the_real_providers_reject_a_made_up_token() {
    assert_eq!(digitalocean::check_token("not-a-token").await, Err(TokenError::Rejected));
    assert_eq!(linode::check_token("not-a-token").await, Err(TokenError::Rejected));
}
