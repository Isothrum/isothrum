//! Google Cloud, through the Google Cloud CLI (`gcloud`): the account it is signed in as, whether
//! its own sign-in and the application-default one (what Terraform's Google provider reads) still
//! hold, the billing accounts and organizations the account can see, and its projects: what state
//! one is in, and making one ready for deployments (created, linked to a billing account, an API
//! enabled) or deleting it.

use std::time::Duration;

use isothrum_runner::ProcessSpec;
use serde_json::Value;

use super::{ask, json_list, line, run, says};
use crate::context::Context;
use crate::failure::{Failure, Refusal};

/// The Google Cloud CLI.
pub const CLI: &str = "gcloud";

/// How gcloud refused, read from what it said: a sign-in run out or missing (the runner's reading,
/// a `gcloud auth` hint, credentials), what was asked about not found (the API's `NOT_FOUND`, a
/// word of its own), a permission denied, an API not enabled, a quota used up (a billing account
/// with no free project slot is refused as `FAILED_PRECONDITION`).
#[must_use]
pub fn refusal(said: &str) -> Refusal {
    if isothrum_runner::sign_in_expired(said) || says(said, &["gcloud auth", "credentials"]) {
        Refusal::CredentialsExpired
    } else if names_status(said, "NOT_FOUND") {
        Refusal::NotFound
    } else if names_status(said, "SERVICE_DISABLED") || says(said, &["has not been used in project", "it is disabled"]) {
        // Before a denial: an API turned off is refused as `PERMISSION_DENIED`.
        Refusal::NotEnabled
    } else if says(said, &["does not have permission", "PERMISSION_DENIED", "Permission denied", "was not found"]) {
        Refusal::Denied
    } else if says(said, &["quota", "FAILED_PRECONDITION", "Precondition"]) {
        Refusal::QuotaExceeded
    } else {
        Refusal::Other
    }
}

/// Whether `said` names the API status `status`, which gcloud writes in capitals as a word of its
/// own (`ERROR: (gcloud.projects.describe) NOT_FOUND: …`).
fn names_status(said: &str, status: &str) -> bool {
    said.split(|c: char| !(c.is_ascii_uppercase() || c == '_')).any(|word| word == status)
}

/// The account gcloud is signed in as (`gcloud auth list`, its active one): None when none is.
/// An account whose sign-in ran out still shows as active here: [`access_token`] and
/// [`signed_in`] tell whether it holds.
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, or failed.
pub async fn active_account(cx: &Context) -> Result<Option<String>, Failure> {
    let out = ask(cx, CLI, &["auth", "list", "--filter=status:ACTIVE", "--format=value(account)"], refusal).await?;
    Ok(Some(line(&out)).filter(|a| !a.is_empty()))
}

/// Whether gcloud's own sign-in still gives a token (`gcloud auth print-access-token`, the token
/// not kept).
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, or Google refused ([`Refusal::CredentialsExpired`]).
pub async fn access_token(cx: &Context) -> Result<(), Failure> {
    ask(cx, CLI, &["auth", "print-access-token", "--quiet"], refusal).await.map(drop)
}

/// Whether the application-default sign-in, the one Terraform's Google provider signs in with,
/// still gives a token (`gcloud auth application-default print-access-token`, the token not
/// kept): the sign-in a deployment on Google Cloud needs.
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, or Google refused ([`Refusal::CredentialsExpired`]).
pub async fn signed_in(cx: &Context) -> Result<(), Failure> {
    ask(cx, CLI, &["auth", "application-default", "print-access-token"], refusal).await.map(drop)
}

/// A billing account the signed-in account can see.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct BillingAccount {
    /// Its id (`billingAccounts/` left out).
    pub id: String,
    /// Its display name.
    pub name: String,
    /// Whether it is open, so it can pay for a project.
    pub open: bool,
}

/// The billing accounts the signed-in account can see (`gcloud billing accounts list`).
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, Google refused (the Cloud Billing API off, a permission
/// denied), or its answer isn't the list.
pub async fn billing_accounts(cx: &Context) -> Result<Vec<BillingAccount>, Failure> {
    let out = ask(cx, CLI, &["billing", "accounts", "list", "--format", "json"], refusal).await?;
    Ok(read_billing_accounts(&json_list(CLI, &out)?))
}

/// Billing accounts as gcloud lists them: an entry without an id is left out, a field of the
/// wrong type read as empty.
fn read_billing_accounts(list: &[Value]) -> Vec<BillingAccount> {
    list.iter()
        .map(|v| BillingAccount {
            id: id(v, "billingAccounts/"),
            name: v["displayName"].as_str().unwrap_or_default().to_owned(),
            open: v["open"].as_bool().unwrap_or(false),
        })
        .filter(|b| !b.id.is_empty())
        .collect()
}

/// An entry's `name` without its `prefix`.
fn id(v: &Value, prefix: &str) -> String {
    v["name"].as_str().unwrap_or_default().trim_start_matches(prefix).to_owned()
}

/// Billing account `id`'s display name (`gcloud billing accounts describe`): gcloud is signed in
/// and can see it.
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, or Google refused ([`Refusal::CredentialsExpired`]: not
/// signed in; [`Refusal::Denied`] or [`Refusal::NotFound`]: not this account's to see).
pub async fn billing_account_name(cx: &Context, id: &str) -> Result<String, Failure> {
    ask(cx, CLI, &["billing", "accounts", "describe", id, "--format", "value(displayName)"], refusal)
        .await
        .map(|out| line(&out))
}

/// An organization the signed-in account belongs to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Organization {
    /// Its numeric id (`organizations/` left out).
    pub id: String,
    /// Its display name.
    pub name: String,
}

/// The organizations the signed-in account belongs to (`gcloud organizations list`): none for a
/// personal account.
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, Google refused, or its answer isn't the list (gcloud
/// prints `Listed 0 items.` for none, which isn't one).
pub async fn organizations(cx: &Context) -> Result<Vec<Organization>, Failure> {
    let out = ask(cx, CLI, &["organizations", "list", "--format", "json"], refusal).await?;
    Ok(read_organizations(&json_list(CLI, &out)?))
}

/// Organizations as gcloud lists them: an entry without an id is left out.
fn read_organizations(list: &[Value]) -> Vec<Organization> {
    list.iter()
        .map(|v| Organization {
            id: id(v, "organizations/"),
            name: v["displayName"].as_str().unwrap_or_default().to_owned(),
        })
        .filter(|o| !o.id.is_empty())
        .collect()
}

/// How long creating a project, linking it to billing, enabling an API on it or deleting it may
/// take.
pub const PROJECT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// What state a project is in, as Google has it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProjectState {
    /// It can be used.
    Active,
    /// It doesn't exist (`NOT_FOUND`), or is being deleted: it can't be used again.
    Gone,
    /// Another state Google names (`SUSPENDED`...).
    Other(String),
}

/// Project `id`'s state (`gcloud projects describe`). Gone only when Google says so plainly: the
/// project is being deleted, or `NOT_FOUND`. "No permission (or it may not exist)", what another
/// signed-in account gets, is a [`Refusal::Denied`], never gone.
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, Google refused (signed out, no permission), or no answer.
pub async fn project_state(cx: &Context, id: &str) -> Result<ProjectState, Failure> {
    match ask(cx, CLI, &["projects", "describe", id, "--format", "value(lifecycleState)"], refusal).await {
        Ok(state) => Ok(match state.trim() {
            "ACTIVE" => ProjectState::Active,
            "DELETE_REQUESTED" | "DELETE_IN_PROGRESS" => ProjectState::Gone,
            other => ProjectState::Other(other.to_owned()),
        }),
        Err(Failure::Refused {
            refusal: Refusal::NotFound, ..
        }) => Ok(ProjectState::Gone),
        Err(other) => Err(other),
    }
}

/// `gcloud args` within [`PROJECT_TIMEOUT`].
async fn change(cx: &Context, args: &[&str]) -> Result<(), Failure> {
    run(cx, PROJECT_TIMEOUT, &ProcessSpec::new(CLI, args.iter().copied()), refusal).await.map(drop)
}

/// The arguments of `gcloud projects create` for project `id` named `name`, under `organization`
/// (its numeric id) when given.
#[must_use]
pub fn create_args(id: &str, name: &str, organization: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = ["projects", "create", id, "--name", name].map(str::to_owned).to_vec();
    args.extend(organization.map(|o| format!("--organization={o}")));
    args
}

/// Creates project `id` named `name`, under `organization` when given.
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, or Google refused (the id taken, a quota of projects).
pub async fn create_project(cx: &Context, id: &str, name: &str, organization: Option<&str>) -> Result<(), Failure> {
    let args = create_args(id, name, organization);
    change(cx, &args.iter().map(String::as_str).collect::<Vec<_>>()).await
}

/// Links project `id` to billing account `billing`, which then pays for it.
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, or Google refused ([`Refusal::QuotaExceeded`]: the billing
/// account has no free project slot).
pub async fn link_billing(cx: &Context, id: &str, billing: &str) -> Result<(), Failure> {
    change(cx, &["billing", "projects", "link", id, "--billing-account", billing]).await
}

/// Enables the API `service` (`compute.googleapis.com`) on project `id`.
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, or Google refused.
pub async fn enable_service(cx: &Context, id: &str, service: &str) -> Result<(), Failure> {
    change(cx, &["services", "enable", service, "--project", id]).await
}

/// Deletes project `id` (Google keeps it 30 days, deleted, before it is gone for good), which frees
/// its billing account's slot.
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, or Google refused.
pub async fn delete_project(cx: &Context, id: &str) -> Result<(), Failure> {
    change(cx, &["projects", "delete", id, "--quiet"]).await
}

#[cfg(test)]
mod tests;
