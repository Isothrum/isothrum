use std::sync::Arc;

use isothrum_runner::fake::{FakeProcesses, Reply};
use proptest::prelude::*;

use super::*;
use crate::host::LinuxPackages;
use crate::testing::linux_host;

/// The check on a Linux host with `packages`, its tools answered by `tools`.
fn context(tools: FakeProcesses, packages: Option<Packages>) -> Context {
    let mut host = linux_host(std::path::Path::new("/nonexistent/isothrum-machine-test"));
    host.packages = packages;
    Context::new(Arc::new(tools), host)
}

#[test]
fn a_version_is_the_first_non_blank_line_trimmed() {
    assert_eq!(first_line("Homebrew 4.6.3 \nHomebrew/core\n").as_deref(), Some("Homebrew 4.6.3"));
    assert_eq!(first_line(""), None);
    assert_eq!(first_line("\n  \nVagrant 2.4.9\n").as_deref(), Some("Vagrant 2.4.9"));
}

#[test]
fn a_version_on_windows_line_endings_or_without_a_newline_has_no_stray_characters() {
    assert_eq!(first_line("Vagrant 2.4.3\r\n\r\n").as_deref(), Some("Vagrant 2.4.3"));
    assert_eq!(first_line("Terraform v1.9.8").as_deref(), Some("Terraform v1.9.8"));
    assert_eq!(
        first_line("\tDocker version 27.3.1, build x\t\r\n").as_deref(),
        Some("Docker version 27.3.1, build x")
    );
    assert_eq!(first_line("\r\n"), None);
}

/// A tool that fails its probe (a wedged daemon, a tool refusing `--version`) or isn't installed
/// reads as not installed with no version.
#[tokio::test(start_paused = true)]
async fn a_probe_that_fails_or_finds_no_tool_reads_as_not_installed() {
    let tools = FakeProcesses::script()
        .expect("docker --version", Reply::fail(1, "Cannot connect to the Docker daemon"))
        .expect("vagrant --version", Reply::missing())
        .expect("terraform version", Reply::ok("Terraform v1.9.8\n"))
        .expect("aws --version", Reply::ok("aws-cli/2.17.0 Python/3.12"))
        .expect("az version --output json", Reply::missing())
        .expect("gcloud --version", Reply::ok("\nGoogle Cloud SDK 500.0.0\nbq 2.1"))
        .build();
    let cx = context(tools, None);
    assert_eq!(version(&cx, probes::DOCKER).await, Tool::MISSING);
    assert_eq!(version(&cx, probes::VAGRANT).await, Tool::MISSING);
    assert_eq!(version(&cx, probes::TERRAFORM).await, Tool::installed(Some("Terraform v1.9.8".into())));
    let clouds = cloud_clis(&cx).await;
    assert_eq!(clouds.aws.version.as_deref(), Some("aws-cli/2.17.0 Python/3.12"));
    assert_eq!(
        (clouds.azure, clouds.gcloud.version.as_deref()),
        (Tool::MISSING, Some("Google Cloud SDK 500.0.0"))
    );
}

#[tokio::test(start_paused = true)]
async fn a_tool_past_the_probes_time_reads_as_not_installed() {
    let host = linux_host(std::path::Path::new("/nonexistent/isothrum-machine-test"));
    let cx = Context::new(Arc::new(crate::testing::Stuck), host);
    assert_eq!(version(&cx, probes::VAGRANT).await, Tool::MISSING);
}

#[tokio::test(start_paused = true)]
async fn a_package_manager_that_fails_its_version_is_not_installed() {
    let apt = Some(Packages::Linux(LinuxPackages::Apt));
    let broken = context(FakeProcesses::script().expect("apt-get --version", Reply::fail(1, "broken")).build(), apt);
    let found = package_manager(&broken).await;
    assert_eq!((found.name.as_str(), found.installed, found.version), ("apt", false, None));
    let working = context(
        FakeProcesses::script()
            .expect("apt-get --version", Reply::ok("apt 2.9.8 (amd64)\nSupported modules:"))
            .build(),
        apt,
    );
    assert_eq!(package_manager(&working).await.version.as_deref(), Some("apt 2.9.8 (amd64)"));
}

#[test]
fn a_host_without_a_package_manager_names_its_oss_usual_one_not_installed() {
    let missing = package_manager_of(None, crate::HostOs::Macos, None);
    assert_eq!((missing.name.as_str(), missing.installed, missing.version), ("Homebrew", false, None));
    let empty = package_manager_of(Some(Packages::Winget), crate::HostOs::Windows, Some("\r\n"));
    assert_eq!((empty.name.as_str(), empty.installed, empty.version), ("winget", true, None));
    let json = serde_json::to_value(Tool::installed(Some("1".into()))).unwrap();
    assert_eq!(json, serde_json::json!({ "installed": true, "version": "1" }));
}

proptest! {
    #[test]
    fn first_line_is_the_first_non_blank_line_trimmed(out in "(?s).{0,120}") {
        let want = out.lines().map(str::trim).find(|l| !l.is_empty());
        let got = first_line(&out);
        prop_assert_eq!(got.as_deref(), want);
        if let Some(l) = got {
            prop_assert!(!l.is_empty() && !l.contains('\n') && l == l.trim());
        }
    }

    #[test]
    fn a_banner_of_blank_lines_is_skipped(
        blank in proptest::collection::vec("[ \t\r]{0,3}", 0..4),
        version in "[!-~][ -~]{0,20}[!-~]|[!-~]",
        rest in "(?s).{0,40}",
    ) {
        let out = format!("{}{version}\n{rest}", blank.iter().flat_map(|b| [b.as_str(), "\n"]).collect::<String>());
        prop_assert_eq!(first_line(&out), Some(version));
    }
}
