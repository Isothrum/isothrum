//! What a cloud's tools say when the sign-in they use has run out or is missing: Terraform's
//! providers, and the AWS, Azure, Google Cloud and Oracle Cloud CLIs they call, from a CLI login
//! or SSO session, an expired session token, application-default credentials gone stale, or a
//! profile that was never signed in. And which cloud a Terraform failure is about, by the provider
//! it names.

use crate::project::platform::Cloud;

/// Each cloud's wordings of a sign-in run out or missing (lowercase, as compared).
const EXPIRED: [&str; 21] = [
    // AWS
    "login session has expired",
    "please reauthenticate",
    "expiredtoken",
    "security token included in the request is expired",
    "token has expired",
    "error loading sso token",
    "unable to locate credentials",
    "aws login",
    // Azure
    "az login",
    "aadsts700082",
    "aadsts50173",
    "refresh token has expired",
    "interactive authentication is needed",
    // Google Cloud
    "reauthentication failed",
    "problem refreshing your current auth tokens",
    "gcloud auth application-default login",
    "could not find default credentials",
    "invalid_grant",
    // Oracle Cloud
    "session is no longer valid",
    "oci session authenticate",
    "notauthenticated",
];

/// Each cloud by its Terraform provider's source address, as a failure names it
/// (`provider["registry.terraform.io/hashicorp/aws"]`).
const PROVIDERS: [(&str, Cloud); 6] = [
    ("hashicorp/aws", Cloud::Aws),
    ("hashicorp/azurerm", Cloud::Azure),
    ("hashicorp/google", Cloud::Gcp),
    ("oracle/oci", Cloud::Oci),
    ("digitalocean/digitalocean", Cloud::Digitalocean),
    ("linode/linode", Cloud::Linode),
];

/// Whether `said`, what a cloud's tool printed, says its sign-in ran out or is missing.
pub(crate) fn expired(said: &str) -> bool {
    let lower = said.to_ascii_lowercase();
    EXPIRED.iter().any(|s| lower.contains(s))
}

/// The cloud whose Terraform provider `said` names; none when it names none of theirs.
pub(crate) fn cloud_named(said: &str) -> Option<Cloud> {
    PROVIDERS.iter().find(|(source, _)| said.contains(source)).map(|(_, cloud)| *cloud)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_clouds_expired_sign_in_is_read_and_its_cloud_named() {
        for said in [
            "Error: operation error STS: GetCallerIdentity, https response error StatusCode: 400, api error ExpiredToken: The security token included in the request is expired",
            "Error loading SSO Token: Token for my-sso does not exist",
            "ERROR: AADSTS700082: The refresh token has expired due to inactivity.",
            "ERROR: (gcloud.compute.instances.list) There was a problem refreshing your current auth tokens: Reauthentication failed. cannot prompt during non-interactive execution.",
            "ServiceError: {'code': 'NotAuthenticated', 'message': 'The required information to complete authentication was not provided.'}",
        ] {
            assert!(expired(said), "{said}");
        }
        assert!(!expired("Error: creating EC2 Instance: InsufficientInstanceCapacity"));
        assert!(!expired(""));
        assert_eq!(cloud_named("with provider[\"registry.terraform.io/hashicorp/azurerm\"]"), Some(Cloud::Azure));
        assert_eq!(cloud_named("provider[\"registry.terraform.io/oracle/oci\"]"), Some(Cloud::Oci));
        assert_eq!(cloud_named("nothing of a cloud"), None);
    }
}
