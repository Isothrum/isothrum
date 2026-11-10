//! The cloud account a real run uses, from the environment only: which variables the runner reads
//! the connection from (`--secret-from-env`), which params name the account's place, as Isoloom's
//! cloud manifests name them (`AWS_PROFILE` and `region`, `subscription_id`, `project`,
//! `compartment_id` and `config_file_profile`), and where `reap` looks. Nothing is
//! stored; a missing credential refuses the run, saying what to set.

use std::path::Path;

use crate::Res;
use isothrum_runner::isoloom_manifest::Cloud;

/// What the environment gives a real run on a cloud.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Account {
    /// The variables the connection slots are read from (each a slot's own name).
    pub(crate) secrets: Vec<String>,
    /// The params naming the account's place.
    pub(crate) params: Vec<(String, String)>,
    /// `reap`'s arguments for where to look (`--region`, `--project`).
    pub(crate) place: Vec<String>,
}

/// The account `var` (the environment) opens on `cloud`; `exists` tells whether a file is there,
/// `home` is the user's folder (the CLIs' own sign-ins). Else what is missing, each said once.
pub(crate) fn account(cloud: Cloud, var: &dyn Fn(&str) -> Option<String>, exists: &dyn Fn(&Path) -> bool, home: Option<&Path>) -> Result<Account, Vec<String>> {
    let mut a = Account::default();
    let set = |names: &[&str]| names.iter().filter(|n| var(n).is_some()).map(|n| (*n).to_owned()).collect::<Vec<_>>();
    let in_home = |rel: &str| home.is_some_and(|h| exists(&h.join(rel)));
    match cloud {
        Cloud::Aws => {
            a.secrets = set(&["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN"]);
            let keys = var("AWS_ACCESS_KEY_ID").is_some() && var("AWS_SECRET_ACCESS_KEY").is_some();
            let profile = var("AWS_PROFILE");
            if !keys && profile.is_none() {
                return Err(vec!["AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, or AWS_PROFILE".into()]);
            }
            a.params.extend(profile.map(|p| ("AWS_PROFILE".to_owned(), p)));
            if let Some(region) = var("AWS_REGION").or_else(|| var("AWS_DEFAULT_REGION")) {
                a.place = vec!["--region".into(), region.clone()];
                a.params.push(("region".into(), region));
            }
        }
        Cloud::Azure => {
            let subscription = var("ARM_SUBSCRIPTION_ID")
                .ok_or_else(|| vec!["ARM_SUBSCRIPTION_ID (and `az login`, or ARM_CLIENT_ID, ARM_CLIENT_SECRET and ARM_TENANT_ID)".to_owned()])?;
            // The module's own variable: the account a destroy goes back to.
            a.params.push(("subscription_id".into(), subscription));
            a.secrets = set(&["ARM_CLIENT_ID", "ARM_CLIENT_SECRET", "ARM_TENANT_ID"]);
        }
        Cloud::Gcp => {
            let mut missing = Vec::new();
            match var("GOOGLE_PROJECT") {
                Some(p) => {
                    a.place = vec!["--project".into(), p.clone()];
                    a.params.push(("project".into(), p));
                }
                None => missing.push("GOOGLE_PROJECT".to_owned()),
            }
            a.secrets = set(&["GOOGLE_OAUTH_ACCESS_TOKEN"]);
            if a.secrets.is_empty() && !in_home(".config/gcloud/application_default_credentials.json") {
                missing.push("GOOGLE_OAUTH_ACCESS_TOKEN, or `gcloud auth application-default login`".into());
            }
            if !missing.is_empty() {
                return Err(missing);
            }
        }
        Cloud::Digitalocean => a.secrets = token(&set, "DIGITALOCEAN_TOKEN")?,
        Cloud::Linode => a.secrets = token(&set, "LINODE_TOKEN")?,
        Cloud::Oci => {
            let mut missing = Vec::new();
            if !in_home(".oci/config") {
                missing.push("~/.oci/config (`oci setup config`)".to_owned());
            }
            match var("OCI_COMPARTMENT_ID") {
                Some(c) => a.params.push(("compartment_id".into(), c)),
                None => missing.push("OCI_COMPARTMENT_ID".into()),
            }
            if !missing.is_empty() {
                return Err(missing);
            }
            // The profile of ~/.oci/config the provider signs in with (the module's default:
            // `DEFAULT`).
            a.params.extend(var("OCI_CLI_PROFILE").map(|p| ("config_file_profile".to_owned(), p)));
        }
    }
    Ok(a)
}

/// A cloud signed in with one token, `name`.
fn token(set: &dyn Fn(&[&str]) -> Vec<String>, name: &str) -> Result<Vec<String>, Vec<String>> {
    let found = set(&[name]);
    if found.is_empty() { Err(vec![name.to_owned()]) } else { Ok(found) }
}

/// The public resolvers asked for this machine's address: two must agree.
const RESOLVERS: [&str; 4] = [
    "https://checkip.amazonaws.com",
    "https://api.ipify.org",
    "https://ifconfig.me/ip",
    "https://icanhazip.com",
];

/// This machine's public IPv4 address, from two resolvers that agree (through `curl`).
///
/// # Errors
///
/// `curl` can't start, or fewer than two resolvers answered with the same address, or two
/// disagreed.
pub(crate) fn public_ip() -> Res<String> {
    let mut answers: Vec<Option<String>> = Vec::new();
    for url in RESOLVERS {
        let out = std::process::Command::new("curl")
            .args(["-fsS", "--max-time", "8", url])
            .output()
            .map_err(|e| format!("`curl` couldn't start, to ask {url} for this machine's address: {e}"))?;
        answers.push(out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned()));
    }
    agreed(&answers).ok_or_else(|| "the public resolvers didn't agree on this machine's address: set ALLOWED_CIDR (<your IP>/32)".into())
}

/// The IPv4 address at least two of `answers` give, when none gives another.
pub(crate) fn agreed(answers: &[Option<String>]) -> Option<String> {
    let ips: Vec<std::net::Ipv4Addr> = answers.iter().flatten().filter_map(|a| a.parse().ok()).collect();
    let first = *ips.first()?;
    (ips.len() >= 2 && ips.iter().all(|ip| *ip == first)).then(|| first.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| (*v).to_owned())
    }

    fn none(_: &Path) -> bool {
        false
    }

    #[test]
    fn aws_takes_keys_or_a_profile_and_its_region() {
        let keys = account(
            Cloud::Aws,
            &env(&[("AWS_ACCESS_KEY_ID", "AKIA"), ("AWS_SECRET_ACCESS_KEY", "s"), ("AWS_REGION", "eu-west-3")]),
            &none,
            None,
        )
        .unwrap();
        assert_eq!(keys.secrets, ["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY"]);
        assert_eq!(keys.params, [("region".to_owned(), "eu-west-3".to_owned())]);
        assert_eq!(keys.place, ["--region", "eu-west-3"]);
        let profile = account(Cloud::Aws, &env(&[("AWS_PROFILE", "demo")]), &none, None).unwrap();
        assert_eq!(profile.params, [("AWS_PROFILE".to_owned(), "demo".to_owned())]);
        let nothing = account(Cloud::Aws, &env(&[("AWS_ACCESS_KEY_ID", "AKIA")]), &none, None).unwrap_err();
        assert_eq!(nothing, ["AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, or AWS_PROFILE"]);
    }

    #[test]
    fn every_cloud_says_what_it_misses() {
        for (cloud, says) in [
            (Cloud::Azure, "ARM_SUBSCRIPTION_ID"),
            (Cloud::Gcp, "GOOGLE_PROJECT"),
            (Cloud::Digitalocean, "DIGITALOCEAN_TOKEN"),
            (Cloud::Linode, "LINODE_TOKEN"),
            (Cloud::Oci, "~/.oci/config"),
        ] {
            let missing = account(cloud, &env(&[]), &none, Some(Path::new("/home/u"))).unwrap_err();
            assert!(missing.iter().any(|m| m.contains(says)), "{cloud:?}: {missing:?}");
        }
    }

    #[test]
    fn google_cloud_signs_in_with_a_token_or_the_clis_own_credentials() {
        let adc = |p: &Path| p == Path::new("/home/u/.config/gcloud/application_default_credentials.json");
        let a = account(Cloud::Gcp, &env(&[("GOOGLE_PROJECT", "demo-1")]), &adc, Some(Path::new("/home/u"))).unwrap();
        assert_eq!(a.params, [("project".to_owned(), "demo-1".to_owned())]);
        assert_eq!(a.place, ["--project", "demo-1"]);
        let token = account(Cloud::Gcp, &env(&[("GOOGLE_PROJECT", "p"), ("GOOGLE_OAUTH_ACCESS_TOKEN", "t")]), &none, None).unwrap();
        assert_eq!(token.secrets, ["GOOGLE_OAUTH_ACCESS_TOKEN"]);
    }

    #[test]
    fn oracle_cloud_needs_its_config_and_a_compartment() {
        let config = |p: &Path| p == Path::new("/home/u/.oci/config");
        let a = account(
            Cloud::Oci,
            &env(&[("OCI_COMPARTMENT_ID", "ocid1.compartment")]),
            &config,
            Some(Path::new("/home/u")),
        )
        .unwrap();
        assert_eq!(a.params, [("compartment_id".to_owned(), "ocid1.compartment".to_owned())]);
    }

    #[test]
    fn this_machines_address_is_one_two_resolvers_agree_on() {
        let s = |v: &str| Some(v.to_owned());
        assert_eq!(agreed(&[s("203.0.113.7"), None, s("203.0.113.7"), s("<html>")]).as_deref(), Some("203.0.113.7"));
        assert_eq!(agreed(&[s("203.0.113.7"), None, None, None]), None, "one is not enough");
        assert_eq!(
            agreed(&[s("203.0.113.7"), s("198.51.100.1"), s("203.0.113.7")]),
            None,
            "a disagreement stops it"
        );
    }

    #[test]
    fn oracle_clouds_profile_is_the_modules_config_file_profile() {
        let config = |p: &Path| p == Path::new("/home/u/.oci/config");
        let vars = env(&[("OCI_COMPARTMENT_ID", "c"), ("OCI_CLI_PROFILE", "DEMO")]);
        let a = account(Cloud::Oci, &vars, &config, Some(Path::new("/home/u"))).unwrap();
        assert!(a.params.contains(&("config_file_profile".to_owned(), "DEMO".to_owned())), "{a:?}");
    }

    #[test]
    fn azures_account_is_the_modules_subscription() {
        let vars = env(&[("ARM_SUBSCRIPTION_ID", "sub-1"), ("ARM_CLIENT_ID", "app")]);
        let a = account(Cloud::Azure, &vars, &none, None).unwrap();
        assert_eq!(a.params, [("subscription_id".to_owned(), "sub-1".to_owned())]);
        assert_eq!(a.secrets, ["ARM_CLIENT_ID"]);
    }
}
