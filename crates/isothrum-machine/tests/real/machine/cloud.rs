//! The clouds' token checks against the providers' real APIs, over the network.

use isothrum_machine::cloud::{TokenError, digitalocean, linode};

/// A made-up token is rejected by each provider; skips when a provider can't be reached.
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_real_providers_reject_a_made_up_token() {
    for (provider, said) in [
        ("DigitalOcean", digitalocean::check_token("not-a-token").await),
        ("Linode", linode::check_token("not-a-token").await),
    ] {
        if let Err(TokenError::Unreachable(why)) = &said {
            skip!("{provider}'s API can't be reached from here: {why}")
        }
        assert_eq!(said, Err(TokenError::Rejected), "{provider}");
    }
}
