//! The root scripts behind the installs on Fedora and the distributions built on it: dnf, the
//! vendors' rpm repositories (their keys pinned) and RPM Fusion's VirtualBox.

#[cfg(test)]
use super::{NEED_CURL, NEED_UNZIP};
use super::{NEED_GPG, RootScript, check_keys, fetch_prelude, then_kvm_group, then_libvirt_groups, valid_user};

/// A vendor's rpm repository, set up the same way for every vendor ([`rpm_repo_setup`]): its
/// signing key downloaded and checked against the pinned fingerprint, kept under
/// `/etc/pki/rpm-gpg` and imported, then a repo file under `/etc/yum.repos.d` that trusts that
/// key alone (`gpgcheck=1`). `RELEASE` in the base URL is the Fedora release the repository is
/// read for: this one, else the newest earlier one it serves (HashiCorp serves a new Fedora
/// release weeks after it ships), else the refusal, as the apt repositories' `release_served`.
struct RpmRepo {
    vendor: &'static str,
    fingerprint: &'static str,
    key_url: &'static str,
    /// The base URL with `RELEASE` for the Fedora release and `$basearch` for dnf's architecture.
    baseurl: &'static str,
    /// The key file's and the repo file's name.
    name: &'static str,
    /// Where to install from instead when the repository serves no release near this one.
    by_hand: &'static str,
}

/// `rpm_served <base url>`: whether an rpm repository at that URL answers (its `repodata/repomd.xml`):
/// a 2xx is yes and a 404 no; anything else stops the script (an unreachable server is never "no
/// packages"), as [`RELEASE_SERVED`] does for apt.
const RPM_SERVED: &str = r#"rpm_served() { served=$(curl -s -o /dev/null -w '%{http_code}' "$1/repodata/repomd.xml" || true); case "$served" in 2??) return 0 ;; 404) return 1 ;; *) echo "Couldn't reach $1 (HTTP $served). Check this machine's internet access or proxy, then try again." >&2; exit 1 ;; esac; }"#;

/// The script that sets `r` up for dnf, after [`fetch_prelude`] in a [`RootScript`]: the
/// release the repository serves found (`$rel`, this Fedora's or the newest earlier one of the
/// three), the key checked and imported, the repo file written.
fn rpm_repo_setup(r: &RpmRepo) -> String {
    let RpmRepo {
        vendor,
        fingerprint,
        key_url,
        baseurl,
        name,
        by_hand,
    } = r;
    let probe = baseurl.replace("$basearch", "$(uname -m)");
    format!(
        r#"{RPM_SERVED}; this=$(rpm -E %fedora); rel=""; for r in "$this" $((this - 1)) $((this - 2)); do if rpm_served "{probe_r}"; then rel=$r; break; fi; done; [ -n "$rel" ] || {{ echo "{vendor} has no packages for Fedora $this yet. Install it from {by_hand}" >&2; exit 1; }}; [ "$rel" = "$this" ] || echo "{vendor} has no packages for Fedora $this: installing its Fedora $rel ones."; {NEED_GPG}; curl -fsSL "{key_url}" -o "$d/key"; {check}; install -m 0644 "$d/key" /etc/pki/rpm-gpg/RPM-GPG-KEY-{name}; rpm --import /etc/pki/rpm-gpg/RPM-GPG-KEY-{name}; printf '[{name}]\nname={vendor}\nbaseurl=%s\nenabled=1\ngpgcheck=1\ngpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-{name}\n' "{base_rel}" >/etc/yum.repos.d/{name}.repo"#,
        probe_r = probe.replace("RELEASE", "$r"),
        // The release found expands; dnf's own `$basearch` stays for dnf.
        base_rel = baseurl.replace("$basearch", "\\$basearch").replace("RELEASE", "$rel"),
        check = check_keys(&[fingerprint], vendor),
    )
}

/// Docker's rpm repository for Fedora, its key pinned (`docker@docker.com`, the key
/// download.docker.com/linux/fedora/gpg serves; not the apt one).
const DOCKER_RPM: RpmRepo = RpmRepo {
    vendor: "Docker",
    fingerprint: "060A61C51B558A7F742B77AAC52FEB6B621E9F35",
    key_url: "https://download.docker.com/linux/fedora/gpg",
    baseurl: "https://download.docker.com/linux/fedora/RELEASE/$basearch/stable",
    name: "docker-ce",
    by_hand: "https://docs.docker.com/engine/install/fedora/",
};

/// HashiCorp's rpm repository (Vagrant, Terraform), signed by the same key as its apt one.
const HASHICORP_RPM: RpmRepo = RpmRepo {
    vendor: "HashiCorp",
    fingerprint: "D55C0D1AC78A8D8126CB631CFC9CA96ACA026560",
    key_url: "https://rpm.releases.hashicorp.com/gpg",
    baseurl: "https://rpm.releases.hashicorp.com/fedora/RELEASE/$basearch/stable",
    name: "hashicorp",
    by_hand: "https://developer.hashicorp.com/vagrant/install",
};

/// Docker Engine on Fedora from Docker's own repository, its key pinned, then its service on;
/// the user joins the `docker` group as on Debian.
#[must_use]
pub fn docker_dnf_script(user: Option<&str>) -> RootScript {
    let mut s = format!(
        "{}; {}; dnf install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin; systemctl enable --now docker",
        fetch_prelude(),
        rpm_repo_setup(&DOCKER_RPM)
    );
    if let Some(u) = user.filter(|u| valid_user(u)) {
        s = format!("{s}; usermod -aG docker {u}; echo 'Added {u} to the docker group: log out and back in (or restart) for it to apply.'");
    }
    RootScript::new(s)
}

/// Vagrant on Fedora from HashiCorp's rpm repository (the distribution has none).
#[must_use]
pub fn vagrant_dnf_script() -> RootScript {
    RootScript::new(format!("{}; {}; dnf install -y vagrant", fetch_prelude(), rpm_repo_setup(&HASHICORP_RPM)))
}

/// Terraform on Fedora from the same repository.
#[must_use]
pub fn terraform_dnf_script() -> RootScript {
    RootScript::new(format!("{}; {}; dnf install -y terraform", fetch_prelude(), rpm_repo_setup(&HASHICORP_RPM)))
}

/// VirtualBox on Fedora from RPM Fusion (Oracle's own repository lags the Fedora releases by
/// months), its kernel module built here for the running kernel (akmods),
/// then the user into `vboxusers`. RPM Fusion's release package carries its signing key, which
/// dnf imports as it installs it.
#[must_use]
pub fn virtualbox_dnf_script(user: Option<&str>) -> RootScript {
    let mut s = r#"dnf install -y "https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm"; dnf install -y VirtualBox akmod-VirtualBox "kernel-devel-$(uname -r)" kernel-headers; akmods --force --kernels "$(uname -r)"; modprobe vboxdrv || echo "VirtualBox's driver isn't loaded yet: restart the computer to start it, and VirtualBox's driver is built then.""#.to_owned();
    if let Some(u) = user.filter(|u| valid_user(u)) {
        s = format!("{s}; usermod -aG vboxusers {u}");
    }
    RootScript::new(s)
}

/// libvirt on Fedora with what vagrant-libvirt builds against (libvirt's headers, a C compiler,
/// pkg-config, Ruby's headers) and virtiofsd for the shared folders, its service on, then the
/// user into the `libvirt` group (and the KVM device's).
#[must_use]
pub fn libvirt_dnf_script(user: Option<&str>) -> RootScript {
    then_libvirt_groups(
        "dnf install -y libvirt libvirt-daemon-kvm qemu-kvm virt-install virt-manager libvirt-devel gcc make pkgconf-pkg-config ruby-devel virtiofsd; systemctl enable --now libvirtd",
        user,
    )
}

/// QEMU and its disk tools on Fedora, with `genisoimage` for vagrant-qemu's cloud-init seeds and
/// `virtiofsd` for its shared folders (its own package from Fedora 36 on), then the user into
/// the KVM device's group.
#[must_use]
pub fn qemu_dnf_script(user: Option<&str>) -> RootScript {
    then_kvm_group("dnf install -y qemu-kvm qemu-img genisoimage virtiofsd", user)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dnf_script_is_valid_sh_and_installs_through_dnf_alone() {
        for (name, s) in [
            ("docker", docker_dnf_script(Some("alice"))),
            ("vagrant", vagrant_dnf_script()),
            ("terraform", terraform_dnf_script()),
            ("virtualbox", virtualbox_dnf_script(Some("alice"))),
            ("libvirt", libvirt_dnf_script(Some("alice"))),
            ("qemu", qemu_dnf_script(Some("alice"))),
        ] {
            assert!(crate::testing::sh_parses(&s), "{name}: not valid sh: {s}");
            // The preludes install curl and gpg through whichever manager is there; the rest is dnf's.
            let own = s.replace(NEED_CURL, "").replace(NEED_GPG, "").replace(NEED_UNZIP, "");
            assert!(!own.contains("apt-get"), "{name} installs through apt: {s}");
            assert!(s.contains("dnf install -y"), "{name}: {s}");
        }
    }

    #[test]
    fn a_vendors_rpm_repository_is_pinned_by_key_and_read_for_a_release_it_serves() {
        let s = rpm_repo_setup(&HASHICORP_RPM);
        assert!(s.contains(r#"[ "$k" = "D55C0D1AC78A8D8126CB631CFC9CA96ACA026560" ]"#), "the key pinned: {s}");
        assert!(
            s.contains("rpm --import /etc/pki/rpm-gpg/RPM-GPG-KEY-hashicorp")
                && s.contains("gpgcheck=1")
                && s.contains("gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-hashicorp"),
            "{s}"
        );
        assert!(
            s.contains(r#"for r in "$this" $((this - 1)) $((this - 2))"#)
                && s.contains(r#"rpm_served "https://rpm.releases.hashicorp.com/fedora/$r/$(uname -m)/stable""#),
            "this release, else the two before: {s}"
        );
        assert!(
            s.contains(r#""https://rpm.releases.hashicorp.com/fedora/$rel/\$basearch/stable""#),
            "the release found, dnf's own $basearch: {s}"
        );
        assert!(
            s.contains("HashiCorp has no packages for Fedora $this yet. Install it from https://developer.hashicorp.com/vagrant/install"),
            "{s}"
        );
        let docker = rpm_repo_setup(&DOCKER_RPM);
        assert!(
            docker.contains("060A61C51B558A7F742B77AAC52FEB6B621E9F35") && docker.contains("/etc/yum.repos.d/docker-ce.repo"),
            "{docker}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_release_the_repository_doesnt_serve_falls_back_to_the_newest_earlier_one() {
        // HashiCorp serving Fedora 44 and not yet 45.
        let s = rpm_repo_setup(&HASHICORP_RPM);
        let head = s.get(..s.find("; command -v gpg").unwrap()).unwrap();
        let bin = crate::testing::fresh_dir("rpm-probe");
        std::fs::create_dir_all(&bin).unwrap();
        crate::testing::write_executable(
            bin.join("curl"),
            "#!/bin/sh\ncase \"$*\" in */fedora/44/*) printf 200 ;; *) printf 404 ;; esac\n",
        );
        crate::testing::write_executable(bin.join("rpm"), "#!/bin/sh\necho 45\n");
        let out = crate::testing::sh(
            &format!("set -e; {head}; echo \"rel=$rel\""),
            &[("PATH", &format!("{}:/usr/bin:/bin", bin.display()))],
        );
        std::fs::remove_dir_all(&bin).unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("HashiCorp has no packages for Fedora 45: installing its Fedora 44 ones.") && stdout.trim_end().ends_with("rel=44"),
            "{stdout}"
        );
    }

    #[test]
    fn virtualbox_on_fedora_comes_from_rpm_fusion_with_its_module_built_here() {
        let s = virtualbox_dnf_script(Some("alice"));
        assert!(
            s.contains("rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm")
                && s.contains("akmod-VirtualBox")
                && s.contains("akmods --force")
                && s.contains("usermod -aG vboxusers alice"),
            "{s}"
        );
    }
}
