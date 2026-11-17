use super::*;

#[test]
fn docker_install_adds_the_user_to_the_docker_group() {
    let s = docker_script(Some("alice"));
    assert!(s.contains("usermod -aG docker alice"), "{s}");
    // A name that isn't a plain login name never reaches the script.
    assert!(!docker_script(Some("a; rm -rf /")).contains("usermod"));
    assert!(!docker_script(None).contains("usermod"));
    assert!(valid_user("_svc-1.x") && !valid_user("Root") && !valid_user(""));
}

#[test]
fn libvirt_install_brings_what_the_vagrant_libvirt_plugin_compiles_against() {
    // Regression: libvirt alone, and `vagrant plugin install vagrant-libvirt` then failed
    // building its native extension (no libvirt headers, no compiler).
    let s = libvirt_script(None);
    for package in ["libvirt-daemon-system", "libvirt-dev", "build-essential", "pkg-config", "ruby-dev"] {
        assert!(s.split_whitespace().any(|w| w == package), "{package}: {s}");
    }
    assert!(!s.contains("usermod"));
    let with_user = libvirt_script(Some("ada"));
    assert!(
        with_user.contains("usermod -aG libvirt ada") && with_user.contains(&join_kvm_group("ada", "/dev/kvm")),
        "{with_user}"
    );
    assert!(!libvirt_script(Some("a; rm -rf /")).contains("usermod"));
    assert!(crate::testing::sh_parses(&s), "not valid sh: {s}");
}

#[test]
fn qemu_install_adds_the_user_to_the_group_that_opens_dev_kvm() {
    // Regression: the QEMU install added nobody to the kvm group, so on Debian 13 (/dev/kvm
    // root:kvm 0660) every VM's `accel=kvm` was refused: "Could not access KVM kernel module:
    // Permission denied".
    let s = qemu_script(Some("ada"));
    let install = "apt-get update; apt-get install -y qemu-system qemu-utils genisoimage; \
                   if apt-cache show virtiofsd >/dev/null 2>&1; then apt-get install -y virtiofsd; fi";
    assert_eq!(s, RootScript::new(format!("{install}; {}", join_kvm_group("ada", "/dev/kvm"))));
    assert!(crate::testing::sh_parses(&s), "not valid sh: {s}");
    assert_eq!(qemu_script(None), RootScript::new(install));
    assert!(!qemu_script(Some("a; rm -rf /")).contains("usermod"));
}

#[test]
fn qemu_install_brings_the_mkisofs_vagrant_builds_a_vms_network_seed_with() {
    // Regression (an Ubuntu 24.04 VM on QEMU): the install was `qemu-system
    // qemu-utils`, and every VM project failed at its first VM: "The executable 'mkisofs'
    // Vagrant is trying to run was not found in the PATH variable". `genisoimage` is the
    // package with /usr/bin/mkisofs on Debian and Ubuntu.
    for user in [None, Some("ada")] {
        let s = qemu_script(user);
        let install = s.split("; ").find(|c| c.contains("apt-get install")).unwrap();
        assert!(install.split_whitespace().any(|w| w == "genisoimage"), "{s}");
    }
}

/// Whether `script` exits non-zero when its package installs fail (`apt-get install` and
/// `dnf`, as when apt's lock is held or the network is down), the group steps after them
/// standing in for success.
#[cfg(unix)]
fn fails_when_its_package_manager_does(script: &str) -> bool {
    let bin = crate::testing::fresh_dir("pm");
    std::fs::create_dir_all(&bin).unwrap();
    for (name, body) in [
        ("apt-get", r#"[ "$1" = update ] || exit 100"#),
        ("dnf", "exit 1"),
        ("apt-cache", "exit 0"),
        ("usermod", "exit 0"),
        ("getent", "exit 0"),
        ("systemctl", "exit 0"),
    ] {
        crate::testing::write_executable(bin.join(name), format!("#!/bin/sh\n{body}\n"));
    }
    let out = crate::testing::sh(script, &[("PATH", &format!("{}:/usr/bin:/bin", bin.display()))]);
    std::fs::remove_dir_all(&bin).unwrap();
    !out.status.success()
}

#[cfg(unix)]
#[test]
fn a_failed_libvirt_or_qemu_install_fails_its_step() {
    // Regression: after a failed install the group step that followed decided the exit status,
    // and the install read as done. `set -e` doesn't stop at a failure inside an `&&` chain,
    // so the commands are separate statements.
    for (name, s) in [
        ("libvirt", libvirt_script(Some("ada"))),
        ("qemu", qemu_script(Some("ada"))),
        ("libvirt dnf", libvirt_dnf_script(Some("ada"))),
        ("qemu dnf", qemu_dnf_script(Some("ada"))),
    ] {
        assert!(s.starts_with(crate::install::root::PRELUDE), "{name}: {s}");
        assert!(fails_when_its_package_manager_does(&s), "{name} reads as done after a failed install: {s}");
    }
}

/// What [`join_kvm_group`] does on a system whose `stat` says `owner` owns the device (empty:
/// no device) and whose groups are `groups`: the `usermod` it ran, if any, and what it said.
#[cfg(unix)]
fn joined(owner: &str, groups: &[&str]) -> (String, String) {
    let fakes = format!(
        "stat() {{ [ -n '{owner}' ] && echo '{owner}' || return 1; }}; getent() {{ for g in {}; do [ \"$g\" = \"$2\" ] && return 0; done; return 1; }}; usermod() {{ echo \"usermod $*\" >&2; }};",
        groups.join(" ")
    );
    let out = crate::testing::sh(&format!("{fakes} {}", join_kvm_group("ada", "/dev/kvm")), &[]);
    assert!(out.status.success(), "{out:?}");
    (
        String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        String::from_utf8_lossy(&out.stdout).trim().to_owned(),
    )
}

// Runs the step in `sh`, which Windows hasn't: the step is for Linux's installs only.
#[cfg(unix)]
#[test]
fn the_kvm_group_joined_is_the_devices_on_every_distribution() {
    // Debian, Ubuntu, Kali, openSUSE: /dev/kvm root:kvm 0660.
    let (ran, said) = joined("kvm", &["root", "kvm"]);
    assert_eq!(ran, "usermod -aG kvm ada");
    assert_eq!(
        said,
        "Added ada to the kvm group, which opens /dev/kvm: log out and back in (or restart) for it to apply."
    );
    // A distribution's own group for it.
    assert_eq!(joined("hypervisor", &["kvm", "hypervisor"]).0, "usermod -aG hypervisor ada");
    // Fedora and Arch (root:kvm 0666), or no device yet: the kvm group where there is one.
    assert_eq!(joined("root", &["kvm"]).0, "usermod -aG kvm ada");
    assert_eq!(joined("", &["kvm"]).0, "usermod -aG kvm ada");
    // No such group: nobody is added, and the install still succeeds.
    assert_eq!(joined("", &["root"]), (String::new(), String::new()));
}

#[test]
fn libvirt_install_brings_virtiofsd_for_the_shared_folder_where_the_release_has_it() {
    // Regression: libvirt alone, and a VM with a shared folder failed to start on
    // libvirt ("Unable to find a satisfying virtiofsd").
    let s = libvirt_script(None);
    assert!(
        s.contains("if apt-cache show virtiofsd >/dev/null 2>&1; then apt-get install -y virtiofsd; fi"),
        "{s}"
    );
    assert!(crate::testing::sh_parses(&s), "not valid sh: {s}");
}

#[test]
fn qemu_install_brings_virtiofsd_for_the_shared_folder_where_the_release_has_it() {
    // Regression: QEMU alone, and a VM with a shared folder failed to start on QEMU
    // on Linux: vagrant-qemu shares it through the virtiofsd it starts ("virtiofsd was not found
    // or is not executable").
    let s = qemu_script(None);
    assert!(
        s.contains("if apt-cache show virtiofsd >/dev/null 2>&1; then apt-get install -y virtiofsd; fi"),
        "{s}"
    );
    assert!(crate::testing::sh_parses(&s), "not valid sh: {s}");
    assert!(qemu_dnf_script(None).split_whitespace().any(|w| w == "virtiofsd"), "dnf too");
}

#[test]
fn login_names_at_the_edges_of_what_goes_unquoted_into_the_script() {
    // 32 characters is the longest a login name can be; one more, a capital, a digit or a
    // dash first, a space or non-ASCII anywhere: not a name, so no `usermod` line.
    assert!(valid_user(&"a".repeat(32)) && !valid_user(&"a".repeat(33)));
    assert!(valid_user("a") && valid_user("_") && valid_user("a-") && valid_user("a.b"));
    for bad in ["1a", "-a", "A", "ab c", "caf\u{e9}", "a;b", "a$b", "a\nb"] {
        assert!(!valid_user(bad), "{bad:?}");
        assert!(!docker_script(Some(bad)).contains("usermod"), "{bad:?}");
    }
    // The group line names the user once, after the install, and the script stays one line.
    let s = docker_script(Some("ada"));
    assert_eq!(s.matches("usermod -aG docker ada").count(), 1);
    assert!(!s.contains('\n'));
}

/// The vendors' repositories: each set up the same way, its key pinned, its release found.
mod repos;

/// The vendors' signing keys and Amazon's bundle signature, checked before anything is trusted.
mod keys;
