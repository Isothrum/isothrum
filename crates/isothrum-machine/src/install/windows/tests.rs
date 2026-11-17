use super::*;

#[test]
fn windows_installs_through_winget_and_the_features_through_their_own_installers() {
    for dep in Dependency::ALL {
        let plan = install(dep, Arch::Amd64);
        match dep {
            Dependency::Utm | Dependency::Libvirt | Dependency::Qemu => {
                assert_eq!(
                    plan.unwrap_err(),
                    Refusal::NotHere {
                        dependency: dep,
                        os: HostOs::Windows
                    }
                );
            }
            Dependency::Wsl | Dependency::VmwareUtility | Dependency::Hyperv => assert_eq!(plan.unwrap()[0].program, "powershell"),
            _ => {
                let steps = plan.unwrap();
                assert_eq!(steps[0].program, "winget");
                assert!(steps[0].args.contains(&"--accept-package-agreements".to_owned()));
                assert!(steps[0].args.windows(2).any(|w| w == ["--source", "winget"]), "{dep:?}: {:?}", steps[0].args);
            }
        }
    }
}

#[test]
fn windows_on_arm_is_refused_the_hypervisors_not_built_for_it() {
    for dep in Dependency::ALL.into_iter().filter(|d| d.runs_on(HostOs::Windows)) {
        let served = match dep {
            Dependency::VmwareUtility => Some(Provider::VmwareDesktop),
            other => other.hypervisor(),
        };
        let refused = served.is_some_and(|p| !ProviderRow::of(p).windows_on_arm);
        assert_eq!(install(dep, Arch::Arm64).is_err(), refused, "{dep:?}");
    }
    assert_eq!(
        install(Dependency::Virtualbox, Arch::Arm64).unwrap_err(),
        Refusal::NotOnWindowsOnArm {
            dependency: Dependency::Virtualbox,
            provider: Provider::Virtualbox
        }
    );
}

#[test]
fn hyper_v_is_turned_on_by_dism_elevated_and_refused_on_windows_home() {
    // DISM's "done, restart to finish" (3010) is a success; Home has no Hyper-V to turn on.
    assert!(HYPERV_ON.contains("/FeatureName:Microsoft-Hyper-V") && HYPERV_ON.contains("/NoRestart") && HYPERV_ON.contains("-Verb RunAs"));
    assert!(HYPERV_ON.contains("-eq 3010) { exit 0 }") && HYPERV_ON.contains("EditionID -like 'Core*'"));
    // Home's refusal is a code a UI words, said by no line of the script's own.
    assert!(HYPERV_ON.contains(&format!("-like 'Core*') {{ exit {} }}", super::super::steps::NOT_SERVED)) && !HYPERV_ON.contains("Write-Output"));
    assert!(WSL_ON.contains("'--install','--no-distribution'") && WSL_ON.contains("catch { exit 1223 }"));
}

#[test]
fn every_windows_tool_is_removed_through_winget_or_its_own_way() {
    for dep in [
        Dependency::Docker,
        Dependency::Vagrant,
        Dependency::Terraform,
        Dependency::Virtualbox,
        Dependency::Awscli,
        Dependency::Azurecli,
        Dependency::Gcloud,
    ] {
        let steps = uninstall(dep).unwrap();
        assert_eq!(steps[0].program, "winget", "{dep:?}");
        assert!(steps[0].args.windows(2).any(|w| w == ["--source", "winget"]), "{dep:?}: {:?}", steps[0].args);
    }
    assert_eq!(
        uninstall(Dependency::VmwareUtility).unwrap()[0].program,
        "powershell",
        "its MSI by its product code"
    );
    assert_eq!(uninstall(Dependency::Wsl).unwrap_err(), Refusal::WindowsFeature { dependency: Dependency::Wsl });
    assert_eq!(
        uninstall(Dependency::Utm).unwrap_err(),
        Refusal::NotHere {
            dependency: Dependency::Utm,
            os: HostOs::Windows
        }
    );
}
