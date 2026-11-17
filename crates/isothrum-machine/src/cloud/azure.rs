//! Azure, through the Azure CLI (`az`): whether its sign-in holds, the subscriptions it can see,
//! and what a subscription may create in a region: the VM sizes Azure lists for it there (each
//! with the reason Azure refuses it to that subscription, if it does) and its core quotas.
//!
//! `az vm list-skus` is slow (a minute is common) and filters by location only: a caller that
//! asks it more than once keeps the answer.

use serde_json::Value;

use super::{ask, json_list, line, says};
use crate::context::Context;
use crate::failure::{Failure, Refusal};

/// The Azure CLI.
pub const CLI: &str = "az";

/// How the Azure CLI refused, read from what it said: a sign-in run out or missing (the runner's
/// reading, `az login`, an `AADSTS` error), or a subscription it can't find.
#[must_use]
pub fn refusal(said: &str) -> Refusal {
    if isothrum_runner::sign_in_expired(said) || says(said, &["az login", "not logged in", "AADSTS"]) {
        Refusal::CredentialsExpired
    } else if says(said, &["was not found"]) {
        Refusal::NotFound
    } else {
        Refusal::Other
    }
}

/// Whether the CLI's sign-in still gives a token (`az account get-access-token`, the token not
/// printed).
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, or Azure refused ([`Refusal::CredentialsExpired`]).
pub async fn signed_in(cx: &Context) -> Result<(), Failure> {
    ask(cx, CLI, &["account", "get-access-token", "--output", "none"], refusal).await.map(drop)
}

/// A subscription the signed-in account can see.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Subscription {
    /// Its display name.
    pub name: String,
    /// Its id.
    pub id: String,
    /// Whether the CLI has it as its default.
    pub is_default: bool,
}

/// The subscriptions the signed-in account can see (`az account list`).
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, Azure refused, or its answer isn't the list.
pub async fn subscriptions(cx: &Context) -> Result<Vec<Subscription>, Failure> {
    let out = ask(
        cx,
        CLI,
        &["account", "list", "--query", "[].{name:name,id:id,isDefault:isDefault}", "--output", "json"],
        refusal,
    )
    .await?;
    read_subscriptions(&out)
}

/// `az account list`'s answer (the query above) as subscriptions: entries without a name or an
/// id read as empty ones.
fn read_subscriptions(out: &str) -> Result<Vec<Subscription>, Failure> {
    let list = json_list(CLI, out)?;
    if list.iter().any(|v| !v.is_object()) {
        return Err(super::unreadable(CLI, "an entry of its list isn't a subscription"));
    }
    Ok(list
        .iter()
        .map(|v| Subscription {
            name: v["name"].as_str().unwrap_or_default().to_owned(),
            id: v["id"].as_str().unwrap_or_default().to_owned(),
            is_default: v["isDefault"].as_bool().unwrap_or(false),
        })
        .collect())
}

/// Subscription `id`'s display name (`az account show`): it is signed in and can reach it.
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, or Azure refused ([`Refusal::CredentialsExpired`]: not
/// signed in; [`Refusal::NotFound`]: no such subscription for this account).
pub async fn subscription_name(cx: &Context, id: &str) -> Result<String, Failure> {
    ask(
        cx,
        CLI,
        &["account", "show", "--subscription", id, "--query", "name", "--output", "tsv"],
        refusal,
    )
    .await
    .map(|out| line(&out))
}

/// A VM size in one location, as `az vm list-skus --all` lists it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Size {
    /// Its name (`Standard_D2als_v6`).
    pub name: String,
    /// Its quota family, lowercase, as [`quotas`] names it.
    pub family: String,
    /// The location this entry is about, lowercase.
    pub location: String,
    /// Why Azure won't create it for this subscription in `location` (its reason code:
    /// `NotAvailableForSubscription`, `QuotaId`), if it won't.
    pub restricted: Option<String>,
    /// Its vCPUs, when listed.
    pub vcpus: Option<u32>,
    /// Its memory in GB, when listed.
    pub memory_gb: Option<f64>,
}

impl Size {
    /// Whether it is the size named `name` (in any case).
    #[must_use]
    pub fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }
}

/// The VM sizes Azure lists for `subscription` (`az vm list-skus --all`, so those refused to it
/// are listed too, with why), in `region` or (None) every region, only `size` when given.
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, Azure refused (signed out, throttled, a bad region), or
/// its answer isn't the list: never an empty list for an answer that isn't one.
pub async fn sizes(cx: &Context, subscription: &str, region: Option<&str>, size: Option<&str>) -> Result<Vec<Size>, Failure> {
    let mut args = vec![
        "vm",
        "list-skus",
        "--subscription",
        subscription,
        "--resource-type",
        "virtualMachines",
        "--all",
        "-o",
        "json",
    ];
    if let Some(region) = region {
        args.extend(["-l", region]);
    }
    if let Some(size) = size {
        args.extend(["--size", size]);
    }
    let out = ask(cx, CLI, &args, refusal).await?;
    Ok(json_list(CLI, &out)?.iter().filter_map(read_size).collect())
}

/// One entry of `az vm list-skus --all`: the location it is about, and whether a `Location`
/// restriction keeps the subscription from it there (a `Zone` one only closes some zones). None
/// for an entry without a name.
fn read_size(v: &Value) -> Option<Size> {
    let lower = |text: &Value| text.as_str().map(str::to_lowercase);
    let location = v["locations"]
        .get(0)
        .and_then(lower)
        .or_else(|| v["locationInfo"].get(0).and_then(|l| lower(&l["location"])))
        .unwrap_or_default();
    let restricted = v["restrictions"].as_array().into_iter().flatten().find_map(|r| {
        let places = r
            .pointer("/restrictionInfo/locations")
            .and_then(Value::as_array)
            .or_else(|| r.get("values").and_then(Value::as_array));
        let here = places.is_none_or(|p| p.iter().any(|l| lower(l).as_deref() == Some(location.as_str())));
        (r["type"].as_str() == Some("Location") && here).then(|| r["reasonCode"].as_str().unwrap_or("restricted").to_owned())
    });
    let capability = |name: &str| v["capabilities"].as_array()?.iter().find(|c| c["name"].as_str() == Some(name))?["value"].as_str();
    Some(Size {
        name: v["name"].as_str()?.to_owned(),
        family: v["family"].as_str().unwrap_or_default().to_lowercase(),
        location,
        restricted,
        vcpus: capability("vCPUs").and_then(|n| n.parse().ok()),
        memory_gb: capability("MemoryGB").and_then(|n| n.parse().ok()),
    })
}

/// Where a size stands in a region's list ([`in_region`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InRegion {
    /// The subscription may create it there: its quota family.
    Usable(String),
    /// The region doesn't offer it.
    NotOffered,
    /// The region offers it, but not to this subscription: Azure's reason code.
    Restricted(String),
}

/// Where `size` stands in `region_sizes` (a region's [`sizes`]).
#[must_use]
pub fn in_region(region_sizes: &[Size], size: &str) -> InRegion {
    match region_sizes.iter().find(|s| s.is(size)) {
        None => InRegion::NotOffered,
        Some(Size { restricted: Some(why), .. }) => InRegion::Restricted(why.clone()),
        Some(s) => InRegion::Usable(s.family.clone()),
    }
}

/// Of `everywhere` (a size's [`sizes`] across every region), the regions other than `region`
/// where the subscription may create `size`, sorted, and how many refuse it to the subscription.
#[must_use]
pub fn elsewhere<'a>(everywhere: &'a [Size], size: &str, region: &str) -> (Vec<&'a str>, usize) {
    let mut usable: Vec<&str> = everywhere
        .iter()
        .filter(|s| s.is(size) && s.restricted.is_none() && s.location != region)
        .map(|s| s.location.as_str())
        .collect();
    usable.sort_unstable();
    usable.dedup();
    let refused = everywhere.iter().filter(|s| s.is(size) && s.restricted.is_some()).count();
    (usable, refused)
}

/// At most `n` sizes of `region_sizes` the subscription may create that are like `size`: as many
/// vCPUs or more, at least its memory, the smallest first (then by name). What `size` is comes
/// from either list; unknown, it is taken as 2 vCPUs.
#[must_use]
pub fn alike<'a>(size: &str, everywhere: Option<&[Size]>, region_sizes: &'a [Size], n: usize) -> Vec<&'a Size> {
    let model = region_sizes.iter().chain(everywhere.into_iter().flatten()).find(|s| s.is(size));
    let (vcpus, memory) = (model.and_then(|m| m.vcpus).unwrap_or(2), model.and_then(|m| m.memory_gb).unwrap_or(0.0));
    let mut usable: Vec<&Size> = region_sizes
        .iter()
        .filter(|s| !s.is(size) && s.restricted.is_none() && s.vcpus.is_some_and(|c| c >= vcpus) && s.memory_gb.unwrap_or(0.0) >= memory)
        .collect();
    usable.sort_by(|a, b| {
        (a.vcpus, a.memory_gb.unwrap_or(0.0))
            .partial_cmp(&(b.vcpus, b.memory_gb.unwrap_or(0.0)))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    usable.truncate(n);
    usable
}

/// A core quota of a subscription in a region, as `az vm list-usage` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Quota {
    /// The VM family it counts (lowercase), as [`Size::family`] names it.
    pub family: String,
    /// Cores allowed.
    pub limit: i64,
    /// Cores in use.
    pub used: i64,
}

impl Quota {
    /// Cores still free.
    #[must_use]
    pub fn free(&self) -> i64 {
        self.limit - self.used
    }
}

/// `subscription`'s core quotas in `region` (`az vm list-usage`).
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, Azure refused, or its answer isn't the list.
pub async fn quotas(cx: &Context, subscription: &str, region: &str) -> Result<Vec<Quota>, Failure> {
    let out = ask(
        cx,
        CLI,
        &["vm", "list-usage", "--subscription", subscription, "-l", region, "-o", "json"],
        refusal,
    )
    .await?;
    Ok(read_quotas(&json_list(CLI, &out)?))
}

/// `az vm list-usage`'s entries as quotas: its numbers come as strings or numbers, one that
/// can't be read counts as 0; an entry without a name is left out.
fn read_quotas(list: &[Value]) -> Vec<Quota> {
    let n = |v: &Value| v.as_str().and_then(|s| s.parse::<i64>().ok()).or_else(|| v.as_i64()).unwrap_or(0);
    list.iter()
        .filter_map(|x| {
            Some(Quota {
                family: x.pointer("/name/value").and_then(Value::as_str)?.to_lowercase(),
                limit: n(&x["limit"]),
                used: n(&x["currentValue"]),
            })
        })
        .collect()
}

/// The cores free in `family`'s quota (`family` lowercase), when `quotas` has it.
#[must_use]
pub fn free_cores(quotas: &[Quota], family: &str) -> Option<i64> {
    quotas.iter().find(|q| q.family == family).map(Quota::free)
}

#[cfg(test)]
mod tests;
