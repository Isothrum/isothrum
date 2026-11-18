use super::*;
use crate::install::command_line;

/// Root steps through pkexec, with no proxy.
fn prompted() -> Root {
    Root {
        elevation: Elevation::Prompt,
        proxy: Vec::new(),
    }
}

/// `set -e` stopping at a failure before a script's last command: no root script chains with `&&`.
mod root_chains;

#[test]
fn every_tool_installs_on_apt_and_each_one_but_the_apt_only_ones_on_dnf() {
    for dep in Dependency::ALL.into_iter().filter(|d| d.runs_on(HostOs::Linux)) {
        let apt = install(dep, Some(Packages::Linux(LinuxPackages::Apt)), Arch::Amd64, Some("ada"), &prompted()).unwrap();
        assert_eq!((apt.len(), apt[0].program.as_str()), (1, "pkexec"), "{dep:?}");
        assert_eq!(apt[0].args[..3], ["env", "sh", "-c"], "{dep:?}");
        let dnf = install(dep, Some(Packages::Linux(LinuxPackages::Dnf)), Arch::Amd64, Some("ada"), &prompted());
        match dep {
            Dependency::Azurecli | Dependency::Gcloud | Dependency::VmwareUtility => assert_eq!(dnf.unwrap_err(), Refusal::AptOnly { dependency: dep }),
            _ => assert!(dnf.is_ok(), "{dep:?}"),
        }
    }
}

#[test]
fn a_hypervisors_install_brings_every_program_its_vms_need_beside_it() {
    // Regression: QEMU and libvirt installed without genisoimage (mkisofs) and virtiofsd read as
    // not installed, and their VMs failed to start. Each program on its provider's list
    // (`providers::helpers`, which its detection reads) comes with its install, apt's and dnf's.
    let mut checked = 0;
    for dep in Dependency::ALL.into_iter().filter(|d| d.runs_on(HostOs::Linux)) {
        let Some(provider) = dep.hypervisor() else { continue };
        for packages in [LinuxPackages::Apt, LinuxPackages::Dnf] {
            let steps = install(dep, Some(Packages::Linux(packages)), Arch::Amd64, Some("ada"), &prompted()).unwrap();
            let script = steps[0].args.last().unwrap();
            for helper in crate::providers::helpers(provider, HostOs::Linux) {
                let package = helper.package(packages);
                let brought = script.split(|c: char| c.is_whitespace() || c == ';').any(|w| w == package);
                assert!(brought, "{dep:?} through {packages:?} brings {package}: {script}");
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 6, "QEMU's mkisofs and virtiofsd, libvirt's virtiofsd, through apt and dnf");
}

#[test]
fn what_doesnt_run_on_linux_or_without_a_package_manager_is_refused() {
    for dep in [Dependency::Wsl, Dependency::Hyperv, Dependency::Utm] {
        let refused = Refusal::NotHere {
            dependency: dep,
            os: HostOs::Linux,
        };
        assert_eq!(
            install(dep, Some(Packages::Linux(LinuxPackages::Apt)), Arch::Amd64, None, &prompted()).unwrap_err(),
            refused
        );
        assert_eq!(uninstall(dep, Some(Packages::Linux(LinuxPackages::Apt)), &prompted()).unwrap_err(), refused);
    }
    assert_eq!(
        install(Dependency::Docker, None, Arch::Amd64, None, &prompted()).unwrap_err(),
        Refusal::WithoutPackages {
            dependency: Dependency::Docker
        }
    );
    // The AWS CLI is a folder anywhere: removed without a package manager.
    assert!(uninstall(Dependency::Awscli, None, &prompted()).is_ok());
}

#[test]
fn a_removal_goes_through_the_package_manager_that_installed_it() {
    let removal = |packages| uninstall(Dependency::Vagrant, Some(Packages::Linux(packages)), &prompted()).unwrap()[0].args[3].clone();
    assert!(removal(LinuxPackages::Apt).ends_with("apt-get remove -y vagrant"));
    assert!(removal(LinuxPackages::Dnf).ends_with("dnf remove -y vagrant"));
    let gcloud = uninstall(Dependency::Gcloud, Some(Packages::Linux(LinuxPackages::Apt)), &prompted()).unwrap();
    assert_eq!(gcloud[1].args, ["-c", "rm -rf \"$HOME/google-cloud-sdk\""], "the per-user SDK too, as the user");
}

#[test]
fn unattended_a_root_step_runs_as_is_for_root_and_through_sudo_n_for_another_account() {
    let script = || RootScript::new("true");
    let as_root = Root {
        elevation: Elevation::Unattended { root: true },
        proxy: Vec::new(),
    };
    assert_eq!(as_root.step(script()).unwrap().program, "env");
    let sudo = Root {
        elevation: Elevation::Unattended { root: false },
        proxy: Vec::new(),
    };
    let step = sudo.step(script()).unwrap();
    assert_eq!((step.program.as_str(), &step.args[..2]), ("sudo", &["-n".to_owned(), "env".to_owned()][..]));
}

#[test]
fn a_root_steps_proxy_is_read_from_a_private_file_never_its_command_line() {
    let proxy = proxy_assignments(|name| (name == "https_proxy").then(|| "http://ada:s3cret@proxy.corp:3128".to_owned()));
    assert_eq!(proxy, ["https_proxy=http://ada:s3cret@proxy.corp:3128"]);
    let root = Root {
        elevation: Elevation::Prompt,
        proxy: [proxy, vec!["https_proxy=a\nb".to_owned()]].concat(),
    };
    let step = root.step(RootScript::new("apt-get update")).unwrap();
    let line = command_line(&step);
    assert!(!line.contains("s3cret") && line.contains("while IFS= read -r v"), "{line}");
    let dir = step.private.as_ref().unwrap().path().to_path_buf();
    assert_eq!(
        std::fs::read_to_string(dir.join("proxy")).unwrap(),
        "https_proxy=http://ada:s3cret@proxy.corp:3128\n",
        "a value over two lines is left out"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(dir.join("proxy")).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert!(crate::testing::sh_parses(&step.args[3]), "{}", step.args[3]);
    drop(step);
    assert!(!dir.exists(), "the private folder goes with the step");
}

#[cfg(unix)]
#[test]
fn only_proxy_variables_are_exported_from_the_file() {
    let dir = crate::testing::fresh_dir("proxy-read");
    let file = dir.join("proxy");
    std::fs::write(&file, "https_proxy=http://p:1\nPATH=/evil\nno_proxy=localhost\n").unwrap();
    let script = format!("{}; echo \"$https_proxy|$no_proxy|$PATH\"", read_proxy(&file));
    let out = crate::testing::sh(&script, &[("PATH", "/usr/bin:/bin")]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "http://p:1|localhost|/usr/bin:/bin");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Regression: on Linux arm64 Vagrant's install went through HashiCorp's repository, which serves
/// no arm64 Vagrant, so apt installed Debian's own 2.3.7: a run refuses it (2.4.0 or later), and it
/// adds a box's `x86_64` build on an arm64 computer. It is refused there, saying why; every other
/// tool's install is as on `x86_64`.
#[test]
fn vagrant_isnt_installed_on_linux_arm64_where_its_vendor_ships_no_build() {
    let apt = Some(Packages::Linux(LinuxPackages::Apt));
    let refused = install(Dependency::Vagrant, apt, Arch::Arm64, Some("ada"), &prompted()).unwrap_err();
    assert!(
        matches!(refused, Refusal::NoBuild { dependency: Dependency::Vagrant, os: HostOs::Linux, arch: Arch::Arm64, why } if why.contains("2.4.0")),
        "{refused:?}"
    );
    assert!(install(Dependency::Vagrant, apt, Arch::Amd64, Some("ada"), &prompted()).is_ok());
    for dep in Dependency::ALL.into_iter().filter(|d| d.runs_on(HostOs::Linux) && *d != Dependency::Vagrant) {
        assert_eq!(
            install(dep, apt, Arch::Arm64, Some("ada"), &prompted()).is_ok(),
            install(dep, apt, Arch::Amd64, Some("ada"), &prompted()).is_ok(),
            "{dep:?}"
        );
    }
}
