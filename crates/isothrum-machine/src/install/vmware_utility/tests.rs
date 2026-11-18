use super::*;

#[test]
fn the_mac_install_checks_hashicorps_file_and_signature_before_installing_as_root() {
    let step = mac_install(Arch::Arm64, Elevation::Prompt);
    assert_eq!(step.program, "sh");
    let [c, user, zero, root] = step.args.as_slice() else {
        panic!("{:?}", step.args)
    };
    assert_eq!((c.as_str(), zero.as_str()), ("-c", "sh"));
    assert!(crate::testing::sh_parses(user), "not valid sh: {user}");
    assert!(crate::testing::sh_parses(root), "not valid sh: {root}");
    assert!(
        user.contains("https://releases.hashicorp.com/vagrant-vmware-utility/1.0.24/vagrant-vmware-utility_1.0.24_darwin_arm64.dmg"),
        "{user}"
    );
    assert!(
        user.contains("with administrator privileges") && user.contains("quoted form of (item 2 of argv)"),
        "{user}"
    );
    // As root, in this order: a copy root owns, its checksum, the signature, Gatekeeper, the install.
    let at = |needle: &str| root.find(needle).unwrap_or_else(|| panic!("{needle} missing: {root}"));
    assert!(at("cp \"$1\"") < at("shasum -a 256") && at("shasum -a 256") < at("pkgutil --check-signature"));
    assert!(at("pkgutil --check-signature") < at("spctl --assess --type install") && at("spctl --assess") < at("installer -pkg"));
    // Regression (an Intel Mac's install run): the service, restarted by launchd after the
    // install, answered 10 s after the step ended, and the run read it stopped. The step waits.
    assert!(at("installer -pkg") < at("until nc -z 127.0.0.1 9922"), "{root}");
    assert!(
        root.contains("Hashicorp, Inc. (D38WU7D763)") && root.contains("mktemp -d /private/tmp/"),
        "{root}"
    );
    // The image is the same file whatever the Mac's CPU: one checksum for both names.
    assert_eq!(mac_image(Arch::Arm64).1, mac_image(Arch::Amd64).1);
    assert!(mac_image(Arch::Amd64).0.ends_with("_darwin_amd64.dmg"));
    assert_eq!(step.note, Some(Note::DownloadingVmwareUtility { version: "1.0.24" }));
}

/// Regression (an Intel Mac's install run, over ssh): the root half always went through macOS's
/// prompt, which an ssh session can't show (`-60007`), so the install failed in a second where
/// `sudo -n` would have run it. Unattended, it goes through `sudo -n`.
#[test]
fn the_mac_install_unattended_runs_its_root_half_through_sudo() {
    let step = mac_install(Arch::Amd64, Elevation::Unattended { root: false });
    let user = &step.args[1];
    assert!(user.contains(r#"sudo -n /bin/sh -c "$1" sh "$d/utility.dmg""#), "{user}");
    assert!(!user.contains("osascript") && crate::testing::sh_parses(user), "{user}");
}

#[test]
fn every_pinned_checksum_is_a_sha256() {
    let sha = mac_image(Arch::Arm64).1;
    assert!(
        sha.len() == 64 && sha.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "{sha}"
    );
}

/// Runs the user's half of the install with `bin`'s stand-ins first on `PATH`, `true` as its root
/// half: its exit code and its error output.
#[cfg(target_os = "macos")]
fn run_users_half(script: &str, bin: &std::path::Path) -> (Option<i32>, String) {
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let out = crate::testing::run_env("sh", &["-c", script, "sh", "true"], &[("PATH", &path)]).expect("sh starts");
    (out.status.code, String::from_utf8_lossy(&out.stderr).into_owned())
}

#[cfg(target_os = "macos")]
#[test]
fn a_download_that_isnt_hashicorps_file_fails_before_any_prompt_and_a_declined_prompt_says_so() {
    let bin = tempfile::tempdir().unwrap();
    let curl = |body: &str| crate::testing::write_executable(bin.path().join("curl"), format!("#!/bin/sh\n{body}\n"));
    let osascript = |body: &str| crate::testing::write_executable(bin.path().join("osascript"), format!("#!/bin/sh\n{body}\n"));
    curl(r#"while [ $# -gt 0 ]; do [ "$1" = -o ] && echo tampered > "$2"; shift; done"#);
    osascript("echo prompted >&2; exit 0");
    let (file, sha256) = mac_image(Arch::Arm64);
    let (code, stderr) = run_users_half(&mac_script(&file, sha256, Elevation::Prompt), bin.path());
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("SHA-256 doesn't match") && !stderr.contains("prompted"), "{stderr}");
    // An empty download, whose checksum is pinned here, then a prompt the user declines.
    let empty_sha = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    curl(r#"while [ $# -gt 0 ]; do [ "$1" = -o ] && : > "$2"; shift; done"#);
    osascript("echo '0:0: execution error: User canceled. (-128)' >&2; exit 1");
    let (declined, said) = run_users_half(&mac_script("x.dmg", empty_sha, Elevation::Prompt), bin.path());
    assert_eq!(declined, Some(1), "{said}");
    assert!(said.contains("password prompt was cancelled"), "{said}");
}

#[test]
fn the_windows_install_checks_hashicorps_file_and_signature_and_elevates_through_uac() {
    let step = windows_install();
    assert_eq!(step.program, "powershell");
    let script = step.args.last().unwrap();
    assert!(script.contains("https://releases.hashicorp.com/vagrant-vmware-utility/1.0.24/vagrant-vmware-utility_1.0.24_windows_amd64.msi"));
    assert!(
        script.contains("-Verb RunAs") && script.contains("catch { exit 1223 }"),
        "a declined prompt reads as cancelled: {script}"
    );
    let elevated = windows_elevated(WINDOWS_MSI.1);
    let at = |needle: &str| elevated.find(needle).unwrap_or_else(|| panic!("{needle} missing: {elevated}"));
    assert!(
        at("Copy-Item") < at("Get-FileHash") && at("Get-FileHash") < at("Get-AuthenticodeSignature") && at("Get-AuthenticodeSignature") < at("msiexec.exe")
    );
    assert!(elevated.contains("'HashiCorp, Inc.'") && elevated.contains("$env:SystemRoot"), "{elevated}");
    // The elevated script is one PowerShell string inside the user's: every quote doubled.
    assert!(script.contains(&ps_quote(&elevated)));
    assert!(windows_uninstall().args.last().unwrap().contains(WINDOWS_PRODUCT_CODE));
    let msi = WINDOWS_MSI.1;
    assert!(
        msi.len() == 64 && msi.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "{msi}"
    );
    assert!(WINDOWS_MSI.0.contains(VERSION));
}
