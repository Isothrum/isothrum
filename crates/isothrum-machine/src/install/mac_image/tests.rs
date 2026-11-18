use super::*;

const BREW: &str = "/opt/homebrew/bin/brew";

/// The step's user half and root half.
fn halves(step: &Step) -> (&str, &str) {
    let [c, user, zero, root] = step.args.as_slice() else {
        panic!("{:?}", step.args)
    };
    assert_eq!((step.program.as_str(), c.as_str(), zero.as_str()), ("sh", "-c", "sh"));
    (user, root)
}

#[test]
fn every_vendor_install_is_one_step_whose_halves_parse_and_says_what_it_accepts() {
    for (dep, vendor) in [
        (Dependency::Docker, DOCKER),
        (Dependency::Vagrant, VAGRANT),
        (Dependency::Virtualbox, VIRTUALBOX),
        (Dependency::Utm, UTM),
        (Dependency::Awscli, AWS_CLI),
    ] {
        for elevation in [Elevation::Prompt, Elevation::Unattended { root: false }, Elevation::Unattended { root: true }] {
            let step = install(Some(BREW), dep, &vendor, elevation).unwrap();
            let (user, root) = halves(&step);
            assert!(crate::testing::sh_parses(user), "not valid sh: {user}");
            assert!(crate::testing::sh_parses(root), "not valid sh: {root}");
            match vendor.source {
                Source::Cask(cask) => assert!(user.contains(&format!("fetch --cask {cask}")), "{user}"),
                Source::Url(url) => assert!(
                    user.contains(&format!("curl -fsSL --proto '=https' --tlsv1.2 '{url}'")) && !user.contains(BREW),
                    "{user}"
                ),
            }
            assert!(root.contains(&format!("({})", vendor.team)), "{root}");
            let opens = |script: &str| script.lines().any(|l| l.trim_start().starts_with("open "));
            assert!(!opens(user) && !opens(root), "no window is opened for its user to finish");
            assert_eq!(
                step.note,
                Some(Note::InstallingFromVendor {
                    dependency: dep,
                    accepts: vendor.accepts
                })
            );
        }
    }
    // Only Docker Desktop's install accepts a licence on its user's behalf, and says so.
    assert_eq!(DOCKER.accepts, Some(DOCKER_LICENCE));
    assert!([VAGRANT, VIRTUALBOX, UTM, AWS_CLI].iter().all(|v| v.accepts.is_none()));
    let docker = install(Some(BREW), Dependency::Docker, &DOCKER, Elevation::Prompt).unwrap();
    let (user, _) = halves(&docker);
    assert!(user.contains("accepts the Docker Subscription Service Agreement"), "{user}");
}

#[test]
fn each_elevation_runs_the_root_half_its_way_the_file_handed_over_as_an_argument() {
    let user = |elevation| {
        let step = install(Some(BREW), Dependency::Utm, &UTM, elevation).unwrap();
        halves(&step).0.to_owned()
    };
    let prompt = user(Elevation::Prompt);
    assert!(
        prompt.contains("with administrator privileges") && prompt.contains("quoted form of (item 2 of argv)"),
        "{prompt}"
    );
    assert!(prompt.contains(r#""$1" "$f" "$(id -un)""#), "{prompt}");
    assert!(user(Elevation::Unattended { root: false }).contains(r#"sudo -n /bin/sh -c "$1" sh "$f" "$(id -un)" /Applications"#));
    let root = user(Elevation::Unattended { root: true });
    assert!(root.contains(r#"/bin/sh -c "$1" sh "$f""#) && !root.contains("sudo") && !root.contains("osascript"));
}

#[test]
fn a_cask_needs_homebrew_and_the_aws_clis_package_from_amazon_does_not() {
    assert_eq!(
        install(None, Dependency::Vagrant, &VAGRANT, Elevation::Prompt).unwrap_err(),
        super::super::Refusal::NoHomebrew
    );
    let aws = install(None, Dependency::Awscli, &AWS_CLI, Elevation::Prompt).unwrap();
    let (user, root) = halves(&aws);
    assert!(
        user.contains("https://awscli.amazonaws.com/AWSCLIV2.pkg") && user.contains(r#"f="$w/AWSCLIV2.pkg""#),
        "{user}"
    );
    assert!(root.contains("Developer ID Installer: .*(94KV3E626L)"), "Amazon's team, pinned: {root}");
}

#[cfg(unix)]
#[test]
fn a_failed_cask_fetch_updates_homebrew_and_retries_once() {
    // A Homebrew whose fetch always fails (an old one crashing on the cask data).
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("calls");
    let brew = dir.path().join("brew");
    crate::testing::write_executable(&brew, format!("#!/bin/sh\necho \"$1\" >> '{}'\n[ \"$1\" = update ]\n", log.display()));
    let step = install(brew.to_str(), Dependency::Vagrant, &VAGRANT, Elevation::Prompt).unwrap();
    let out = crate::testing::sh(&step.args[1], &[]);
    assert!(!out.status.success());
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "fetch\nupdate\nfetch\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("Run 'brew update' in Terminal"));
}

/// The stand-ins for the programs the root half runs, installed once for every test and run
/// (macOS checks each new program on its first run): each logs its call (`program args`) to
/// `$LOG` and fails when `$REFUSE` names it. `hdiutil attach` mounts a copy of `$FIXTURE` in the
/// folder `-mountrandom` names, `pkgutil` names `$TEAM`'s Developer ID, `installer` puts
/// `VirtualBox.app` in `$APPS`, `ditto` copies; `install` is Docker's installer, which puts
/// `Docker.app` in `$APPS`; `aws` answers its version.
#[cfg(unix)]
const STAND_INS: [(&str, &str); 9] = [
    (
        "hdiutil",
        r#"[ "$1" = attach ] || exit 0
while [ $# -gt 0 ]; do [ "$1" = -mountrandom ] && r="$2"; shift; done
mkdir "$r/mnt" && cp -R "$FIXTURE/." "$r/mnt/" && printf '/dev/disk9s1\tApple_HFS\t%s\n' "$r/mnt""#,
    ),
    ("codesign", ""),
    ("spctl", ""),
    ("xattr", ""),
    ("pkgutil", r#"echo "   1. Developer ID Installer: Someone ($TEAM)""#),
    ("installer", r#"mkdir -p "$APPS/VirtualBox.app""#),
    ("ditto", r#"cp -R "$1" "$2""#),
    ("install", r#"mkdir -p "$APPS/Docker.app""#),
    ("aws", "echo aws-cli/2.41.0"),
];

/// Where a stand-in named `name` is installed.
#[cfg(unix)]
fn stand_in(name: &str) -> std::path::PathBuf {
    let (_, then) = STAND_INS.iter().find(|(n, _)| *n == name).unwrap();
    let body = format!("#!/bin/sh\necho \"{name} $*\" >> \"$LOG\"\nif [ \"$REFUSE\" = {name} ]; then exit 1; fi\n{then}\n");
    // Beside the test binary, under the build's target folder: kept from run to run.
    let root = std::env::current_exe().unwrap().parent().unwrap().join("isothrum-machine-stand-ins");
    isothrum_test_support::installed_executable(&root, name, &body).unwrap()
}

/// A root half's run: its folders (`fixture`, the disk image's contents; `apps`, the
/// Applications folder; `downloads`), the stand-ins first on `PATH`, `refuse` the one that fails
/// and `team` the Developer ID `pkgutil` names.
#[cfg(unix)]
struct Rig {
    dir: tempfile::TempDir,
    refuse: &'static str,
    team: &'static str,
}

#[cfg(unix)]
impl Rig {
    fn new(refuse: &'static str, team: &'static str) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["fixture", "apps", "downloads"] {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
        }
        Rig { dir, refuse, team }
    }

    fn path(&self, sub: &str) -> std::path::PathBuf {
        self.dir.path().join(sub)
    }

    /// Runs `vendor`'s root half on the download `file` (under `downloads`), for the user `ada`
    /// and the Applications folder `apps`: its exit code and error output.
    fn root_half(&self, vendor: &Vendor, file: &str) -> (Option<i32>, String) {
        let download = self.path("downloads").join(file);
        std::fs::write(&download, "the vendor's file").unwrap();
        let mut path: Vec<String> = STAND_INS
            .iter()
            .map(|(name, _)| stand_in(name).parent().unwrap().display().to_string())
            .collect();
        path.push("/usr/bin:/bin:/usr/sbin:/sbin".to_owned());
        let at = |sub: &str| self.path(sub).display().to_string();
        let out = crate::testing::sh(
            r#"sh -c "$ROOT" sh "$FILE" ada "$APPS""#,
            &[
                ("ROOT", &root_half(vendor)),
                ("FILE", &download.display().to_string()),
                ("APPS", &at("apps")),
                ("FIXTURE", &at("fixture")),
                ("LOG", &at("calls")),
                ("REFUSE", self.refuse),
                ("TEAM", self.team),
                ("PATH", &path.join(":")),
            ],
        );
        (out.status.code, String::from_utf8_lossy(&out.stderr).into_owned())
    }

    /// The programs called, in order, each with its first argument unless that is a path.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("calls"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.split_whitespace().take(2).filter(|w| !w.starts_with('/')).collect::<Vec<_>>().join(" "))
            .collect()
    }
}

#[cfg(unix)]
#[test]
fn an_app_in_a_disk_image_is_checked_copied_whole_checked_again_and_the_image_detached() {
    let rig = Rig::new("", UTM.team);
    std::fs::create_dir_all(rig.path("fixture/UTM.app/Contents")).unwrap();
    std::fs::write(rig.path("fixture/UTM.app/Contents/Info.plist"), "plist").unwrap();
    // A damaged copy from before (Finder's, cut short) is replaced.
    std::fs::create_dir_all(rig.path("apps/UTM.app/Contents")).unwrap();
    let (code, err) = rig.root_half(&UTM, "abc--UTM.dmg");
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(
        rig.calls(),
        [
            "hdiutil attach",
            "codesign --verify",
            "spctl --assess",
            "ditto",
            "codesign --verify",
            "spctl --assess",
            "xattr -dr",
            "hdiutil detach"
        ]
    );
    assert_eq!(std::fs::read_to_string(rig.path("apps/UTM.app/Contents/Info.plist")).unwrap(), "plist");
    let left: Vec<_> = std::fs::read_dir(rig.path("apps")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(left, ["UTM.app"], "no hidden copy left behind");
    let log = std::fs::read_to_string(rig.path("calls")).unwrap();
    assert!(log.contains("-nobrowse") && log.contains("-readonly"), "{log}");
    assert!(log.contains(r#"certificate leaf[subject.OU] = "WDNLXAD4W8""#), "{log}");
}

#[cfg(unix)]
#[test]
fn an_app_not_signed_by_its_vendor_is_never_copied_and_the_image_is_detached() {
    let rig = Rig::new("codesign", UTM.team);
    std::fs::create_dir_all(rig.path("fixture/UTM.app")).unwrap();
    let (code, err) = rig.root_half(&UTM, "abc--UTM.dmg");
    assert_eq!(code, Some(1));
    assert!(
        err.contains("isn't signed by Turing Software, LLC (WDNLXAD4W8). Nothing was installed."),
        "{err}"
    );
    assert_eq!(rig.calls(), ["hdiutil attach", "codesign --verify", "hdiutil detach"]);
    assert!(!rig.path("apps/UTM.app").exists());
}

#[cfg(unix)]
#[test]
fn a_package_in_a_disk_image_is_checked_installed_and_its_app_checked() {
    let rig = Rig::new("", VIRTUALBOX.team);
    std::fs::write(rig.path("fixture/VirtualBox.pkg"), "pkg").unwrap();
    let (code, err) = rig.root_half(&VIRTUALBOX, "abc--VirtualBox-7.2.4-OSX.dmg");
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(
        rig.calls(),
        [
            "hdiutil attach",
            "pkgutil --check-signature",
            "spctl --assess",
            "installer -pkg",
            "codesign --verify",
            "spctl --assess",
            "xattr -dr",
            "hdiutil detach"
        ]
    );
}

#[cfg(unix)]
#[test]
fn a_package_downloaded_as_is_is_installed_without_a_disk_image_and_one_signed_by_another_is_not() {
    let rig = Rig::new("", VAGRANT.team);
    let (code, err) = rig.root_half(&VAGRANT, "abc--vagrant.pkg");
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(rig.calls(), ["pkgutil --check-signature", "spctl --assess", "installer -pkg"]);

    let stranger = Rig::new("", "XXXXXXXXXX");
    let (refused, said) = stranger.root_half(&VAGRANT, "abc--vagrant.pkg");
    assert_eq!(refused, Some(1));
    assert!(said.contains("isn't signed by Hashicorp, Inc. (D38WU7D763)"), "{said}");
    assert_eq!(stranger.calls(), ["pkgutil --check-signature"]);
}

#[cfg(unix)]
#[test]
fn the_aws_clis_package_is_checked_installed_then_must_answer() {
    // The stand-in `aws`, where the package would put it.
    let answering = |refuse| {
        let rig = Rig::new(refuse, AWS_CLI.team);
        let vendor = Vendor {
            answers: Some("aws --version"),
            ..AWS_CLI
        };
        let (code, err) = rig.root_half(&vendor, "AWSCLIV2.pkg");
        (code, err, rig.calls())
    };
    let (code, err, calls) = answering("");
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(calls, ["pkgutil --check-signature", "spctl --assess", "installer -pkg", "aws --version"]);
    let (refused, said, _) = answering("aws");
    assert_eq!(refused, Some(1));
    assert!(said.contains("AWS CLI was installed, but `aws --version` doesn't answer."), "{said}");
    assert_eq!(AWS_CLI.answers, Some("/usr/local/aws-cli/aws --version"));
}

#[cfg(unix)]
#[test]
fn docker_desktop_is_installed_by_its_own_unattended_installer_for_the_user() {
    let rig = Rig::new("", DOCKER.team);
    let installer = rig.path("fixture/Docker.app/Contents/MacOS/install");
    std::fs::create_dir_all(installer.parent().unwrap()).unwrap();
    // Docker's installer, which copies the app into Applications itself (a link to the stand-in,
    // which the image's copy keeps a link).
    std::os::unix::fs::symlink(stand_in("install"), &installer).unwrap();
    let (code, err) = rig.root_half(&DOCKER, "abc--Docker.dmg");
    assert_eq!(code, Some(0), "{err}");
    let log = std::fs::read_to_string(rig.path("calls")).unwrap();
    assert!(log.contains("install --accept-license --user=ada\n"), "{log}");
    assert_eq!(
        rig.calls(),
        [
            "hdiutil attach",
            "codesign --verify",
            "spctl --assess",
            "install --accept-license",
            "codesign --verify",
            "spctl --assess",
            "xattr -dr",
            "hdiutil detach"
        ]
    );
}

#[cfg(unix)]
#[test]
fn an_unknown_download_installs_nothing() {
    let rig = Rig::new("", UTM.team);
    let (code, err) = rig.root_half(&UTM, "abc--UTM.zip");
    assert_eq!(code, Some(1));
    assert!(err.contains("neither a disk image nor a package"), "{err}");
    assert_eq!(rig.calls(), Vec::<String>::new());
}

#[tokio::test(start_paused = true)]
async fn a_copy_under_way_or_damaged_is_not_an_installed_app() {
    use isothrum_runner::fake::{FakeProcesses, Reply};
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("Docker.app");
    let at = app.display().to_string();
    let cx = |fake: FakeProcesses| Context::new(std::sync::Arc::new(fake), crate::testing::linux_host(dir.path()));
    // Not there: nothing is asked.
    assert!(!complete(&cx(FakeProcesses::script().build()), &app).await);
    std::fs::create_dir(&app).unwrap();
    // Finder's copy, cut short: its resumable-copy marks; its signature isn't even asked.
    let partial = FakeProcesses::script()
        .expect(
            &format!("xattr {at}"),
            Reply::ok("com.apple.quarantine\ncom.apple.metadata:kMDItemResumableCopy\ncom.apple.finder.copy.source.checksum#N"),
        )
        .build();
    assert!(!complete(&cx(partial), &app).await);
    // No marks, but a signature that doesn't verify: damaged.
    let damaged = FakeProcesses::script()
        .expect(&format!("xattr {at}"), Reply::ok("com.apple.quarantine"))
        .expect(
            &format!("codesign --verify --strict {at}"),
            Reply::fail(1, "a sealed resource is missing or invalid"),
        )
        .build();
    assert!(!complete(&cx(damaged), &app).await);
    let whole = FakeProcesses::script()
        .expect(&format!("xattr {at}"), Reply::ok(""))
        .expect(&format!("codesign --verify --strict {at}"), Reply::ok(""))
        .build();
    assert!(complete(&cx(whole), &app).await);
}
