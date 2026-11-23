//! Oracle Cloud, through the OCI CLI (`oci`) and the config file it and Terraform's provider sign
//! in with (`~/.oci/config`, an API key): whether that sign-in holds, a compartment's name, and the
//! config file's default profile.

use std::path::Path;

use super::{ask, line, says};
use crate::context::Context;
use crate::failure::{Failure, Refusal};

/// The OCI CLI.
pub const CLI: &str = "oci";

/// The variable that moves the config file from `~/.oci/config`.
pub const CONFIG_FILE_VAR: &str = "OCI_CLI_CONFIG_FILE";

/// How the OCI CLI refused, read from what it said: a sign-in run out or missing, or the config
/// file's key rejected (the runner's reading, `NotAuthenticated`, a config or private key
/// problem, a 401), or a compartment it can't see (`NotAuthorizedOrNotFound`).
#[must_use]
pub fn refusal(said: &str) -> Refusal {
    if isothrum_runner::sign_in_expired(said) || says(said, &["NotAuthenticated", "config", "private key", "401"]) {
        Refusal::CredentialsExpired
    } else if says(said, &["NotAuthorizedOrNotFound"]) {
        Refusal::Denied
    } else {
        Refusal::Other
    }
}

/// Whether the config file's sign-in still holds: the regions it can list
/// (`oci iam region list`).
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, or Oracle refused ([`Refusal::CredentialsExpired`]).
pub async fn signed_in(cx: &Context) -> Result<(), Failure> {
    ask(cx, CLI, &["iam", "region", "list", "--output", "json"], refusal).await.map(drop)
}

/// Compartment `id`'s name (`oci iam compartment get`): the config file signs in and can reach it.
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, or Oracle refused ([`Refusal::CredentialsExpired`]: the
/// config file's key; [`Refusal::Denied`]: not this account's to see).
pub async fn compartment_name(cx: &Context, id: &str) -> Result<String, Failure> {
    ask(
        cx,
        CLI,
        &["iam", "compartment", "get", "--compartment-id", id, "--query", "data.name", "--raw-output"],
        refusal,
    )
    .await
    .map(|out| line(&out))
}

/// The default profile of an OCI config file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Config {
    /// Its tenancy's OCID, empty when it has none.
    pub tenancy: String,
    /// Its region, empty when it has none.
    pub region: String,
}

/// The config file at `path`'s default profile; None when it can't be read (there is none).
#[must_use]
pub fn config(path: &Path) -> Option<Config> {
    std::fs::read_to_string(path).ok().map(|text| parse_config(&text))
}

/// The `[DEFAULT]` profile's tenancy and region in a config file's text (the section matched in
/// any case; a later one of the same name overrides; a value may hold `=`).
#[must_use]
pub fn parse_config(text: &str) -> Config {
    let mut config = Config::default();
    let mut in_default = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_default = l.eq_ignore_ascii_case("[DEFAULT]");
            continue;
        }
        if !in_default {
            continue;
        }
        if let Some((k, v)) = l.split_once('=') {
            match k.trim() {
                "tenancy" => v.trim().clone_into(&mut config.tenancy),
                "region" => v.trim().clone_into(&mut config.region),
                _ => {}
            }
        }
    }
    config
}

#[cfg(test)]
mod tests;
