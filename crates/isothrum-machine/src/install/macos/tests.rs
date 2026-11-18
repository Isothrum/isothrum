use super::*;

const BREW: &str = "/opt/homebrew/bin/brew";
const HOMEBREW: Option<Packages> = Some(Packages::Homebrew(BREW));
const PROMPT: Elevation = Elevation::Prompt;

/// Each step of `steps` as its program.
fn programs(steps: &[Step]) -> Vec<&str> {
    steps.iter().map(|s| s.program.as_str()).collect()
}

#[test]
fn macos_installs_through_homebrew_and_the_vendors_installers() {
    assert_eq!(install(Dependency::Docker, None, Arch::Arm64, PROMPT).unwrap_err(), Refusal::NoHomebrew);
    assert!(install(Dependency::VmwareUtility, None, Arch::Arm64, PROMPT).is_ok(), "HashiCorp's own package");
    for dep in Dependency::ALL {
        let plan = install(dep, HOMEBREW, Arch::Arm64, PROMPT);
        match dep {
            Dependency::Libvirt | Dependency::Wsl | Dependency::Hyperv => {
                assert_eq!(
                    plan.unwrap_err(),
                    Refusal::NotHere {
                        dependency: dep,
                        os: HostOs::Macos
                    }
                );
            }
            _ => assert!(!plan.unwrap().is_empty(), "{dep:?}"),
        }
    }
    // The apps and Vagrant: one step each, from their vendor's file, as an administrator; Docker
    // Desktop's says it accepts Docker's licence.
    for dep in [Dependency::Docker, Dependency::Vagrant, Dependency::Virtualbox, Dependency::Utm] {
        let plan = install(dep, HOMEBREW, Arch::Arm64, PROMPT).unwrap();
        let Some(Note::InstallingFromVendor { dependency, accepts }) = plan[0].note else {
            panic!("{dep:?}: {:?}", plan[0].note)
        };
        assert_eq!((programs(&plan), dependency), (vec!["sh"], dep));
        assert_eq!(accepts.is_some(), dep == Dependency::Docker, "{dep:?}");
    }
    let azure = install(Dependency::Azurecli, HOMEBREW, Arch::Arm64, PROMPT).unwrap();
    assert_eq!(
        azure.iter().map(|s| s.note).collect::<Vec<_>>(),
        [None, Some(Note::TrustingAzureTap), Some(Note::AzureCliCask)]
    );
    assert!(azure[2].args.contains(&"azure-cli-preview".to_owned()));
    // QEMU is one shell step: a best-effort Homebrew update that never aborts the install (`|| …`),
    // then the install itself; never a second, fatal `brew update` step.
    let qemu = install(Dependency::Qemu, HOMEBREW, Arch::Arm64, PROMPT).unwrap();
    let script = qemu[0].args.join(" ");
    assert_eq!(programs(&qemu), ["sh"]);
    assert!(script.contains("update ||") && script.contains("install qemu"), "{script}");
    assert!(qemu.iter().all(|s| s.args.get(1).is_none_or(|text| crate::testing::sh_parses(text))));
}

#[test]
fn every_macos_tool_has_a_way_out_and_only_homebrews_need_it() {
    assert_eq!(uninstall(Dependency::Terraform, None, None).unwrap_err(), Refusal::HomebrewNeeded);
    for dep in [
        Dependency::VmwareUtility,
        Dependency::Vagrant,
        Dependency::Docker,
        Dependency::Utm,
        Dependency::Awscli,
    ] {
        assert!(uninstall(dep, None, None).is_ok(), "{dep:?}");
    }
    for dep in Dependency::ALL
        .into_iter()
        .filter(|d| !matches!(d, Dependency::Wsl | Dependency::Libvirt | Dependency::Hyperv))
    {
        assert!(!uninstall(dep, HOMEBREW, Some("/x/VirtualBox.dmg")).unwrap().is_empty(), "{dep:?}");
    }
    assert_eq!(uninstall(Dependency::Virtualbox, HOMEBREW, None).unwrap_err(), Refusal::NoVirtualboxImage);
    let vagrant = uninstall(Dependency::Vagrant, None, None).unwrap();
    assert_eq!(
        (programs(&vagrant), vagrant[0].note),
        (vec!["osascript"], Some(Note::Removing(Dependency::Vagrant)))
    );
    assert!(vagrant[0].args[1].ends_with("with administrator privileges"), "{:?}", vagrant[0].args);
    assert!(
        vagrant[0].args[1].contains("with prompt \"Remove Vagrant: macOS needs your password to remove it.\""),
        "{:?}",
        vagrant[0].args
    );
}

#[test]
fn virtualbox_is_removed_by_its_own_uninstaller_from_homebrews_image_only() {
    let steps = uninstall(
        Dependency::Virtualbox,
        HOMEBREW,
        Some("/Users/ada/Library/Caches/Homebrew/downloads/x--VirtualBox-7.1.4.dmg"),
    )
    .unwrap();
    let script = &steps[0].args[1];
    assert!(
        script.contains("hdiutil attach -nobrowse -noautoopen '/Users/ada/Library/Caches/Homebrew/downloads/x--VirtualBox-7.1.4.dmg'"),
        "{script}"
    );
    assert!(!script.contains("/Volumes/VirtualBox"), "never whatever is mounted under that name: {script}");
    assert_eq!(virtualbox_image_from("/c/VirtualBox-7.1.4.dmg\n").as_deref(), Some("/c/VirtualBox-7.1.4.dmg"));
    assert_eq!(virtualbox_image_from("/c/VirtualBox.pkg"), None);
    assert_eq!(virtualbox_image_from("/c/it's.dmg"), None, "a quote the script can't hold");
    assert_eq!(virtualbox_image_from(""), None);
}

#[tokio::test(start_paused = true)]
async fn the_image_is_fetched_again_then_read_from_homebrews_cache() {
    use isothrum_runner::fake::{FakeProcesses, Reply};
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("VirtualBox-7.1.4.dmg");
    let cache = Reply::ok(&image.display().to_string());
    let tools = FakeProcesses::script()
        .expect_times("/b/brew fetch --cask virtualbox", 2, Reply::ok("==> Downloading"))
        .expect_times("/b/brew --cache --cask virtualbox", 2, cache)
        .build();
    let cx = Context::new(std::sync::Arc::new(tools), crate::testing::linux_host(dir.path()));
    let mut lines = Vec::new();
    let missing = virtualbox_image(&cx, "/b/brew", &mut |l: &str| lines.push(l.to_owned())).await;
    assert_eq!(missing, Err(ImageMissing::NotThere(image.display().to_string())));
    std::fs::write(&image, "").unwrap();
    assert_eq!(virtualbox_image(&cx, "/b/brew", &mut |_: &str| {}).await, Ok(image.display().to_string()));
    assert_eq!(lines, ["$ /b/brew fetch --cask virtualbox", "==> Downloading"]);
}

#[test]
fn each_cpu_gets_a_vendors_build_where_there_is_one_and_an_intel_mac_is_told_what_it_builds() {
    for arch in [Arch::Amd64, Arch::Arm64] {
        // The AWS CLI: Amazon's package on both CPUs, never Homebrew's formula (which an Intel Mac
        // builds from source, and failed to).
        let aws = install(Dependency::Awscli, HOMEBREW, arch, PROMPT).unwrap();
        let script = aws[0].args.join(" ");
        assert_eq!(
            aws[0].note,
            Some(Note::InstallingFromVendor {
                dependency: Dependency::Awscli,
                accepts: None
            }),
            "{arch:?}"
        );
        assert!(
            script.contains("https://awscli.amazonaws.com/AWSCLIV2.pkg") && !script.contains(BREW),
            "{arch:?}: {script}"
        );
        // Terraform: HashiCorp's tap, whose formula installs HashiCorp's own build on either CPU.
        let terraform = install(Dependency::Terraform, HOMEBREW, arch, PROMPT).unwrap();
        assert_eq!(terraform[0].args, ["install", "hashicorp/tap/terraform"], "{arch:?}");
        // QEMU: no vendor's build, Homebrew's formula; built from source on Intel, which it says.
        let qemu = install(Dependency::Qemu, HOMEBREW, arch, PROMPT).unwrap();
        let said = match arch {
            Arch::Amd64 => Note::BuildsFromSource(Dependency::Qemu),
            Arch::Arm64 => Note::QemuUpdatingHomebrewFirst,
        };
        assert_eq!(qemu[0].note, Some(said), "{arch:?}");
    }
}

#[test]
fn the_aws_cli_is_removed_as_amazons_package_installed_it_and_nothing_else() {
    let steps = uninstall(Dependency::Awscli, None, None).unwrap();
    assert_eq!((programs(&steps), steps[0].note), (vec!["osascript"], Some(Note::Removing(Dependency::Awscli))));
    // Its folder and receipt; a link in /usr/local/bin only where it points into that folder
    // (Homebrew's own `aws`, there on an Intel Mac, stays).
    let script = AWS_CLI_REMOVAL;
    assert!(crate::testing::sh_parses(script), "{script}");
    assert!(
        script.contains("rm -rf /usr/local/aws-cli") && script.contains("pkgutil --forget com.amazon.aws.cli2"),
        "{script}"
    );
    assert!(script.contains("/usr/local/aws-cli/*) rm -f"), "{script}");
    assert!(!script.contains("brew"), "{script}");
}

#[cfg(unix)]
#[test]
fn the_aws_cli_removal_leaves_a_link_it_didnt_make() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink("/usr/local/aws-cli/aws", bin.join("aws")).unwrap();
    std::os::unix::fs::symlink("../Cellar/awscli/2.0/bin/aws_completer", bin.join("aws_completer")).unwrap();
    let script = AWS_CLI_REMOVAL
        .replace("/usr/local/bin", &bin.display().to_string())
        .replace("rm -rf /usr/local/aws-cli && (pkgutil --forget com.amazon.aws.cli2 || true)", "true");
    let out = crate::testing::sh(&script, &[]);
    assert!(out.status.success());
    assert!(bin.join("aws").symlink_metadata().is_err(), "Amazon's link removed");
    assert!(bin.join("aws_completer").symlink_metadata().is_ok(), "Homebrew's link kept");
}
