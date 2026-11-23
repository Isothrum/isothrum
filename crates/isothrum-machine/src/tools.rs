//! The command-line tools a machine check reports: whether each answers its version probe, and the
//! version it printed. A tool that can't be started, fails its probe or runs out of the probe's
//! time reads as not installed, with no version: the check goes on, it never fails for one tool.

use serde::{Deserialize, Serialize};

use crate::context::Context;
use crate::host::{Packages, usual_packages};

pub mod ovftool;

/// A command-line tool as the check found it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    /// Whether it answered its version probe.
    pub installed: bool,
    /// The first line of what it printed, when it answered.
    pub version: Option<String>,
}

impl Tool {
    /// A tool that isn't installed.
    pub const MISSING: Tool = Tool {
        installed: false,
        version: None,
    };

    /// An installed tool, at `version` when it said one.
    #[must_use]
    pub fn installed(version: Option<String>) -> Tool {
        Tool { installed: true, version }
    }
}

/// The version probes of the tools a check reports, each a program and the arguments it answers
/// its version with.
pub mod probes {
    /// The Docker CLI.
    pub const DOCKER: (&str, &[&str]) = ("docker", &["--version"]);
    /// Docker Compose, the Docker CLI's plugin.
    pub const COMPOSE: (&str, &[&str]) = ("docker", &["compose", "version", "--short"]);
    /// Vagrant.
    pub const VAGRANT: (&str, &[&str]) = ("vagrant", &["--version"]);
    /// Terraform.
    pub const TERRAFORM: (&str, &[&str]) = ("terraform", &["version"]);
    /// The AWS CLI.
    pub const AWS: (&str, &[&str]) = ("aws", &["--version"]);
    /// The Azure CLI: `az version` prints JSON, its version read from it ([`super::azure_cli`]).
    /// A `--query` would need quotes, which can't pass through `cmd.exe` (`az` is a `.cmd`).
    pub const AZURE: (&str, &[&str]) = ("az", &["version", "--output", "json"]);
    /// The Google Cloud CLI.
    pub const GCLOUD: (&str, &[&str]) = ("gcloud", &["--version"]);
}

/// `program args`, a version probe: installed when it exits 0, its version the first line it
/// printed ([`first_line`]).
pub async fn version(cx: &Context, (program, args): (&str, &[&str])) -> Tool {
    match cx.read_ok(program, args).await {
        Ok(out) => Tool::installed(first_line(&out)),
        Err(_) => Tool::MISSING,
    }
}

/// The first non-blank line, trimmed: a tool whose output opens with a blank line (a banner's
/// spacing) or ends its lines with `\r\n` reads with its version, not an empty one.
#[must_use]
pub fn first_line(out: &str) -> Option<String> {
    out.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_owned)
}

/// The Azure CLI ([`probes::AZURE`]), its version the `azure-cli` entry of the JSON it prints.
pub async fn azure_cli(cx: &Context) -> Tool {
    let (program, args) = probes::AZURE;
    match cx.read_ok(program, args).await {
        Ok(out) => Tool::installed(isothrum_runner::azure_cli_version(&out)),
        Err(_) => Tool::MISSING,
    }
}

/// The cloud providers' command-line tools.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct CloudClis {
    /// The AWS CLI (`aws`).
    pub aws: Tool,
    /// The Azure CLI (`az`).
    pub azure: Tool,
    /// The Google Cloud CLI (`gcloud`).
    pub gcloud: Tool,
}

/// Which cloud CLIs answer here, asked side by side.
pub async fn cloud_clis(cx: &Context) -> CloudClis {
    let (aws, azure, gcloud) = tokio::join!(version(cx, probes::AWS), azure_cli(cx), version(cx, probes::GCLOUD));
    CloudClis { aws, azure, gcloud }
}

/// The package manager a setup installs through, as the check found it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PackageManager {
    /// Its name: `Homebrew`, `winget`, `apt` or `dnf`.
    pub name: String,
    /// Whether it answered its version probe.
    pub installed: bool,
    /// The first line of its version (`Homebrew 4.6.3`), when it said one.
    pub version: Option<String>,
}

/// The context's host's package manager ([`crate::Host::packages`]) and whether it answers its
/// `--version`; when the host has none, the one its OS usually has ([`usual_packages`]), not
/// installed.
pub async fn package_manager(cx: &Context) -> PackageManager {
    let answer = match cx.host.packages {
        Some(packages) => cx.read_ok(packages.program(), &["--version"]).await.ok(),
        None => None,
    };
    package_manager_of(cx.host.packages, cx.host.os, answer.as_deref())
}

/// The package manager `packages` (else `os`'s usual one), given its `--version` output when it
/// answered.
fn package_manager_of(packages: Option<Packages>, os: crate::HostOs, answer: Option<&str>) -> PackageManager {
    PackageManager {
        name: packages.map_or(usual_packages(os), Packages::name).to_owned(),
        installed: answer.is_some(),
        version: answer.and_then(first_line),
    }
}

#[cfg(test)]
mod tests;
