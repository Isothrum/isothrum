//! Placing a picked folder: what is refused, what is copied and checked, and what is left.

use std::time::{Duration, SystemTime};

use isothrum_runner::fake::{Call, FakeProcesses, Reply};

use super::*;

#[tokio::test]
async fn a_signed_download_is_copied_into_the_callers_folder_and_the_download_left_as_it_was() {
    let d = Dirs::new("signed");
    let picked = download(&d.picked());
    let before = files(&picked);
    let (cx, fake) = on(HostOs::Macos, &d.root(), vmware_signed());
    let outcome = place(&cx, &picked, &d.dest()).await.unwrap();
    assert_eq!(placed_path(&outcome), d.dest().join(EXE));
    assert_eq!(files(&d.dest()), before, "the whole tool is copied");
    assert_eq!(files(&picked), before, "the download is only read");
    assert!(is_program(&d.dest().join(EXE)), "the copy is still a program");
    // The program and its library are both held to VMware's team, on the copy only.
    let calls = lines(&fake);
    let held = |file: &Path| {
        fake.calls().iter().any(|c| {
            c.program == CODESIGN
                && c.args.iter().any(|a| a.contains("certificate leaf[subject.OU] = \"EG7KH642X6\""))
                && c.args.last().is_some_and(|last| Path::new(last).ends_with(file))
        })
    };
    assert!(held(Path::new(EXE)) && held(&Path::new("lib").join("libvmomi.dylib")), "{calls:?}");
    let picked_text = picked.to_string_lossy().into_owned();
    assert!(
        !calls.iter().any(|c| c.contains(&picked_text)),
        "the download itself isn't run against: {calls:?}"
    );
    // Nothing of the copy's making is left beside it.
    assert_eq!(
        files(&d.tools()).iter().filter(|f| f.starts_with(".ovftool-")).count(),
        0,
        "{:?}",
        files(&d.tools())
    );
}

#[tokio::test]
async fn off_macos_nothing_is_signature_checked_nor_unquarantined() {
    for os in [HostOs::Linux, HostOs::Windows] {
        let d = Dirs::new("off-mac");
        let picked = d.picked();
        let exe = exe(os);
        write(&picked.join(exe), b"program");
        executable(&picked.join(exe));
        write(&picked.join("lib").join("libvmomi.so"), b"library");
        let (cx, fake) = on(os, &d.root(), vmware_signed());
        let outcome = place(&cx, &picked, &d.dest()).await.unwrap();
        assert_eq!(placed_path(&outcome), d.dest().join(exe));
        assert!(fake.calls().is_empty(), "{os:?}: {:?}", lines(&fake));
    }
}

/// The quarantine is lifted from the copy only, and only after every signature check passed.
#[tokio::test]
async fn the_quarantine_is_lifted_from_the_checked_copy_only() {
    let d = Dirs::new("quarantine");
    let picked = download(&d.picked());
    let (cx, fake) = on(HostOs::Macos, &d.root(), vmware_signed());
    place(&cx, &picked, &d.dest()).await.unwrap();
    let calls = fake.calls();
    let lifted: Vec<&Call> = calls.iter().filter(|c| c.program == XATTR).collect();
    assert_eq!(lifted.len(), 1, "{:?}", lines(&fake));
    assert_eq!(lifted[0].args[..3], ["-d", "-r", "com.apple.quarantine"]);
    let target = PathBuf::from(&lifted[0].args[3]);
    assert!(target.starts_with(d.tools()), "lifted from the copy: {}", target.display());
    assert!(!target.starts_with(&picked), "never from the download: {}", target.display());
    let last_check = calls.iter().rposition(|c| c.program == CODESIGN).unwrap();
    let lift = calls.iter().position(|c| c.program == XATTR).unwrap();
    assert!(last_check < lift, "lifted only once the checks passed: {:?}", lines(&fake));
}

/// What placing a signed download on a Mac whose `codesign` answers as `fake` comes to, once
/// checked that nothing was placed nor left behind, and the quarantine kept.
async fn refused_on_a_mac(tag: &str, fake: FakeProcesses) -> Outcome {
    let d = Dirs::new(tag);
    let picked = download(&d.picked());
    let (cx, fake) = on(HostOs::Macos, &d.root(), fake);
    let outcome = place(&cx, &picked, &d.dest()).await.unwrap();
    assert!(!d.dest().exists(), "nothing placed");
    assert!(!fake.calls().iter().any(|c| c.program == XATTR), "the quarantine stays: {:?}", lines(&fake));
    assert_eq!(files(&d.tools()), Vec::<String>::new(), "no copy left behind");
    outcome
}

#[tokio::test]
async fn a_tool_signed_by_another_team_is_refused_with_that_team_and_nothing_is_placed() {
    let fake = FakeProcesses::model(|call: &Call| {
        if call.args.iter().any(|a| a.starts_with("-R=")) {
            Reply::fail(3, "test-requirement: code failed to satisfy specified code requirement(s)")
        } else if call.args.first().map(String::as_str) == Some("-d") {
            Reply::ok("designated => identifier ovftool and anchor apple generic and certificate leaf[subject.OU] = ABCDE12345")
        } else {
            Reply::ok("")
        }
    });
    assert_eq!(
        refused_on_a_mac("other-team", fake).await,
        Outcome::refused(Refusal::NotVmware, Some("ABCDE12345".into()))
    );
}

#[tokio::test]
async fn an_unsigned_library_is_refused_with_what_codesign_said() {
    let fake = FakeProcesses::model(|call: &Call| {
        if call.program == CODESIGN && call.args.last().is_some_and(|f| f.ends_with("libvmomi.dylib")) {
            Reply::fail(1, "libvmomi.dylib: code object is not signed at all")
        } else {
            Reply::ok("")
        }
    });
    // The library as the folder names it, in this machine's own separators.
    let library = Path::new("lib").join("libvmomi.dylib");
    let said = format!("{}: libvmomi.dylib: code object is not signed at all", library.display());
    assert_eq!(refused_on_a_mac("unsigned", fake).await, Outcome::refused(Refusal::Unsigned, Some(said)));
}

#[tokio::test]
async fn a_codesign_that_cant_run_is_an_error_not_a_refusal() {
    let d = Dirs::new("no-codesign");
    let picked = download(&d.picked());
    let (cx, _fake) = on(HostOs::Macos, &d.root(), FakeProcesses::model(|_| Reply::missing()));
    let err = place(&cx, &picked, &d.dest()).await.unwrap_err();
    assert!(
        matches!(err, PlaceError::Tool(Failure::Missing { ref program }) if program == CODESIGN),
        "{err:?}"
    );
    assert!(!d.dest().exists());
}

#[tokio::test]
async fn a_folder_without_the_program_is_refused_before_anything_is_copied() {
    let d = Dirs::new("no-tool");
    let picked = d.picked();
    write(&picked.join("lib").join("libvmomi.dylib"), &thin(X86_64));
    write(&picked.join("README.txt"), b"VMware OVF Tool");
    let (cx, fake) = on(HostOs::Macos, &d.root(), vmware_signed());
    assert_eq!(place(&cx, &picked, &d.dest()).await.unwrap(), Outcome::refused(Refusal::NoTool, None));
    assert!(!d.tools().exists(), "nothing copied");
    assert!(fake.calls().is_empty(), "{:?}", lines(&fake));
}

#[tokio::test]
async fn the_program_alone_without_its_libraries_is_refused() {
    let d = Dirs::new("no-lib");
    let picked = d.picked();
    write(&picked.join(EXE), &thin(X86_64));
    executable(&picked.join(EXE));
    let (cx, _fake) = on(HostOs::Macos, &d.root(), vmware_signed());
    assert_eq!(place(&cx, &picked, &d.dest()).await.unwrap(), Outcome::refused(Refusal::NoLibraries, None));
    assert!(!d.tools().exists());
}

#[test]
fn the_libraries_beside_the_program_are_each_oss_own() {
    let d = Dirs::new("libs");
    let root = d.picked();
    write(&root.join("libssl.so.3"), b"");
    assert!(has_libraries(&root, HostOs::Linux));
    assert!(!has_libraries(&root, HostOs::Windows) && !has_libraries(&root, HostOs::Macos));
    write(&root.join("LIBSSL.DLL"), b"");
    assert!(has_libraries(&root, HostOs::Windows));
}

#[cfg(unix)]
#[tokio::test]
async fn a_program_that_isnt_executable_is_refused() {
    let d = Dirs::new("not-exec");
    let picked = download(&d.picked());
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(picked.join(EXE), std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let (cx, _fake) = on(HostOs::Macos, &d.root(), vmware_signed());
    assert_eq!(place(&cx, &picked, &d.dest()).await.unwrap(), Outcome::refused(Refusal::NoTool, None));
    assert!(!d.dest().exists());
}

#[tokio::test]
async fn the_one_folder_inside_the_picked_one_that_holds_the_tool_is_used() {
    // The Windows and Linux downloads unpack into an `ovftool` folder.
    let d = Dirs::new("nested");
    let picked = d.picked();
    download(&picked.join("ovftool"));
    let (cx, _fake) = on(HostOs::Macos, &d.root(), vmware_signed());
    let outcome = place(&cx, &picked, &d.dest()).await.unwrap();
    assert_eq!(placed_path(&outcome), d.dest().join(EXE));
    // Two candidates: which one is meant isn't guessed.
    download(&picked.join("ovftool-older"));
    assert_eq!(place(&cx, &picked, &d.dest()).await.unwrap(), Outcome::refused(Refusal::NoTool, None));
}

#[tokio::test]
async fn a_new_copy_replaces_the_earlier_one_whole() {
    let d = Dirs::new("replace");
    write(&d.dest().join("lib").join("libold.dylib"), &thin(X86_64));
    write(&d.dest().join(EXE), b"old");
    let picked = download(&d.picked());
    let (cx, _fake) = on(HostOs::Macos, &d.root(), vmware_signed());
    place(&cx, &picked, &d.dest()).await.unwrap();
    assert_eq!(files(&d.dest()), files(&picked), "nothing of the earlier copy is left");
    assert_eq!(
        files(&d.tools()),
        files(&picked).iter().map(|f| format!("ovftool/{f}")).collect::<Vec<_>>(),
        "no copy aside is left"
    );
}

#[tokio::test]
async fn a_refused_copy_keeps_the_earlier_one() {
    let d = Dirs::new("keep");
    write(&d.dest().join(EXE), b"the copy that works");
    let picked = download(&d.picked());
    let fake = FakeProcesses::model(|call: &Call| {
        if call.program == CODESIGN {
            Reply::fail(1, "code object is not signed at all")
        } else {
            Reply::ok("")
        }
    });
    let (cx, _fake) = on(HostOs::Macos, &d.root(), fake);
    assert!(matches!(
        place(&cx, &picked, &d.dest()).await.unwrap(),
        Outcome::Refused { reason: Refusal::Unsigned, .. }
    ));
    assert_eq!(std::fs::read(d.dest().join(EXE)).unwrap(), b"the copy that works");
}

#[test]
fn a_folder_far_bigger_than_the_tool_isnt_copied() {
    let d = Dirs::new("budget");
    let from = download(&d.picked());
    let to = d.top.join("copy");
    let err = copy_tree(&from, &to, &mut Budget { files: 2, bytes: MAX_BYTES }).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::FileTooLarge);
    let small = Budget { files: MAX_FILES, bytes: 10 };
    assert_eq!(
        copy_tree(&from, &d.top.join("copy2"), &mut { small }).unwrap_err().kind(),
        std::io::ErrorKind::FileTooLarge
    );
    copy_tree(&from, &d.top.join("copy3"), &mut Budget::default()).unwrap();
}

#[tokio::test]
async fn a_folder_far_bigger_than_the_tool_is_refused_and_nothing_is_left() {
    let d = Dirs::new("too-large");
    let picked = download(&d.picked());
    for n in 0..MAX_FILES {
        write(&picked.join("data").join(format!("{n}")), b"");
    }
    let (cx, fake) = on(HostOs::Macos, &d.root(), vmware_signed());
    assert_eq!(place(&cx, &picked, &d.dest()).await.unwrap(), Outcome::refused(Refusal::TooLarge, None));
    assert_eq!(lines(&fake), Vec::<String>::new());
    assert_eq!(files(&d.tools()), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn a_link_inside_the_folder_is_copied_as_a_link_and_one_leading_out_left_out() {
    let d = Dirs::new("links");
    let from = download(&d.picked());
    std::os::unix::fs::symlink("libvmomi.dylib", from.join("lib").join("libvmomi.1.dylib")).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", from.join("lib").join("passwd")).unwrap();
    std::os::unix::fs::symlink("../certs", from.join("lib").join("certs")).unwrap();
    let to = d.top.join("copy");
    copy_tree(&from, &to, &mut Budget::default()).unwrap();
    assert_eq!(
        std::fs::read_link(to.join("lib").join("libvmomi.1.dylib")).unwrap(),
        PathBuf::from("libvmomi.dylib")
    );
    assert!(std::fs::symlink_metadata(to.join("lib").join("passwd")).is_err());
    assert!(std::fs::symlink_metadata(to.join("lib").join("certs")).is_err());
}

/// Sets folder `dir`'s modification time to `to`. Unix changes it through any handle; Windows
/// opens a folder only with backup semantics, and changes its times only through a handle that
/// may write attributes (`File::open` asks for neither: "Access is denied").
fn backdate(dir: &Path, to: SystemTime) {
    #[cfg(windows)]
    let handle = {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        std::fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(dir)
            .unwrap()
    };
    #[cfg(not(windows))]
    let handle = std::fs::File::open(dir).unwrap();
    handle.set_modified(to).unwrap();
}

#[test]
fn a_leftover_of_an_interrupted_placement_is_removed_once_old() {
    let d = Dirs::new("leftovers");
    let leftover = d.tools().join(".ovftool-new-abc");
    std::fs::create_dir_all(&leftover).unwrap();
    std::fs::create_dir(d.dest()).unwrap();
    remove_leftovers(&d.tools());
    assert!(leftover.exists(), "a young one may be another placement's");
    backdate(&leftover, SystemTime::now() - LEFTOVER_AFTER - Duration::from_secs(1));
    remove_leftovers(&d.tools());
    assert!(!leftover.exists());
    assert!(d.dest().exists(), "the placed copy stays");
}
