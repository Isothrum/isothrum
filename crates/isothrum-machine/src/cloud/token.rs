//! Clouds whose credential is an API token alone: whether a token still signs in, asked of the
//! provider's own `account` endpoint over HTTPS (no CLI to ask). DigitalOcean's
//! ([`digitalocean`]) and Linode's ([`linode`]). What a refusal means to a user is the caller's.

use std::time::Duration;

use isothrum_runner::tls::{Tls, Trust};

use crate::place::proxmox::chain;

/// How long the provider may take to answer.
pub const TOKEN_TIMEOUT: Duration = Duration::from_secs(15);

/// Why a token wasn't accepted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TokenError {
    /// The provider refused the token (HTTP 401): wrong, revoked or expired.
    #[error("the provider rejected the token")]
    Rejected,
    /// The provider answered with another status than a success (403 for a read-only scope, 5xx).
    #[error("the provider's API answered HTTP {0}")]
    Status(u16),
    /// No answer: no network, the name not resolved, TLS refused, timed out. Why.
    #[error("can't reach the provider: {0}")]
    Unreachable(String),
}

/// DigitalOcean: `GET /v2/account` with the token.
pub mod digitalocean {
    use super::{TokenError, Trust, account};

    /// The account endpoint a token is checked against.
    pub const ACCOUNT_URL: &str = "https://api.digitalocean.com/v2/account";

    /// Whether `token` signs in to DigitalOcean.
    ///
    /// # Errors
    ///
    /// The token was refused, or the API didn't answer as it does to a valid one.
    pub async fn check_token(token: &str) -> Result<(), TokenError> {
        account(ACCOUNT_URL, token, Trust::System).await
    }
}

/// Linode (Akamai): `GET /v4/account` with the token.
pub mod linode {
    use super::{TokenError, Trust, account};

    /// The account endpoint a token is checked against.
    pub const ACCOUNT_URL: &str = "https://api.linode.com/v4/account";

    /// Whether `token` signs in to Linode.
    ///
    /// # Errors
    ///
    /// The token was refused, or the API didn't answer as it does to a valid one.
    pub async fn check_token(token: &str) -> Result<(), TokenError> {
        account(ACCOUNT_URL, token, Trust::System).await
    }
}

/// `GET url` with `token` as its bearer, the server trusted as `trust` says: a success is a token
/// that signs in.
async fn account(url: &str, token: &str, trust: Trust) -> Result<(), TokenError> {
    let tls = Tls::new(trust).map_err(|e| TokenError::Unreachable(e.to_string()))?;
    let http = reqwest::Client::builder()
        .tls_backend_preconfigured(tls.config().clone())
        .connect_timeout(TOKEN_TIMEOUT)
        .timeout(TOKEN_TIMEOUT)
        .build()
        .map_err(|e| TokenError::Unreachable(chain(&e)))?;
    let answer = http.get(url).bearer_auth(token).send().await.map_err(|e| TokenError::Unreachable(chain(&e)))?;
    match answer.status().as_u16() {
        200..=299 => Ok(()),
        401 => Err(TokenError::Rejected),
        other => Err(TokenError::Status(other)),
    }
}

#[cfg(test)]
mod tests;
