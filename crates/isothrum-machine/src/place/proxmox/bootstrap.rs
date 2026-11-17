//! Setting a Proxmox VE node up over SSH, signed in with a key the node already lets in (a user's
//! own): a second key (the caller's, made by [`ssh::ensure_key`](crate::place::ssh::ensure_key))
//! is let into the user's `authorized_keys`, then an API token is (re)made with `pveum`, its
//! secret returned once. The node's host key must already be confirmed in the target's
//! `known_hosts` ([`known_hosts`](crate::place::ssh::known_hosts)).

use std::fmt::Write as _;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::is_token;
use crate::Context;
use crate::place::ssh::{SshError, Target, sh_quote};

/// The most one SSH call to the node may take.
const SSH_LIMIT: Duration = Duration::from_secs(60);

/// The longest token secret taken from `pveum`'s answer: Proxmox's are UUIDs (36 characters);
/// anything far longer is not a secret but something else printed.
const MAX_SECRET_LEN: usize = 128;

/// A token name for the key `public` on a node: `prefix`, a dash and the first 8 hex digits of the
/// key's SHA-256, so two computers set up on one node each keep their own token, and setting up
/// again from one replaces its own.
#[must_use]
pub fn token_name(prefix: &str, public: &str) -> String {
    let digest = Sha256::digest(public.trim().as_bytes());
    let mut short = String::with_capacity(8);
    for b in digest.iter().take(4) {
        let _written = write!(short, "{b:02x}");
    }
    format!("{prefix}-{short}")
}

/// The node command that (re)makes token `name` of `user@pam`: an older token of the same name is
/// removed first, since Proxmox shows a token's secret only when it is made. Privilege separation
/// off: the token acts as its user.
fn token_command(user: &str, name: &str) -> String {
    let (user, name) = (sh_quote(&format!("{user}@pam")), sh_quote(name));
    format!("pveum user token remove {user} {name} >/dev/null 2>&1; pveum user token add {user} {name} --privsep 0 --output-format json")
}

/// The node command that adds `public` to the user's `authorized_keys` unless it is there
/// already, printing `added` or `present`.
fn authorize_command(public: &str) -> String {
    let key = sh_quote(public.trim());
    format!(
        "umask 077; mkdir -p ~/.ssh; if grep -qxF {key} ~/.ssh/authorized_keys 2>/dev/null; then echo present; else echo {key} >> ~/.ssh/authorized_keys && echo added; fi"
    )
}

/// An API token `pveum user token add` made.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiToken {
    /// Its full id, `user@realm!name`.
    pub id: String,
    /// The value the API is called with; shown once.
    pub secret: String,
}

impl std::fmt::Debug for ApiToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiToken").field("id", &self.id).finish_non_exhaustive()
    }
}

/// `pveum`'s JSON answer.
#[derive(Deserialize)]
struct Added {
    #[serde(rename = "full-tokenid", default)]
    id: String,
    #[serde(rename = "value", default)]
    secret: String,
}

/// The token in `pveum user token add --output-format json`'s answer; why not, when it has none.
fn parse_token(out: &str) -> Result<ApiToken, String> {
    let json = out
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| format!("pveum gave no token: {}", out.trim()))?;
    let added: Added = serde_json::from_str(json).map_err(|e| format!("pveum's answer isn't JSON ({e})"))?;
    let secret_ok = !added.secret.is_empty() && added.secret.len() <= MAX_SECRET_LEN && added.secret.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !is_token(&added.id) || !secret_ok {
        return Err("pveum's answer has no token id and secret".to_owned());
    }
    Ok(ApiToken {
        id: added.id,
        secret: added.secret,
    })
}

/// Which part of the setup failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Part {
    /// Signing in to the node over SSH (no connection, the key refused, a host key not confirmed).
    Ssh,
    /// Letting the second key in.
    Key,
    /// Making the API token.
    Token,
}

/// A failed part and why.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{part:?}: {detail}")]
pub struct BootstrapError {
    /// The part that failed.
    pub part: Part,
    /// Why, in the words of the tool that failed.
    pub detail: String,
    /// The node's host key wasn't one confirmed for it.
    pub host_key_refused: bool,
}

/// Whether ssh itself failed (no connection, the key refused, an unknown host key), rather than
/// the command it ran on the node.
fn ssh_failed(said: &str) -> bool {
    [
        "Permission denied",
        "Connection refused",
        "Connection timed out",
        "Operation timed out",
        "No route to host",
        "Could not resolve",
        "Host key verification failed",
        "Too many authentication failures",
        "Load key",
        "Connection closed",
        "kex_exchange_identification",
    ]
    .iter()
    .any(|m| said.contains(m))
}

/// The failure of `part` when SSH gave `e`: ssh's own when ssh itself failed.
fn failure(e: &SshError, part: Part) -> BootstrapError {
    let host_key_refused = matches!(e, SshError::HostKeyRefused { .. });
    let part = match e {
        SshError::Failed(said) if !ssh_failed(said) => part,
        _ => Part::Ssh,
    };
    BootstrapError {
        part,
        detail: e.to_string(),
        host_key_refused,
    }
}

/// Lets `public` into the `authorized_keys` of `node`'s user, over SSH as `node` says; `true` when
/// it was added, `false` when it was there already.
///
/// # Errors
///
/// SSH failed ([`Part::Ssh`]), or the node's answer wasn't `added` or `present` ([`Part::Key`]).
pub async fn authorize(ctx: &Context, node: &Target, public: &str) -> Result<bool, BootstrapError> {
    let out = node
        .exec(ctx, &authorize_command(public), SSH_LIMIT)
        .await
        .map_err(|e| failure(&e, Part::Key))?;
    if out.lines().any(|l| l.trim() == "added") {
        Ok(true)
    } else if out.lines().any(|l| l.trim() == "present") {
        Ok(false)
    } else {
        Err(BootstrapError {
            part: Part::Key,
            detail: format!("unexpected answer: {}", out.trim()),
            host_key_refused: false,
        })
    }
}

/// What [`bootstrap`] set up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Made {
    /// The token made.
    pub token: ApiToken,
    /// Whether the key was added now (false: it was there already).
    pub key_added: bool,
}

/// Sets `node` up over SSH as `node` says: lets `public` in (first: it also proves SSH works
/// before anything is changed), then (re)makes the API token `token_name` of `node`'s user (in the
/// `pam` realm).
///
/// # Errors
///
/// The part that failed, and why.
pub async fn bootstrap(ctx: &Context, node: &Target, public: &str, token_name: &str) -> Result<Made, BootstrapError> {
    let key_added = authorize(ctx, node, public).await?;
    let out = node
        .exec(ctx, &token_command(&node.user, token_name), SSH_LIMIT)
        .await
        .map_err(|e| failure(&e, Part::Token))?;
    let token = parse_token(&out).map_err(|detail| BootstrapError {
        part: Part::Token,
        detail,
        host_key_refused: false,
    })?;
    Ok(Made { token, key_added })
}

#[cfg(test)]
mod tests;
