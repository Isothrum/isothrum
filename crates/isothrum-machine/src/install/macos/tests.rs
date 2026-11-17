use super::*;

const BREW: &str = "/opt/homebrew/bin/brew";
const HOMEBREW: Option<Packages> = Some(Packages::Homebrew(BREW));

/// Each step of `steps` as its program.
fn programs(steps: &[Step]) -> Vec<&str> {
    steps.iter().map(|s| s.program.as_str()).collect()
}

#[test]
fn macos_installs_through_homebrew_and_the_vendors_installers() {
    assert_eq!(install(Dependency::Docker, None, Arch::Arm64).unwrap_err(), Refusal::NoHomebrew);
    assert!(install(Dependency::VmwareUtility, None, Arch::Arm64).is_ok(), "HashiCorp's own package");
    for dep in Dependency::ALL {
        let plan = install(dep, HOMEBREW, Arch::Arm64);
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
    let docker = install(Dependency::Docker, HOMEBREW, Arch::Arm64).unwrap();
    assert_eq!((programs(&docker), docker[0].note), (vec!["sh"], Some(Note::Downloading(Dependency::Docker))));
    let azure = install(Dependency::Azurecli, HOMEBREW, Arch::Arm64).unwrap();
    assert_eq!(
        azure.iter().map(|s| s.note).collect::<Vec<_>>(),
        [None, Some(Note::TrustingAzureTap), Some(Note::AzureCliCask)]
    );
    assert!(azure[2].args.contains(&"azure-cli-preview".to_owned()));
    // QEMU is one shell step: a best-effort Homebrew update that never aborts the install (`|| …`),
    // then the install itself; never a second, fatal `brew update` step.
    let qemu = install(Dependency::Qemu, HOMEBREW, Arch::Arm64).unwrap();
    let script = qemu[0].args.join(" ");
    assert_eq!(programs(&qemu), ["sh"]);
    assert!(script.contains("update ||") && script.contains("install qemu"), "{script}");
    assert!(qemu.iter().all(|s| s.args.get(1).is_none_or(|text| crate::testing::sh_parses(text))));
}

#[test]
fn every_macos_tool_has_a_way_out_and_only_homebrews_need_it() {
    assert_eq!(uninstall(Dependency::Terraform, None, None).unwrap_err(), Refusal::HomebrewNeeded);
    for dep in [Dependency::VmwareUtility, Dependency::Vagrant, Dependency::Docker, Dependency::Utm] {
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

#[cfg(unix)]
#[test]
fn a_failed_cask_fetch_updates_homebrew_and_retries_once() {
    // A Homebrew whose fetch always fails (an old one crashing on the cask data).
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("calls");
    let brew = dir.path().join("brew");
    crate::testing::write_executable(&brew, format!("#!/bin/sh\necho \"$1\" >> '{}'\n[ \"$1\" = update ]\n", log.display()));
    let step = fetch_and_open(brew.to_str().unwrap(), "vagrant", "Vagrant", Dependency::Vagrant);
    let out = crate::testing::sh(&step.args[1], &[]);
    assert!(!out.status.success());
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "fetch\nupdate\nfetch\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("Run 'brew update' in Terminal"));
}
