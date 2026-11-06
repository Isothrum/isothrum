//! The system check: which tools, engines, hypervisors and package manager this machine has, and
//! what the providers say it can run. Every probe is timed and forgiving — a wedged daemon or a
//! held Vagrant lock reads as "not installed" rather than hanging or failing the whole check, so a
//! report is always produced.

pub mod docker;
pub mod wsl;

use std::time::Duration;

use isothrum_runner::ProcessRunner;
use serde::Serialize;

use crate::dirs::Dirs;
use crate::exec;
use crate::providers::{self, ProviderStatus};

/// How long a read-only version or status probe may take before a stuck tool is given up on.
const STATUS_TIMEOUT: Duration = Duration::from_secs(25);

/// Whether a tool is installed, and its version when it is.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    /// It answered its version query.
    pub installed: bool,
    /// The first line of that answer, when it gave one.
    pub version: Option<String>,
}

/// The OS package manager the one-click installs rely on (Homebrew, winget or apt).
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PkgManager {
    /// Its name, as this OS calls it.
    pub name: String,
    /// It is installed.
    pub installed: bool,
    /// The first line of its version output, e.g. "Homebrew 4.6.3", when installed.
    pub version: Option<String>,
}

/// The cloud provider CLIs, for the cloud setup.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CloudClis {
    /// The AWS CLI.
    pub aws: Tool,
    /// The Azure CLI.
    pub azure: Tool,
    /// The Google Cloud CLI.
    pub gcloud: Tool,
}

/// Everything the system check found: the tools, the Docker engines, the VM providers and the
/// package manager, with the machine's OS and architecture.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SystemReport {
    /// This machine's OS (`macos`, `windows`, `linux`).
    pub os: &'static str,
    /// Its CPU architecture (`x86_64`, `aarch64`).
    pub arch: &'static str,
    /// The OS package manager.
    pub pkg_manager: PkgManager,
    /// The Docker CLI.
    pub docker: Tool,
    /// The Docker daemon answers for this account (an engine is started and usable).
    pub docker_running: bool,
    /// The daemon runs but refused this account (not in the right group yet): starting the engine
    /// won't help. `docker_denied_hint` says what will.
    pub docker_denied: bool,
    /// What to do about a denied daemon, in plain words, when `docker_denied`.
    pub docker_denied_hint: Option<String>,
    /// Which Docker-compatible engine the CLI uses, when one answers.
    pub docker_engine: Option<&'static str>,
    /// Every engine whose Docker context answers right now; several can run side by side.
    pub docker_engines_running: Vec<&'static str>,
    /// The Docker Compose plugin.
    pub docker_compose: Tool,
    /// The Vagrant CLI.
    pub vagrant: Tool,
    /// A local Terraform binary (preferred for provisioning; the container is the fallback).
    pub terraform: Tool,
    /// VMware's OVF Tool, which the ESXi Vagrant plugin uses to upload VMs.
    pub ovftool: Tool,
    /// The cloud provider CLIs.
    pub cloud_clis: CloudClis,
    /// The Vagrant providers (local hypervisors and remote hosts such as ESXi), each with whether
    /// it can run here.
    pub vm_providers: Vec<ProviderStatus>,
    /// Windows: whether WSL 2 is on, waits for a restart, or is missing (`None` elsewhere).
    pub wsl: Option<wsl::WslReport>,
}

/// Checks the whole machine through `runner`, using `dirs` for anything that reads a file. Every
/// probe runs concurrently and forgives its own failure, so one stuck or missing tool never sinks
/// the report.
pub async fn system_check(runner: &dyn ProcessRunner, dirs: &Dirs) -> SystemReport {
    let (docker, docker_compose, vagrant, daemon, aws, azure, gcloud, terraform) = tokio::join!(
        probe(runner, "docker", &["--version"]),
        probe(runner, "docker", &["compose", "version", "--short"]),
        probe(runner, "vagrant", &["--version"]),
        docker::daemon_os(runner),
        probe(runner, "aws", &["--version"]),
        probe_azure(runner),
        probe(runner, "gcloud", &["--version"]),
        probe(runner, "terraform", &["version"]),
    );
    let (vm_providers, docker_engines_running, ovftool, pkg_manager) = tokio::join!(
        providers::detect(runner, vagrant.installed),
        docker::running_engines(runner),
        probe(runner, "ovftool", &["--version"]),
        package_manager(runner),
    );
    let docker_denied = daemon.as_ref().err().is_some_and(docker::access_denied);
    let docker_engine = match &daemon {
        Ok(os) => Some(docker::engine_in_use(runner, os.trim()).await),
        Err(_) => None,
    };
    SystemReport {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        pkg_manager,
        docker,
        docker_running: daemon.is_ok(),
        docker_denied,
        docker_denied_hint: docker_denied.then(docker::denied_hint),
        docker_engine,
        docker_engines_running,
        docker_compose,
        vagrant,
        terraform,
        ovftool,
        cloud_clis: CloudClis { aws, azure, gcloud },
        vm_providers,
        wsl: wsl::report(runner, dirs),
    }
}

/// A tool's version, timed: a wedged daemon or a held Vagrant lock must not hang the check.
async fn probe(runner: &dyn ProcessRunner, program: &str, args: &[&str]) -> Tool {
    match exec::within(program, STATUS_TIMEOUT, exec::run(runner, program, args, None)).await {
        Ok(out) => Tool { installed: true, version: first_line(&out) },
        Err(_) => Tool { installed: false, version: None },
    }
}

/// `az version` prints JSON (`{"azure-cli": "2.66.0", …}`); the version is read from it so no
/// quoted `--query` has to pass through cmd.exe (`az` is a batch file on Windows).
async fn probe_azure(runner: &dyn ProcessRunner) -> Tool {
    match exec::within("az", STATUS_TIMEOUT, exec::run(runner, "az", &["version", "--output", "json"], None)).await {
        Ok(out) => Tool { installed: true, version: azure_version(&out) },
        Err(_) => Tool { installed: false, version: None },
    }
}

/// The Azure CLI version from its JSON, when it is text.
fn azure_version(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    value.get("azure-cli")?.as_str().map(str::to_owned)
}

/// The first non-blank line, trimmed: a tool whose output opens with a banner's blank line read as
/// installed with an empty version without this.
fn first_line(out: &str) -> Option<String> {
    out.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_owned)
}

/// This OS's package manager, with its version when it is installed.
async fn package_manager(runner: &dyn ProcessRunner) -> PkgManager {
    pkg_manager(package_manager_version(runner).await)
}

/// This OS's package manager, given its `--version` output when installed.
fn pkg_manager(version: Option<String>) -> PkgManager {
    let name = if cfg!(target_os = "windows") {
        "winget"
    } else if cfg!(target_os = "linux") {
        "apt"
    } else {
        "Homebrew"
    };
    let installed = version.is_some();
    let version = version.and_then(|v| first_line(&v)).filter(|v| !v.is_empty());
    PkgManager { name: name.to_owned(), installed, version }
}

/// `--version` of this OS's package manager, when it is installed.
async fn package_manager_version(runner: &dyn ProcessRunner) -> Option<String> {
    let (program, args): (&str, &[&str]) = if cfg!(target_os = "windows") {
        ("winget", &["--version"])
    } else if cfg!(target_os = "linux") {
        ("apt-get", &["--version"])
    } else {
        ("brew", &["--version"])
    };
    exec::run(runner, program, args, None).await.ok()
}

#[cfg(test)]
mod tests {
    use isothrum_runner::process::fake::{FakeProcesses, Reply};

    use super::*;

    #[test]
    fn a_version_is_the_first_non_blank_line_trimmed() {
        assert_eq!(first_line("Homebrew 4.6.3 \nHomebrew/core\n").as_deref(), Some("Homebrew 4.6.3"));
        assert_eq!(first_line(""), None);
        assert_eq!(first_line("\n  \nVagrant 2.4.9\n").as_deref(), Some("Vagrant 2.4.9"));
        assert_eq!(first_line("Vagrant 2.4.3\r\n\r\n").as_deref(), Some("Vagrant 2.4.3"));
        assert_eq!(first_line("Terraform v1.9.8").as_deref(), Some("Terraform v1.9.8"));
        assert_eq!(first_line("\r\n"), None);
    }

    #[test]
    fn the_azure_cli_version_comes_from_its_json() {
        let out = "{\n  \"azure-cli\": \"2.90.0\",\n  \"azure-cli-core\": \"2.90.0\",\n  \"extensions\": {}\n}\n";
        assert_eq!(azure_version(out).as_deref(), Some("2.90.0"));
        assert_eq!(azure_version("WARNING: something\n{}"), None);
        assert_eq!(azure_version("{\"azure-cli\": 2}"), None);
        assert_eq!(azure_version("not json"), None);
        assert_eq!(azure_version(""), None);
    }

    #[test]
    fn the_package_manager_reports_its_first_version_line() {
        let pm = pkg_manager(Some("Homebrew 4.6.3\nHomebrew/core\n".to_owned()));
        assert!(pm.installed);
        assert_eq!(pm.version.as_deref(), Some("Homebrew 4.6.3"));
        assert_ne!(pm.name, "");
        let empty = pkg_manager(Some(String::new()));
        assert!(empty.installed && empty.version.is_none());
        let missing = pkg_manager(None);
        assert!(!missing.installed && missing.version.is_none());
        let json = serde_json::to_value(Tool { installed: true, version: Some("1".to_owned()) }).unwrap();
        assert_eq!(json, serde_json::json!({ "installed": true, "version": "1" }));
    }

    #[tokio::test]
    async fn a_probe_that_fails_or_finds_no_tool_reads_as_not_installed() {
        let fake = FakeProcesses::new(|call| match call.program.as_str() {
            "docker" => Reply::fail(1, "Cannot connect to the Docker daemon"),
            "vagrant" | "az" => Reply::missing(),
            _ => Reply::ok("Terraform v1.9.8\n"),
        });
        let failing = probe(&fake, "docker", &["--version"]).await;
        assert!(!failing.installed && failing.version.is_none());
        assert!(!probe(&fake, "vagrant", &["--version"]).await.installed);
        let found = probe(&fake, "terraform", &["version"]).await;
        assert_eq!((found.installed, found.version.as_deref()), (true, Some("Terraform v1.9.8")));
        assert!(!probe_azure(&fake).await.installed);
    }

    #[tokio::test]
    async fn the_report_names_each_tool_and_survives_one_that_fails() {
        let fake = FakeProcesses::new(|call| match call.program.as_str() {
            "docker" => Reply::fail(1, "Cannot connect to the Docker daemon"),
            "vagrant" | "az" | "aws" | "gcloud" => Reply::missing(),
            _ => Reply::ok("v1.0\n"),
        });
        let report = system_check(&fake, &Dirs::default()).await;
        assert!(!report.docker.installed && !report.docker_running && !report.docker_denied);
        assert!(report.docker_denied_hint.is_none());
        assert!(!report.vagrant.installed && !report.cloud_clis.azure.installed);
        assert_eq!(report.os, std::env::consts::OS);
        assert!(report.wsl.is_none());
    }
}
