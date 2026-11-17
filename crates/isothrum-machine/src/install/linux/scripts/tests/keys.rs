use super::repos::{repo_scripts, root_scripts};
use super::*;

/// Whether `program` runs here (the gpg checks run for real where gpg is installed).
#[cfg(unix)]
fn have(program: &str) -> bool {
    crate::testing::run(program, &["--version"]).is_some_and(|o| o.status.success())
}

/// Runs `script` with `sh`, in a fresh directory `$d` holding `files`: its exit success and
/// its stderr.
#[cfg(unix)]
fn run_in_workdir(script: &str, files: &[(&str, &[u8])]) -> (bool, String) {
    let d = crate::testing::fresh_dir("script");
    std::fs::create_dir_all(&d).unwrap();
    for (name, body) in files {
        std::fs::write(d.join(name), body).unwrap();
    }
    let out = crate::testing::sh(&format!("set -e; d='{}'; {script}", d.display()), &[]);
    std::fs::remove_dir_all(&d).ok();
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn vendor_keys_are_pinned_and_checked_before_apt_trusts_them() {
    let vendors = repo_scripts().map(|(repo, _)| repo.fingerprint);
    for fpr in vendors.into_iter().chain([AWS_CLI_KEY_FINGERPRINT]) {
        assert!(
            fpr.len() == 40 && fpr.bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b)),
            "{fpr}"
        );
    }
    // The key is checked, then installed for apt: never fetched straight into the keyring.
    let check = |s: &str, fpr: &str, install: &str| {
        let checked = s.find(&format!(r#"[ "$k" = "{fpr}" ]"#)).unwrap_or_else(|| panic!("no fingerprint check: {s}"));
        let installed = s.find(install).unwrap_or_else(|| panic!("no install of the key: {s}"));
        assert!(s.find(r#"-o "$d/key""#).is_some_and(|fetched| fetched < checked) && checked < installed, "{s}");
    };
    for (repo, s) in repo_scripts() {
        check(&s, repo.fingerprint, &format!("-o /etc/apt/keyrings/{}.gpg", repo.name));
        assert!(!s.contains("gpg | gpg"), "{s}");
        assert!(s.contains(r#"--homedir "$d/gnupg""#) && !s.contains("--keyserver"), "{s}");
    }
    assert!(gcloud_script().contains(r#"repo="https://packages.cloud.google.com/apt"; "#));
}

#[test]
fn the_aws_cli_bundle_must_be_signed_by_the_embedded_key() {
    let s = awscli_script();
    assert!(AWS_CLI_KEY.starts_with("-----BEGIN PGP PUBLIC KEY BLOCK-----") && AWS_CLI_KEY.trim_end().ends_with("-----END PGP PUBLIC KEY BLOCK-----"));
    assert!(s.contains(AWS_CLI_KEY.trim_end()), "the key is in the script");
    assert!(s.contains(r#"curl -fsSL "$url.sig" -o "$d/awscliv2.zip.sig""#), "{s}");
    // Verified before it is unpacked, let alone run.
    let verified = s.find(&format!("VALIDSIG {AWS_CLI_KEY_FINGERPRINT} ")).unwrap();
    assert!(
        verified < s.find("unzip -tq").unwrap() && verified < s.find(r#""$d/aws/install""#).unwrap(),
        "{s}"
    );
    assert!(crate::testing::sh_parses(&s), "not valid sh: {s}");
}

#[test]
fn no_vendor_install_script_runs_as_root() {
    // Regression: Docker (on distros without its apt repository), Azure CLI and gcloud ran
    // the vendor's install script (get.docker.com, aka.ms/InstallAzureCLIDeb,
    // sdk.cloud.google.com), which fetches its own key: no fingerprint held it.
    for s in root_scripts() {
        for script in ["get.docker.com", "aka.ms/InstallAzureCLIDeb", "sdk.cloud.google.com", "install.sh"] {
            assert!(!s.contains(script), "{script} in {s}");
        }
    }
}

#[cfg(unix)]
#[test]
fn docker_without_its_apt_repository_stops_changing_nothing_with_its_code() {
    // Only the branch past the distro check: `dist` empty, nothing else run. Regression: the
    // slice began at the distro detection's own `[ -n "$dist" ]`, a syntax error that never ran
    // the refusal; bash echoes the failing line (URL included), so it passed on macOS only.
    let s = docker_script(None);
    let refuse = s.get(s.find("[ -n \"$dist\" ] ||").unwrap()..s.find("; if [ -n \"$own\" ]").unwrap()).unwrap();
    let out = crate::testing::sh(&format!("dist=''; {refuse}; echo installed"), &[]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code,
        Some(crate::install::steps::NOT_SERVED),
        "the refusal's own exit, not a syntax error: {err}"
    );
    assert!(!String::from_utf8_lossy(&out.stdout).contains("installed"));
    // What to do instead is the UI's to say ([`crate::install::StepFailed::NotServed`]).
    assert!(err.is_empty(), "{err}");
}

#[cfg(unix)]
#[test]
fn a_key_with_another_fingerprint_stops_the_install() {
    if !have("gpg") {
        eprintln!("no gpg here: the key check isn't run");
        return;
    }
    let pinned = format!(
        r#"mkdir -p -m 700 "$d/gnupg"; {}; echo checked >&2"#,
        check_keys(&[AWS_CLI_KEY_FINGERPRINT], "AWS")
    );
    let (ok, err) = run_in_workdir(&pinned, &[("key", AWS_CLI_KEY.as_bytes())]);
    assert!(ok && err.contains("checked"), "{err}");
    let other = format!(
        r#"mkdir -p -m 700 "$d/gnupg"; {}; echo checked >&2"#,
        check_keys(&[DOCKER.fingerprint], "Docker")
    );
    let (other_ok, other_err) = run_in_workdir(&other, &[("key", AWS_CLI_KEY.as_bytes())]);
    assert!(!other_ok && !other_err.contains("checked"), "{other_err}");
    assert!(
        other_err.contains(
            "Docker's signing key isn't the expected one (fingerprint 9DC858229FC7DD38854AE2D88D81803C0EBFCD88, got: FB5DB77FD5C118B80511ADA8A6310ACC4672475C)"
        ),
        "{other_err}"
    );
    let (portal_ok, portal_err) = run_in_workdir(&other, &[("key", b"<html>captive portal</html>")]);
    assert!(!portal_ok && portal_err.contains("got: )"), "{portal_err}");
}

#[cfg(unix)]
#[test]
fn an_aws_cli_bundle_with_a_bad_signature_is_not_installed() {
    if !have("gpg") || !have("gpgv") {
        eprintln!("no gpg here: the signature check isn't run");
        return;
    }
    let verify = format!("{}; echo verified >&2", aws_verify());
    let (ok, err) = run_in_workdir(&verify, &[("awscliv2.zip", b"PK not really"), ("awscliv2.zip.sig", b"not a signature")]);
    assert!(!ok && !err.contains("verified"), "{err}");
    assert!(
        err.contains("The AWS CLI download isn't signed by AWS's key (FB5DB77FD5C118B80511ADA8A6310ACC4672475C)"),
        "{err}"
    );
}

#[cfg(unix)]
#[test]
fn docker_picks_its_apt_repository_by_distro_or_the_distributions_own_docker() {
    let dir = crate::testing::fresh_dir("os-release");
    std::fs::create_dir_all(&dir).unwrap();
    let release = dir.join("os-release");
    assert!(DOCKER_DIST.contains(". /etc/os-release;") && DOCKER_DIST.contains("command -v apt-get >/dev/null"));
    let probe = DOCKER_DIST
        .replace("/etc/os-release", &release.display().to_string())
        .replace("command -v apt-get >/dev/null", "true");
    // Docker's repository as it is: the releases it serves, per distribution.
    let serves = r#"release_served() { case "$1/$2" in */debian/bookworm|*/debian/trixie|*/ubuntu/jammy|*/ubuntu/noble|*/raspbian/bookworm) return 0;; esac; return 1; }"#;
    let pick = |os: &str| {
        std::fs::write(&release, os).unwrap();
        let out = crate::testing::sh(&format!("set -e; {serves}; {probe}; echo \"$dist/$codename/$own\""), &[]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    assert_eq!(
        pick("ID=ubuntu\nID_LIKE=debian\nVERSION_CODENAME=noble\nUBUNTU_CODENAME=noble\n"),
        "ubuntu/noble/"
    );
    assert_eq!(
        pick("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\nVERSION_CODENAME=wilma\nUBUNTU_CODENAME=noble\n"),
        "ubuntu/noble/"
    );
    assert_eq!(pick("ID=debian\nVERSION_CODENAME=trixie\n"), "debian/trixie/");
    assert_eq!(pick("ID=raspbian\nID_LIKE=debian\nVERSION_CODENAME=bookworm\n"), "raspbian/bookworm/");
    assert_eq!(pick("ID=raspbian\nID_LIKE=debian\nVERSION_CODENAME=trixie\n"), "debian/trixie/");
    // A Debian derivative on the Debian release it names (LMDE): Docker's Debian repository.
    assert_eq!(
        pick("ID=linuxmint\nID_LIKE=debian\nVERSION_CODENAME=faye\nDEBIAN_CODENAME=bookworm\n"),
        "debian/bookworm/"
    );
    // Regression: Kali (and Devuan) had no repository at all and the install stopped. Docker
    // serves neither release: the distribution's own Docker.
    assert_eq!(
        pick("ID=kali\nID_LIKE=debian\nVERSION_CODENAME=kali-rolling\nVERSION_ID=\"2025.4\"\n"),
        "debian/kali-rolling/1"
    );
    assert_eq!(pick("ID=devuan\nID_LIKE=debian\nVERSION_CODENAME=excalibur\n"), "debian/excalibur/1");
    // A release newer than Docker serves yet: the distribution's own too.
    assert_eq!(pick("ID=debian\nVERSION_CODENAME=forky\n"), "debian/forky/1");
    assert_eq!(
        pick("ID=ubuntu\nID_LIKE=debian\nVERSION_CODENAME=resolute\nUBUNTU_CODENAME=resolute\n"),
        "ubuntu/resolute/1"
    );
    // Neither Debian nor Ubuntu: nothing from here.
    assert_eq!(pick("ID=fedora\nVERSION_ID=43\n"), "//");
    assert_eq!(pick("ID=opensuse-tumbleweed\nID_LIKE=\"opensuse suse\"\n"), "//");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn where_dockers_repository_has_no_release_the_distributions_own_docker_and_compose_go_in() {
    let s = docker_script(Some("ada"));
    let own = s.get(s.find("if [ -n \"$own\" ]; then").unwrap()..s.find("; else repo=").unwrap()).unwrap();
    assert!(
        own.contains("apt-get install -y docker.io") && own.contains("docker-compose-v2") && own.contains("apt-get install -y docker-compose"),
        "{own}"
    );
    assert!(!own.contains("docker-ce") && !own.contains("download.docker.com/linux/$dist"), "{own}");
    // Either way the user joins the docker group, and the script parses.
    assert!(
        s.ends_with("echo 'Added ada to the docker group: log out and back in (or restart) for it to apply.'"),
        "{s}"
    );
    assert!(crate::testing::sh_parses(&s), "not valid sh: {s}");
}
