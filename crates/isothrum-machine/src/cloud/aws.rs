//! AWS, through the AWS CLI: who an account's credentials are, the CLI's profiles, this month's
//! spend (Cost Explorer), and whether the account may create EC2 instances (a dry run, which
//! creates nothing). An account signs in with the CLI's own credentials (its default chain, or a
//! profile), or with an access key, given to the CLI's official container (Docker is all that
//! needs).

use std::time::{Duration, SystemTime};

use isothrum_runner::ProcessSpec;

use super::{CLI_TIMEOUT, line, run, says, unreadable};
use crate::context::Context;
use crate::failure::{Failure, Refusal};

/// The AWS CLI.
pub const CLI: &str = "aws";

/// The AWS CLI's official image, which runs a call made with an access key.
pub const CLI_IMAGE: &str = "amazon/aws-cli:2.37.9";

/// How long a call in [`CLI_IMAGE`] may take: the first one pulls the image.
pub const IMAGE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// The question whose answer is who an account's credentials are, the identity's ARN: the one
/// that tells its sign-in still holds.
pub const WHO_AM_I: [&str; 6] = ["sts", "get-caller-identity", "--query", "Arn", "--output", "text"];

/// The instance type the launch permission is dry-run with.
pub const PROBE_INSTANCE_TYPE: &str = "t3.micro";

/// The SSM public parameter naming the latest Amazon Linux 2023 image (`x86_64`, for
/// [`PROBE_INSTANCE_TYPE`]) in the account's region: a real, public image the dry run is checked
/// against.
pub const LATEST_AMAZON_LINUX: &str = "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64";

/// A well-formed image id no account has: AWS answers it with `InvalidAMIID.NotFound`, which it
/// only says once the request is authorized. Used when the public parameter can't be read
/// (`ssm:GetParameter` denied, SSM unreachable). Not a 17-character made-up id: AWS reads those as
/// malformed before it looks at the permission at all.
pub const UNKNOWN_IMAGE: &str = "ami-12345678";

/// Where an account's credentials come from.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Credentials {
    /// The AWS CLI's own: its default chain, or `profile`.
    Cli {
        /// The profile, when not the default.
        profile: Option<String>,
    },
    /// An access key: the CLI runs in [`CLI_IMAGE`], the key in its environment.
    Keys {
        /// The access key id.
        id: String,
        /// The secret access key.
        secret: String,
    },
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credentials::Cli { profile } => f.debug_struct("Cli").field("profile", profile).finish(),
            Credentials::Keys { id, .. } => f.debug_struct("Keys").field("id", id).field("secret", &"<masked>").finish(),
        }
    }
}

/// An AWS account: its credentials, and the region asked.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Account {
    /// Where its credentials come from.
    pub credentials: Credentials,
    /// The region its calls go to (`AWS_REGION`), when one is set.
    pub region: Option<String>,
}

impl Account {
    /// An account with `credentials`, its calls to `region`.
    #[must_use]
    pub fn new(credentials: Credentials, region: Option<String>) -> Account {
        Account { credentials, region }
    }

    /// The program, arguments and environment of `aws args` the way this account's credentials
    /// run: the host CLI (its profile in `AWS_PROFILE`), or the CLI's container with the key in
    /// its environment.
    #[must_use]
    pub fn invocation(&self, args: &[&str]) -> (&'static str, Vec<String>, Vec<(String, String)>) {
        let mut env: Vec<(String, String)> = Vec::new();
        let region = self.region.iter().map(|r| ("AWS_REGION".to_owned(), r.clone()));
        match &self.credentials {
            Credentials::Cli { profile } => {
                env.extend(region);
                env.extend(profile.iter().map(|p| ("AWS_PROFILE".to_owned(), p.clone())));
                (CLI, args.iter().map(|a| (*a).to_owned()).collect(), env)
            }
            Credentials::Keys { id, secret } => {
                env.push(("AWS_ACCESS_KEY_ID".to_owned(), id.clone()));
                env.push(("AWS_SECRET_ACCESS_KEY".to_owned(), secret.clone()));
                env.extend(region);
                let mut docker: Vec<String> = [
                    "run",
                    "--rm",
                    "-e",
                    "AWS_ACCESS_KEY_ID",
                    "-e",
                    "AWS_SECRET_ACCESS_KEY",
                    "-e",
                    "AWS_REGION",
                    CLI_IMAGE,
                ]
                .map(str::to_owned)
                .to_vec();
                docker.extend(args.iter().map(|a| (*a).to_owned()));
                ("docker", docker, env)
            }
        }
    }

    /// `aws args` as this account: its standard output, else why not.
    async fn ask(&self, cx: &Context, args: &[&str]) -> Result<String, Failure> {
        let (program, invoked, env) = self.invocation(args);
        let limit = if program == CLI { CLI_TIMEOUT } else { IMAGE_TIMEOUT };
        let mut spec = ProcessSpec::new(program, invoked);
        spec.env = env;
        run(cx, limit, &spec, refusal).await
    }
}

/// How the AWS CLI refused, read from what it said: a sign-in run out or missing, or keys
/// rejected (the runner's reading of an expired sign-in, and the CLI's own words), Cost Explorer
/// not enabled, a permission denied.
#[must_use]
pub fn refusal(said: &str) -> Refusal {
    const SIGNED_OUT: [&str; 7] = [
        "InvalidClientTokenId",
        "SignatureDoesNotMatch",
        "Unable to locate credentials",
        "sso",
        "not logged in",
        "TokenRefresh",
        "credentials",
    ];
    if isothrum_runner::sign_in_expired(said) || says(said, &SIGNED_OUT) {
        Refusal::CredentialsExpired
    } else if says(said, &["Cost Explorer", "not enabled", "OptInRequired"]) {
        // Before a denial: Cost Explorer off is refused as `AccessDeniedException`.
        Refusal::NotEnabled
    } else if says(said, &["AccessDenied", "UnauthorizedOperation", "not authorized"]) {
        Refusal::Denied
    } else {
        Refusal::Other
    }
}

/// Who `account`'s credentials are ([`WHO_AM_I`]): the identity's ARN.
///
/// # Errors
///
/// [`Failure`]: the CLI (or Docker) isn't there, or AWS refused ([`Refusal::CredentialsExpired`]
/// for a sign-in run out or missing, or keys rejected).
pub async fn identity(cx: &Context, account: &Account) -> Result<String, Failure> {
    account.ask(cx, &WHO_AM_I).await.map(|out| line(&out))
}

/// The profiles the host's AWS CLI has (`aws configure list-profiles`), in its order.
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed, or failed.
pub async fn profiles(cx: &Context) -> Result<Vec<String>, Failure> {
    super::ask(cx, CLI, &["configure", "list-profiles"], refusal).await.map(|out| lines(&out))
}

/// The non-blank lines of an answer, trimmed.
fn lines(out: &str) -> Vec<String> {
    out.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect()
}

/// `account`'s spend this month so far, in USD, as Cost Explorer has it (unblended).
///
/// # Errors
///
/// [`Failure`]: the CLI isn't there, AWS refused ([`Refusal::NotEnabled`]: Cost Explorer isn't
/// enabled), or its answer isn't a finite number.
pub async fn month_to_date_cost(cx: &Context, account: &Account) -> Result<f64, Failure> {
    let period = month_period(SystemTime::now());
    let out = account.ask(cx, &cost_args(&period)).await?;
    spend(&out)
}

/// `aws ce get-cost-and-usage` for `period`, printing just the amount.
fn cost_args(period: &str) -> [&str; 12] {
    [
        "ce",
        "get-cost-and-usage",
        "--time-period",
        period,
        "--granularity",
        "MONTHLY",
        "--metrics",
        "UnblendedCost",
        "--query",
        "ResultsByTime[0].Total.UnblendedCost.Amount",
        "--output",
        "text",
    ]
}

/// Cost Explorer's amount (its text output) as a number: a finite one (`f64::from_str` reads
/// `NaN` and `inf` too, neither a spend).
fn spend(out: &str) -> Result<f64, Failure> {
    out.trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| unreadable(CLI, format!("the spend isn't a number: {:?}", out.trim())))
}

/// Cost Explorer's period for the month of `now` so far: `Start` the 1st, `End` tomorrow (an
/// exclusive end, so today's spend is in), UTC.
fn month_period(now: SystemTime) -> String {
    let day = |at: SystemTime| {
        isothrum_runner::rfc3339(at)
            .split_once('T')
            .map(|(date, _)| date.to_owned())
            .unwrap_or_default()
    };
    let today = day(now);
    let month = today.rsplit_once('-').map_or(today.as_str(), |(month, _)| month);
    format!("Start={month}-01,End={}", day(now + Duration::from_secs(86_400)))
}

/// Whether the account may create EC2 instances, as a dry run of one said.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LaunchPermission {
    /// AWS said the instance would have been created (`DryRunOperation`).
    Allowed,
    /// AWS checked the permission, then stopped at an error about the probe itself (its image,
    /// the account's network): the permission is there.
    AllowedThen {
        /// The error AWS stopped at (`InvalidAMIID.NotFound`, `VPCIdNotSpecified`...).
        code: String,
    },
    /// `ec2:RunInstances` is denied (`UnauthorizedOperation`).
    Denied,
    /// AWS stopped before it looked at the permission (throttled, a sign-in run out, a malformed
    /// parameter, AWS itself), or gave no answer: what it said.
    Unconfirmed(Failure),
    /// The dry run printed nothing and exited 0: it says nothing.
    NoAnswer,
}

/// Dry-runs creating a [`PROBE_INSTANCE_TYPE`] from the region's latest Amazon Linux image
/// ([`LATEST_AMAZON_LINUX`], else [`UNKNOWN_IMAGE`]): creates nothing, and tells whether the
/// account may create instances.
pub async fn launch_permission(cx: &Context, account: &Account) -> LaunchPermission {
    let latest = account
        .ask(
            cx,
            &[
                "ssm",
                "get-parameter",
                "--name",
                LATEST_AMAZON_LINUX,
                "--query",
                "Parameter.Value",
                "--output",
                "text",
            ],
        )
        .await;
    let image = probe_image(latest.ok().as_deref());
    let dry = account
        .ask(
            cx,
            &[
                "ec2",
                "run-instances",
                "--dry-run",
                "--instance-type",
                PROBE_INSTANCE_TYPE,
                "--image-id",
                &image,
                "--output",
                "text",
            ],
        )
        .await;
    match dry {
        Ok(_) => LaunchPermission::NoAnswer,
        Err(failure) => permission(failure),
    }
}

/// The image a dry run uses: the one SSM printed when it is an image id, else [`UNKNOWN_IMAGE`].
fn probe_image(latest: Option<&str>) -> String {
    match latest.map(str::trim) {
        Some(id) if id.starts_with("ami-") && id.len() > 4 && !id.contains(char::is_whitespace) => id.to_owned(),
        _ => UNKNOWN_IMAGE.to_owned(),
    }
}

/// What a refused dry run says of the permission, by AWS's error code.
fn permission(failure: Failure) -> LaunchPermission {
    let code = match &failure {
        Failure::Refused { said, .. } => error_code(said).map(str::to_owned),
        _ => None,
    };
    match code.as_deref() {
        Some("DryRunOperation") => LaunchPermission::Allowed,
        Some("UnauthorizedOperation") => LaunchPermission::Denied,
        Some(code) if !before_authorization(code) => LaunchPermission::AllowedThen { code: code.to_owned() },
        _ => LaunchPermission::Unconfirmed(failure),
    }
}

/// The error code of a failed AWS CLI call: `Code` in "An error occurred (Code) when calling…".
#[must_use]
pub fn error_code(said: &str) -> Option<&str> {
    let (_, after) = said.split_once("An error occurred (")?;
    after.split_once(')').map(|(code, _)| code)
}

/// Whether AWS raises error `code` before it authorizes a request, so it says nothing of the
/// permission: bad or expired credentials, throttling, a malformed or missing parameter, AWS
/// itself, an account not yet usable. Every other code comes after authorization.
#[must_use]
pub fn before_authorization(code: &str) -> bool {
    const BEFORE: &[&str] = &[
        "AuthFailure",
        "ExpiredToken",
        "InvalidClientTokenId",
        "SignatureDoesNotMatch",
        "IncompleteSignature",
        "RequestExpired",
        "RequestLimitExceeded",
        "Throttling",
        "ServiceUnavailable",
        "InternalError",
        "InternalFailure",
        "Unavailable",
        "ValidationError",
        "MissingParameter",
        "InvalidParameter",
        "UnknownParameter",
        "OptInRequired",
        "PendingVerification",
        "AccessDenied",
        "Blocked",
    ];
    BEFORE.iter().any(|b| code.starts_with(b)) || code.ends_with(".Malformed")
}

#[cfg(test)]
mod tests;
