use super::*;
use crate::host::LinuxPackages;
use crate::install::{Note, plan};

const BREW: &str = "/opt/homebrew/bin/brew";
const PROMPT: Elevation = Elevation::Prompt;
const SUDO_N: Elevation = Elevation::Unattended { root: false };

fn host(os: HostOs, arch: Arch, packages: Option<Packages>) -> Host {
    let mut host = crate::testing::linux_host(std::path::Path::new("/nonexistent/isothrum-interaction"));
    host.os = os;
    host.arch = arch;
    host.packages = packages;
    host
}

fn mac(arch: Arch) -> Host {
    host(HostOs::Macos, arch, Some(Packages::Homebrew(BREW)))
}

fn windows_host() -> Host {
    host(HostOs::Windows, Arch::Amd64, Some(Packages::Winget))
}

fn linux_host(packages: LinuxPackages) -> Host {
    host(HostOs::Linux, Arch::Amd64, Some(Packages::Linux(packages)))
}

const MAC: Interaction = Interaction::AdminPassword { via: Prompt::Mac };
const UAC: Interaction = Interaction::AdminPassword { via: Prompt::Uac };
const PKEXEC: Interaction = Interaction::AdminPassword { via: Prompt::Pkexec };

#[test]
fn a_mac_asks_for_the_password_of_a_vendors_package_and_names_dockers_licence() {
    let on = |dep, arch| interactions(dep, &mac(arch), PROMPT, None);
    assert_eq!(on(Dependency::Docker, Arch::Arm64), [MAC, DOCKER_LICENCE]);
    for dep in [Dependency::Vagrant, Dependency::Utm, Dependency::VmwareUtility, Dependency::Awscli] {
        assert_eq!(on(dep, Arch::Arm64), [MAC], "{dep:?}");
        assert_eq!(on(dep, Arch::Amd64), [MAC], "{dep:?}: a vendor's build, nothing built here");
    }
    for dep in [Dependency::Terraform, Dependency::Azurecli, Dependency::Gcloud] {
        assert_eq!(on(dep, Arch::Arm64), [], "{dep:?}: Homebrew installs it as the user");
    }
    let Interaction::Licence { name, url } = DOCKER_LICENCE else { unreachable!() };
    assert_eq!(name, "Docker Subscription Service Agreement");
    assert!(url.starts_with("https://www.docker.com/legal/"));
}

#[test]
fn virtualbox_on_an_intel_mac_asks_to_allow_oracles_extension_then_a_restart() {
    assert_eq!(
        interactions(Dependency::Virtualbox, &mac(Arch::Amd64), PROMPT, None),
        [
            MAC,
            Interaction::AllowSystemExtension {
                vendor: "Oracle America, Inc."
            },
            Interaction::Restart
        ]
    );
    assert_eq!(
        interactions(Dependency::Virtualbox, &mac(Arch::Arm64), PROMPT, None),
        [MAC],
        "no kernel extension on Apple silicon"
    );
}

#[test]
fn an_intel_mac_builds_qemu_from_source() {
    assert_eq!(
        interactions(Dependency::Qemu, &mac(Arch::Amd64), PROMPT, None),
        [Interaction::LongBuild { minutes: 15 }]
    );
    assert_eq!(interactions(Dependency::Qemu, &mac(Arch::Arm64), PROMPT, None), []);
    // The step says it too.
    let qemu = plan(Dependency::Qemu, &mac(Arch::Amd64), None, PROMPT).unwrap();
    assert_eq!(qemu[0].note, Some(Note::BuildsFromSource(Dependency::Qemu)));
}

#[test]
fn an_unattended_install_raises_no_password_prompt() {
    for elevation in [SUDO_N, Elevation::Unattended { root: true }] {
        assert_eq!(interactions(Dependency::Docker, &mac(Arch::Arm64), elevation, None), [DOCKER_LICENCE]);
        assert_eq!(interactions(Dependency::Vagrant, &mac(Arch::Arm64), elevation, None), []);
        assert_eq!(interactions(Dependency::Vagrant, &linux_host(LinuxPackages::Apt), elevation, None), []);
        assert_eq!(
            interactions(Dependency::Docker, &linux_host(LinuxPackages::Apt), elevation, Some("ada")),
            [Interaction::Relogin],
            "the group still applies from the next login"
        );
    }
}

#[test]
fn windows_asks_through_uac_but_for_terraforms_portable_zip() {
    let on = |dep| interactions(dep, &windows_host(), PROMPT, None);
    assert_eq!(on(Dependency::Docker), [UAC, DOCKER_LICENCE]);
    assert_eq!(on(Dependency::Terraform), []);
    for dep in [Dependency::Wsl, Dependency::Hyperv, Dependency::Vagrant] {
        assert_eq!(on(dep), [UAC, Interaction::Restart], "{dep:?}");
    }
    for dep in [
        Dependency::Virtualbox,
        Dependency::Awscli,
        Dependency::Azurecli,
        Dependency::Gcloud,
        Dependency::VmwareUtility,
    ] {
        assert_eq!(on(dep), [UAC], "{dep:?}");
    }
    // UAC is Windows' own, whatever the elevation says.
    assert_eq!(interactions(Dependency::Awscli, &windows_host(), SUDO_N, None), [UAC]);
}

#[test]
fn linux_asks_through_pkexec_and_a_new_login_for_the_groups_it_joins() {
    let apt = linux_host(LinuxPackages::Apt);
    let dnf = linux_host(LinuxPackages::Dnf);
    for dep in [Dependency::Docker, Dependency::Qemu, Dependency::Libvirt] {
        assert_eq!(interactions(dep, &apt, PROMPT, Some("ada")), [PKEXEC, Interaction::Relogin], "{dep:?}");
        assert_eq!(interactions(dep, &dnf, PROMPT, Some("ada")), [PKEXEC, Interaction::Relogin], "{dep:?}");
        assert_eq!(interactions(dep, &apt, PROMPT, None), [PKEXEC], "{dep:?}: nobody joins a group");
        assert_eq!(interactions(dep, &apt, PROMPT, Some("a; rm -rf /")), [PKEXEC], "{dep:?}: not a user name");
    }
    assert_eq!(interactions(Dependency::Virtualbox, &apt, PROMPT, Some("ada")), [PKEXEC]);
    assert_eq!(interactions(Dependency::Virtualbox, &dnf, PROMPT, Some("ada")), [PKEXEC, Interaction::Relogin]);
    assert_eq!(interactions(Dependency::Terraform, &apt, PROMPT, Some("ada")), [PKEXEC]);
}

#[test]
fn a_tool_that_doesnt_run_there_asks_nothing() {
    assert_eq!(interactions(Dependency::Wsl, &mac(Arch::Arm64), PROMPT, None), []);
    assert_eq!(interactions(Dependency::Utm, &linux_host(LinuxPackages::Apt), PROMPT, None), []);
}

/// Every host a test reads: each OS, CPU and package manager.
fn hosts() -> Vec<Host> {
    let mut all = vec![mac(Arch::Arm64), mac(Arch::Amd64), windows_host()];
    for packages in [LinuxPackages::Apt, LinuxPackages::Dnf] {
        for arch in [Arch::Amd64, Arch::Arm64] {
            all.push(host(HostOs::Linux, arch, Some(Packages::Linux(packages))));
        }
    }
    all
}

/// Whether `step` raises the OS's password prompt, as its command says: pkexec's, macOS's
/// administrator prompt (`with administrator privileges`), or UAC's (`-Verb RunAs`, or winget's
/// machine-wide installer).
fn raises_prompt(step: &crate::install::Step) -> Option<Prompt> {
    let script = step.args.join(" ");
    if step.program == "pkexec" {
        Some(Prompt::Pkexec)
    } else if script.contains("with administrator privileges") {
        Some(Prompt::Mac)
    } else if script.contains("RunAs") || step.program == "winget" && !script.contains("Hashicorp.Terraform") {
        Some(Prompt::Uac)
    } else {
        None
    }
}

#[test]
fn every_plan_hands_its_interactions_to_the_step_that_raises_them() {
    for host in hosts() {
        for elevation in [PROMPT, SUDO_N] {
            for dep in Dependency::ALL {
                let Ok(steps) = plan(dep, &host, Some("ada"), elevation) else { continue };
                let want = interactions(dep, &host, elevation, Some("ada"));
                let (last, before) = steps.split_last().unwrap();
                assert_eq!(last.interactions, want, "{dep:?} on {host:?} {elevation:?}");
                assert!(before.iter().all(|s| s.interactions.is_empty() && raises_prompt(s).is_none()), "{dep:?}");
                // A password prompt is said exactly where the step's command raises one.
                let said = want.iter().find_map(|i| match i {
                    Interaction::AdminPassword { via } => Some(*via),
                    _ => None,
                });
                assert_eq!(said, raises_prompt(last), "{dep:?} on {:?} {:?} {elevation:?}", host.os, host.arch);
                // A licence is said exactly where the step accepts one.
                let licence = want.iter().any(|i| matches!(i, Interaction::Licence { .. }));
                let accepts = matches!(last.note, Some(Note::InstallingFromVendor { accepts: Some(_), .. }))
                    || last.args.iter().any(|a| a.contains("Docker.DockerDesktop"));
                assert_eq!(licence, accepts, "{dep:?} on {:?}", host.os);
            }
        }
    }
}

#[test]
fn interactions_read_as_their_kind_and_fields() {
    let json = serde_json::to_value([MAC, DOCKER_LICENCE, Interaction::LongBuild { minutes: 30 }, Interaction::Relogin]).unwrap();
    assert_eq!(
        json,
        serde_json::json!([
            {"kind": "adminPassword", "via": "mac"},
            {"kind": "licence", "name": "Docker Subscription Service Agreement", "url": "https://www.docker.com/legal/docker-subscription-service-agreement/"},
            {"kind": "longBuild", "minutes": 30},
            {"kind": "relogin"},
        ])
    );
    let extension = Interaction::AllowSystemExtension {
        vendor: "Oracle America, Inc.",
    };
    assert_eq!(serde_json::to_value(extension).unwrap()["kind"], "allowSystemExtension");
}
