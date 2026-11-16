use super::*;

#[cfg(target_os = "macos")]
pub(super) fn repo_scripts() -> [(&'static VendorRepo, RootScript); 7] {
    [
        (&HASHICORP, terraform_script()),
        (&HASHICORP, vagrant_script()),
        (&HASHICORP, vmware_utility_script()),
        (&ORACLE, virtualbox_script()),
        (&DOCKER, docker_script(None)),
        (&MICROSOFT, azurecli_script()),
        (&GOOGLE, gcloud_script()),
    ]
}

#[test]
fn repo_scripts_rerun_without_a_terminal_and_parse() {
    // gpg asked "overwrite?" on a second run, with no tty to answer it.
    for (_, s) in repo_scripts() {
        assert!(s.contains("gpg --batch --yes --dearmor"), "{s}");
        assert!(crate::testing::sh_parses(&s), "not valid sh: {s}");
    }
    assert!(virtualbox_script().contains("download.virtualbox.org/virtualbox/debian"));
    // VMware's utility from HashiCorp's repository, its key pinned like Terraform's; a release
    // the repository doesn't serve yet is sent to the utility's own page.
    let utility = vmware_utility_script();
    assert!(
        utility.contains(HASHICORP.fingerprint) && utility.contains("apt-get install -y --reinstall vagrant-vmware-utility"),
        "{utility}"
    );
    assert!(
        utility.contains("developer.hashicorp.com/vagrant/install/vmware") && !utility.contains("terraform/install"),
        "{utility}"
    );
}

#[test]
fn every_vendor_repository_is_set_up_the_same_way() {
    // Regression: five copies of one sequence had drifted (keyrings in /usr/share/keyrings or
    // /etc/apt/keyrings, an architecture on some sources and not others).
    for (repo, s) in repo_scripts() {
        let name = repo.name;
        assert!(s.contains(&format!("-o /etc/apt/keyrings/{name}.gpg")), "{s}");
        let source = format!(
            "signed-by=/etc/apt/keyrings/{name}.gpg] $repo {} {}\" > /etc/apt/sources.list.d/{name}.list",
            repo.suite, repo.component
        );
        assert!(s.contains(&format!("deb [arch=$(dpkg --print-architecture) {source}")), "{s}");
        assert!(!s.contains("/usr/share/keyrings"), "{s}");
        assert_eq!(
            s.contains(&format!(r#"release_served "$repo" "{}""#, repo.suite)),
            repo.by_hand.is_some(),
            "{s}"
        );
    }
}

/// Every install script that runs as root.
pub(super) fn root_scripts() -> [RootScript; 8] {
    [
        awscli_script(),
        docker_script(Some("alice")),
        terraform_script(),
        vagrant_script(),
        virtualbox_script(),
        azurecli_script(),
        gcloud_script(),
        vmware_utility_script(),
    ]
}

#[test]
fn every_root_script_that_downloads_installs_curl_first() {
    // Regression: Debian's minimal image has no curl; the Vagrant install failed, saying
    // HashiCorp had no packages for trixie, and Docker's repository check read as "not
    // served", so Docker came from the distribution instead.
    for s in root_scripts() {
        if let Some(first) = s.find("curl -") {
            let installed = s.find(NEED_CURL).unwrap_or_else(|| panic!("curl used, never installed: {s}"));
            assert!(installed < first, "curl used before it is installed: {s}");
        }
    }
}

/// Runs `script` with [`RELEASE_SERVED`] defined and a `curl` that answers HTTP status `http`:
/// its stdout and stderr.
#[cfg(unix)]
fn with_release_answer(http: &str, script: &str) -> (String, String) {
    let bin = crate::testing::fresh_dir("curl");
    std::fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("curl");
    crate::testing::write_executable(&fake, format!("#!/bin/sh\nprintf '%s' '{http}'\n"));
    let out = crate::testing::sh(
        &format!("set -e; {RELEASE_SERVED}; {script}"),
        &[("PATH", &format!("{}:/usr/bin:/bin", bin.display()))],
    );
    std::fs::remove_dir_all(&bin).unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Runs [`KERNEL_BUILD`] on a machine running kernel `running`, whose package sources have
/// `packages`: its `$build` and `$reboot`, else its stderr.
#[cfg(unix)]
fn kernel_build(running: &str, packages: &[&str]) -> Result<(String, String), String> {
    let bin = crate::testing::fresh_dir("kernel");
    std::fs::create_dir_all(&bin).unwrap();
    let fake = |name: &str, body: String| {
        crate::testing::write_executable(bin.join(name), body);
    };
    fake("uname", format!("#!/bin/sh\necho '{running}'\n"));
    // `apt-cache policy <package>`: a candidate when the sources have it, none otherwise, its
    // label translated (as apt does under pkexec, which keeps `LANG`) unless `LC_ALL=C`.
    let known = packages.iter().map(|p| format!("'{p}'")).collect::<Vec<_>>().join(" ");
    fake(
        "apt-cache",
        format!(
            "#!/bin/sh\nlabel='Candidat :'; [ \"$LC_ALL\" = C ] && label='Candidate:'\nfor p in {known}; do [ \"$2\" = \"$p\" ] && {{ echo \"$p:\"; echo \"  $label 1.0-1\"; exit 0; }}; done\necho \"$2:\"; echo \"  $label (none)\"\n"
        ),
    );
    // Behind the prelude every root script starts with, under a French `LANG`.
    let out = crate::testing::sh(
        &format!("{}; {KERNEL_BUILD}; echo \"$build|$reboot\"", crate::install::root::PRELUDE),
        &[("PATH", &format!("{}:/usr/bin:/bin", bin.display())), ("LANG", "fr_FR.UTF-8")],
    );
    std::fs::remove_dir_all(&bin).unwrap();
    let said = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    let (build, reboot) = said.split_once('|').unwrap();
    Ok((build.to_owned(), reboot.to_owned()))
}

#[cfg(unix)]
#[test]
fn virtualbox_builds_its_driver_with_the_running_kernels_headers_or_waits_for_the_newer_kernel() {
    // Regression (a Debian 13 running 6.12.86 while its archive had 6.12.116): VirtualBox's
    // recommends brought linux-headers-amd64, the newest kernel's headers (and that kernel), never
    // the running one's, so its driver couldn't be built and VirtualBox read as installed with no
    // driver.
    let debian = "6.12.86+deb13-amd64";
    let headers = format!("linux-headers-{debian}");
    assert_eq!(
        kernel_build(debian, &[&headers, "linux-headers-amd64"]).unwrap(),
        ("gcc make perl kmod linux-headers-6.12.86+deb13-amd64 linux-headers-amd64".into(), String::new())
    );
    // Gone from the sources: the flavour's kernel and headers, and a restart.
    assert_eq!(
        kernel_build(debian, &["linux-headers-amd64"]).unwrap(),
        ("gcc make perl kmod linux-image-amd64 linux-headers-amd64".into(), "1".into())
    );
    // A flavour of its own (Debian's cloud kernel), Ubuntu's, Kali's.
    assert_eq!(
        kernel_build("6.12.86+deb13-cloud-amd64", &["linux-headers-cloud-amd64", "linux-headers-amd64"])
            .unwrap()
            .0,
        "gcc make perl kmod linux-image-cloud-amd64 linux-headers-cloud-amd64"
    );
    assert_eq!(
        kernel_build("6.8.0-45-generic", &["linux-headers-6.8.0-45-generic", "linux-headers-generic"])
            .unwrap()
            .0,
        "gcc make perl kmod linux-headers-6.8.0-45-generic linux-headers-generic"
    );
    assert_eq!(
        kernel_build("6.12.25-amd64", &["linux-headers-amd64"]).unwrap(),
        ("gcc make perl kmod linux-image-amd64 linux-headers-amd64".into(), "1".into())
    );
    // Neither: it stops, saying so, before anything is installed.
    let none = kernel_build(debian, &[]).unwrap_err();
    assert!(
        none.starts_with("VirtualBox builds its kernel driver with the running kernel's headers (linux-headers-6.12.86+deb13-amd64)"),
        "{none}"
    );
}

#[test]
fn apt_cache_policy_is_read_in_english_whatever_the_language() {
    // Regression: under `LANG=fr_FR.UTF-8` apt prints `Candidat :`, so the `Candidate:` grep
    // never matched: the kernel headers read as missing and Ubuntu's own VirtualBox was missed.
    // The prelude, which every policy read comes after, sets the C locale.
    let s = virtualbox_script();
    let prelude = crate::install::root::PRELUDE;
    assert!(prelude.ends_with("export LC_ALL=C") && s.starts_with(prelude), "{s}");
    assert_eq!(s.matches("apt-cache policy").count(), 3, "{s}");
}

#[test]
fn virtualbox_installs_without_its_recommends_with_what_builds_its_driver() {
    // Regression: its recommends brought a PDF viewer, Qt's translations and the newest kernel (325
    // MB, 154 packages on a bare Debian 13), and no driver.
    let s = virtualbox_script();
    assert!(s.contains("apt-get install -y --no-install-recommends virtualbox $build;"), "{s}");
    assert!(s.contains(r#"apt-get install -y --no-install-recommends "$pkg" $build;"#), "{s}");
    assert!(
        !s.contains("apt-get install -y virtualbox") && !s.contains(r#"apt-get install -y "$pkg""#),
        "{s}"
    );
    assert!(s.find(KERNEL_BUILD).unwrap() < s.find("--no-install-recommends").unwrap(), "{s}");
    assert!(s.ends_with("restart the computer to start it, and VirtualBox's driver is built then.\""), "{s}");
    assert!(crate::testing::sh_parses(&s), "{s}");
}

#[cfg(unix)]
#[test]
fn a_repository_check_says_no_packages_only_when_the_release_has_none() {
    // Regression: any curl failure (curl missing, no network) read as "no packages for trixie".
    let s = apt_repo_script(&HASHICORP);
    let snippet = s
        .get(s.find("; release_served \"$repo\"").unwrap() + 2..s.find("; command -v gpg").unwrap())
        .unwrap();
    let check = |http: &str| with_release_answer(http, &format!("repo=https://apt.example; codename=trixie; {snippet}; echo went-on"));
    assert_eq!(check("200").0.trim(), "went-on");
    let missing = check("404").1;
    assert!(missing.starts_with("HashiCorp has no packages for trixie yet"), "{missing}");
    let unreachable = check("000").1;
    assert!(
        unreachable.starts_with("Couldn't reach https://apt.example (HTTP 000)") && !unreachable.contains("no packages"),
        "{unreachable}"
    );
}

#[cfg(unix)]
#[test]
fn a_debian_derivative_the_vendor_doesnt_serve_takes_its_debian_bases_packages() {
    // Regression: Kali (kali-rolling) stopped at "HashiCorp has no packages for kali-rolling
    // yet"; HashiCorp serves trixie and bookworm.
    let s = apt_repo_script(&HASHICORP);
    let snippet = s
        .get(s.find("; release_served \"$repo\"").unwrap() + 2..s.find("; command -v gpg").unwrap())
        .unwrap();
    let bin = crate::testing::fresh_dir("curl");
    std::fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("curl");
    crate::testing::write_executable(&fake, "#!/bin/sh\ncase \"$*\" in */dists/trixie/*) printf 200 ;; *) printf 404 ;; esac\n");
    let run = |vars: &str| {
        let out = crate::testing::sh(
            &format!("set -e; {RELEASE_SERVED}; repo=https://apt.example; {vars}; {snippet}; echo \"went-on $codename\""),
            &[("PATH", &format!("{}:/usr/bin:/bin", bin.display()))],
        );
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (out, _) = run("codename=kali-rolling; debian_base='trixie bookworm'");
    assert!(
        out.contains("has no packages for kali-rolling: installing its trixie ones") && out.trim_end().ends_with("went-on trixie"),
        "{out}"
    );
    let (rolling, rolling_err) = run("codename=kali-rolling; debian_base=''");
    assert!(
        !rolling.contains("went-on") && rolling_err.starts_with("HashiCorp has no packages for kali-rolling yet"),
        "{rolling} / {rolling_err}"
    );
    let (out_noble, err_noble) = run("codename=noble; debian_base='trixie bookworm'; unset debian_base");
    assert!(
        !out_noble.contains("went-on") && err_noble.starts_with("HashiCorp has no packages for noble yet"),
        "{out_noble} / {err_noble}"
    );
    std::fs::remove_dir_all(&bin).unwrap();
}

#[cfg(unix)]
#[test]
fn dockers_repository_check_gives_the_distributions_docker_only_on_a_404() {
    // Regression: any failure of Docker's probe (DNS, a proxy, a timeout) installed docker.io,
    // saying Docker's repository had no packages for the release.
    let dir = crate::testing::fresh_dir("docker-probe");
    std::fs::create_dir_all(&dir).unwrap();
    let release = dir.join("os-release");
    std::fs::write(&release, "ID=debian\nVERSION_CODENAME=trixie\n").unwrap();
    let probe = DOCKER_DIST
        .replace("/etc/os-release", &release.display().to_string())
        .replace("command -v apt-get >/dev/null", "true");
    let check = |http: &str| with_release_answer(http, &format!("{probe}; echo \"$dist/$codename/$own\""));
    assert_eq!(check("200").0.trim(), "debian/trixie/");
    assert_eq!(check("404").0.trim(), "debian/trixie/1");
    for unreachable in ["000", "407", "503"] {
        let (out, err) = check(unreachable);
        assert!(out.is_empty(), "went on after HTTP {unreachable}: {out}");
        assert!(
            err.starts_with(&format!("Couldn't reach https://download.docker.com/linux/debian (HTTP {unreachable})")),
            "{err}"
        );
    }
    // The script asks once, and defines the probe before it asks.
    let s = docker_script(None);
    assert_eq!(s.matches(NEED_CURL).count(), 1, "{s}");
    assert_eq!(s.matches("release_served()").count(), 1, "{s}");
    assert!(s.find("release_served()").unwrap() < s.find("! release_served").unwrap());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn vagrant_names_its_own_install_page() {
    assert!(
        vagrant_script().contains("https://developer.hashicorp.com/vagrant/install"),
        "not Terraform's page"
    );
}

#[test]
fn root_scripts_never_use_fixed_temp_paths_or_pipe_downloads_into_a_shell() {
    // Under pkexec the AWS CLI was unzipped into /tmp/aws and /tmp/aws/install run as root:
    // any local user could plant that file first.
    // Every download fails on an HTTP error (`-f`); the one curl that doesn't is the
    // repository check, which reads the status code itself (RELEASE_SERVED).
    const STATUS_PROBE: &str = "s -o /dev/null -w '%{http_code}'";
    assert!(RELEASE_SERVED.contains(&format!("curl -{STATUS_PROBE}")));
    for s in root_scripts() {
        assert!(!s.contains("/tmp"), "fixed temp path: {s}");
        assert!(!s.contains("| sh") && !s.contains("| bash"), "download piped into a shell: {s}");
        for call in s.split("curl -").skip(1) {
            assert!(
                call.starts_with('f') || call.starts_with(STATUS_PROBE),
                "a failed download must fail: curl -{call}"
            );
        }
        assert!(s.starts_with("set -e"), "{s}");
        assert!(crate::testing::sh_parses(&s), "not valid sh: {s}");
    }
    for s in root_scripts() {
        assert!(s.contains(r#"d="$(mktemp -d)""#) && s.contains("trap 'rm -rf \"$d\"' EXIT"), "{s}");
    }
    assert!(awscli_script().contains(r#""$d/aws/install" --update"#) && awscli_script().contains("unzip -tq"));
}
