use std::path::{Path, PathBuf};
use std::sync::Arc;

use isothrum_runner::fake::{Call, FakeProcesses, Reply};

use super::*;
use crate::host::{Devices, Host};
use crate::testing::fresh_dir;

/// A Mach-O header: a thin 64-bit file for `cpu`.
fn thin(cpu: u32) -> Vec<u8> {
    let mut h = MH_MAGIC_64.to_le_bytes().to_vec();
    h.extend(cpu.to_le_bytes());
    h.resize(64, 0);
    h
}

const X86_64: u32 = 0x0100_0007;
const ARM64: u32 = 0x0100_000c;
const EXE: &str = "ovftool";

fn write(path: &Path, body: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// The OVF Tool as Broadcom's macOS download unpacks: the program, its libraries in `lib/`, and
/// data.
fn download(root: &Path) -> PathBuf {
    let exe = root.join(EXE);
    write(&exe, &thin(X86_64));
    executable(&exe);
    write(&root.join("lib").join("libvmomi.dylib"), &thin(X86_64));
    write(&root.join("env").join("en").join("messages.vmsg"), b"messages");
    write(&root.join("certs").join("cacert.pem"), b"-----BEGIN CERTIFICATE-----");
    root.to_path_buf()
}

/// Every file under `dir`, relative, sorted.
fn files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if e.file_type().unwrap().is_dir() {
            out.extend(files(&e.path()).into_iter().map(|f| format!("{name}/{f}")));
        } else {
            out.push(name);
        }
    }
    out.sort();
    out
}

/// A host on `os` and `arch`, its devices under `root`.
fn host(os: HostOs, arch: Arch, root: &Path) -> Host {
    Host {
        os,
        arch,
        packages: None,
        devices: Devices::under(root),
    }
}

/// A context on a host on `os` (Apple silicon, devices under `root`) whose tools `fake` answers;
/// the fake is kept to read its calls.
fn on(os: HostOs, root: &Path, fake: FakeProcesses) -> (Context, Arc<FakeProcesses>) {
    let fake = Arc::new(fake);
    let runner: Arc<dyn isothrum_runner::ProcessRunner> = Arc::<FakeProcesses>::clone(&fake);
    (Context::new(runner, host(os, Arch::Arm64, root)), fake)
}

/// `codesign` as macOS answers for VMware's own signature; `xattr` succeeds.
fn vmware_signed() -> FakeProcesses {
    FakeProcesses::model(|call: &Call| {
        if call.program == CODESIGN && call.args.first().map(String::as_str) == Some("-d") {
            Reply::ok(&format!(
                "designated => identifier ovftool and anchor apple generic and certificate leaf[subject.OU] = {VMWARE_TEAM}"
            ))
        } else {
            Reply::ok("")
        }
    })
}

fn lines(fake: &FakeProcesses) -> Vec<String> {
    fake.calls().iter().map(Call::line).collect()
}

fn placed_path(outcome: &Outcome) -> PathBuf {
    match outcome {
        Outcome::Placed { path, .. } => PathBuf::from(path),
        refused @ Outcome::Refused { .. } => panic!("refused: {refused:?}"),
    }
}

/// A test's folders: the picked download, the caller's tools folder (`dest`'s parent) and the
/// devices' root, all under one folder removed at the end.
struct Dirs {
    top: PathBuf,
}

impl Dirs {
    fn new(tag: &str) -> Dirs {
        Dirs {
            top: fresh_dir(&format!("ovftool-{tag}")),
        }
    }
    fn picked(&self) -> PathBuf {
        self.top.join("picked")
    }
    fn tools(&self) -> PathBuf {
        self.top.join("tools")
    }
    fn dest(&self) -> PathBuf {
        self.tools().join("ovftool")
    }
    fn root(&self) -> PathBuf {
        self.top.join("root")
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.top).ok();
    }
}

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
    let checked = |name: &str| {
        calls
            .iter()
            .any(|c| c.contains("anchor apple generic and certificate leaf[subject.OU] = \"EG7KH642X6\"") && c.ends_with(name))
    };
    assert!(checked(&format!("/{EXE}")) && checked("lib/libvmomi.dylib"), "{calls:?}");
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

#[tokio::test]
async fn a_tool_signed_by_another_team_is_refused_with_that_team_and_nothing_is_placed() {
    let d = Dirs::new("other-team");
    let picked = download(&d.picked());
    let fake = FakeProcesses::model(|call: &Call| {
        if call.args.iter().any(|a| a.starts_with("-R=")) {
            Reply::fail(3, "test-requirement: code failed to satisfy specified code requirement(s)")
        } else if call.args.first().map(String::as_str) == Some("-d") {
            Reply::ok("designated => identifier ovftool and anchor apple generic and certificate leaf[subject.OU] = ABCDE12345")
        } else {
            Reply::ok("")
        }
    });
    let (cx, fake) = on(HostOs::Macos, &d.root(), fake);
    let outcome = place(&cx, &picked, &d.dest()).await.unwrap();
    assert_eq!(
        outcome,
        Outcome::Refused {
            reason: Refusal::NotVmware,
            detail: Some("ABCDE12345".into())
        }
    );
    assert!(!d.dest().exists(), "nothing placed");
    assert!(!fake.calls().iter().any(|c| c.program == XATTR), "the quarantine stays: {:?}", lines(&fake));
    assert_eq!(files(&d.tools()), Vec::<String>::new(), "no copy left behind");
}

#[tokio::test]
async fn an_unsigned_library_is_refused_with_what_codesign_said() {
    let d = Dirs::new("unsigned");
    let picked = download(&d.picked());
    let fake = FakeProcesses::model(|call: &Call| {
        if call.program == CODESIGN && call.args.last().is_some_and(|f| f.ends_with("libvmomi.dylib")) {
            Reply::fail(1, "libvmomi.dylib: code object is not signed at all")
        } else {
            Reply::ok("")
        }
    });
    let (cx, fake) = on(HostOs::Macos, &d.root(), fake);
    let outcome = place(&cx, &picked, &d.dest()).await.unwrap();
    assert_eq!(
        outcome,
        Outcome::Refused {
            reason: Refusal::Unsigned,
            detail: Some("lib/libvmomi.dylib: libvmomi.dylib: code object is not signed at all".into())
        }
    );
    assert!(!d.dest().exists());
    assert!(!fake.calls().iter().any(|c| c.program == XATTR));
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
    assert_eq!(
        place(&cx, &picked, &d.dest()).await.unwrap(),
        Outcome::Refused {
            reason: Refusal::NoTool,
            detail: None
        }
    );
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
    assert_eq!(
        place(&cx, &picked, &d.dest()).await.unwrap(),
        Outcome::Refused {
            reason: Refusal::NoLibraries,
            detail: None
        }
    );
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
    assert_eq!(
        place(&cx, &picked, &d.dest()).await.unwrap(),
        Outcome::Refused {
            reason: Refusal::NoTool,
            detail: None
        }
    );
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
    assert_eq!(
        place(&cx, &picked, &d.dest()).await.unwrap(),
        Outcome::Refused {
            reason: Refusal::NoTool,
            detail: None
        }
    );
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
    assert_eq!(
        place(&cx, &picked, &d.dest()).await.unwrap(),
        Outcome::Refused {
            reason: Refusal::TooLarge,
            detail: None
        }
    );
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

#[test]
fn the_hypervisors_copy_comes_first_then_the_callers_then_the_path() {
    let d = Dirs::new("order");
    let root = d.root();
    let (cx, _fake) = on(HostOs::Macos, &root, FakeProcesses::model(|_| Reply::ok("")));
    let fusion = cx.host.devices.ovftool[0].clone();
    let managed = d.dest();
    let on_path = d.top.join("bin").join(EXE);
    let path = std::env::join_paths([d.top.join("empty"), d.top.join("bin")]).unwrap();
    let found = |folder: Option<&Path>| locate(&cx, folder, Some(&path));
    assert_eq!(found(Some(&managed)), None, "none yet");
    write(&on_path, b"");
    assert_eq!(found(Some(&managed)), Some((Source::Path, on_path.clone())));
    write(&managed.join(EXE), b"");
    assert_eq!(found(Some(&managed)), Some((Source::Managed, managed.join(EXE))));
    write(&fusion, b"");
    assert_eq!(found(Some(&managed)), Some((Source::Fusion, fusion.clone())));
    // No folder of the caller's: the hypervisor's, then the PATH.
    std::fs::remove_file(&fusion).unwrap();
    assert_eq!(found(None), Some((Source::Path, on_path)));
    assert_eq!(locate(&cx, None, None), None);
}

#[test]
fn the_hypervisor_copies_are_fusions_on_macos_and_workstations_on_windows() {
    let d = Dirs::new("bundled");
    let mac = Devices::under(&d.root());
    assert_eq!(
        mac.ovftool[0],
        d.root().join("Applications/VMware Fusion.app/Contents/Library/VMware OVF Tool/ovftool")
    );
    let (windows, _fake) = on(HostOs::Windows, &d.root(), FakeProcesses::model(|_| Reply::ok("")));
    let workstation = workstation_dir(&d.root().join("Program Files")).join("ovftool.exe");
    assert!(windows.host.devices.ovftool.contains(&workstation));
    write(&workstation, b"");
    assert_eq!(locate(&windows, None, None), Some((Source::Workstation, workstation)));
    // Workstation for Linux puts it in /usr/bin, on the PATH; macOS's own here has Fusion's.
    assert_eq!(Devices::here(HostOs::Linux).ovftool, Vec::<PathBuf>::new());
    assert!(
        Devices::here(HostOs::Macos)
            .ovftool
            .contains(&PathBuf::from("/Applications/VMware Fusion.app/Contents/Library/VMware OVF Tool/ovftool"))
    );
}

/// A check runs the found file's `--version` by its path, and says where it is.
#[tokio::test]
async fn the_check_asks_the_found_copy_its_version() {
    let d = Dirs::new("probe");
    let file = d.dest().join(EXE);
    write(&file, &thin(X86_64));
    let answers = FakeProcesses::script()
        .expect(&format!("{} --version", file.display()), Reply::ok("VMware ovftool 5.1.0 (build-24936374)\n"))
        .build();
    let (cx, _fake) = on(HostOs::Linux, &d.root(), answers);
    let (tool, location) = probe(&cx, Some(&d.dest()), None).await;
    assert_eq!((tool.installed, tool.version.as_deref()), (true, Some("VMware ovftool 5.1.0 (build-24936374)")));
    assert_eq!(
        location,
        Some(Location {
            source: Source::Managed,
            path: file.display().to_string(),
            needs_rosetta: false
        })
    );
    // One that doesn't run (Rosetta missing, Gatekeeper) reads as not installed.
    let (broken, _broken_fake) = on(HostOs::Linux, &d.root(), FakeProcesses::model(|_| Reply::fail(1, "Bad CPU type in executable")));
    let answer = version_of(&broken, &file).await;
    assert!(!answer.installed && answer.version.is_none());
    assert_eq!(probe(&broken, None, None).await, (Tool::MISSING, None));
}

#[test]
fn an_intel_only_tool_needs_rosetta_on_apple_silicon_until_it_is_installed() {
    let d = Dirs::new("rosetta");
    let intel = d.top.join("intel");
    let arm = d.top.join("arm");
    write(&intel, &thin(X86_64));
    write(&arm, &thin(ARM64));
    let fake = || Arc::new(FakeProcesses::model(|_| Reply::ok("")));
    let cx = |os, arch| Context::new(fake(), host(os, arch, &d.root()));
    let silicon = cx(HostOs::Macos, Arch::Arm64);
    assert!(needs_rosetta(&silicon, &intel));
    assert!(!needs_rosetta(&silicon, &arm));
    assert!(!needs_rosetta(&cx(HostOs::Macos, Arch::Amd64), &intel), "an Intel Mac runs it");
    assert!(!needs_rosetta(&cx(HostOs::Linux, Arch::Arm64), &intel), "macOS only");
    write(&silicon.host.devices.rosetta_runtime, b"");
    assert!(!needs_rosetta(&silicon, &intel), "Rosetta is installed");
}

#[test]
fn the_cpus_come_from_the_mach_o_header() {
    assert_eq!(cpus(&thin(X86_64)), [Cpu::X86_64]);
    assert_eq!(cpus(&thin(ARM64)), [Cpu::Arm64]);
    // A universal binary: big-endian, its count, then one 20-byte entry per CPU.
    let mut fat = FAT_MAGIC.to_be_bytes().to_vec();
    fat.extend(2u32.to_be_bytes());
    for cpu in [X86_64, ARM64] {
        let mut entry = cpu.to_be_bytes().to_vec();
        entry.resize(20, 0);
        fat.extend(entry);
    }
    assert_eq!(cpus(&fat), [Cpu::X86_64, Cpu::Arm64]);
    assert!(is_mach_o(&fat) && is_mach_o(&thin(X86_64)));
    assert_eq!(cpus(b"#!/bin/sh\n"), []);
    assert!(!is_mach_o(b"#!/bin/sh\n") && !is_mach_o(b"MZ") && !is_mach_o(b""));
    assert!(intel_only(&cpus(&thin(X86_64))));
    assert!(!intel_only(&cpus(&fat)) && !intel_only(&cpus(&thin(ARM64))) && !intel_only(&[]));
}

#[test]
fn only_mach_o_files_are_signature_checked() {
    let d = Dirs::new("macho");
    let dir = download(&d.picked());
    let found: Vec<String> = mach_o_files(&dir)
        .iter()
        .map(|f| f.strip_prefix(&dir).unwrap().to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(found, ["lib/libvmomi.dylib".to_owned(), EXE.to_owned()]);
}

#[test]
fn the_signing_team_comes_from_the_designated_requirement() {
    let dr = "designated => identifier ovftool and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and certificate leaf[subject.OU] = EG7KH642X6\n";
    assert_eq!(signing_team(dr).as_deref(), Some("EG7KH642X6"));
    assert_eq!(
        signing_team("designated => certificate leaf[subject.OU] = \"ABCDE12345\"").as_deref(),
        Some("ABCDE12345")
    );
    assert_eq!(signing_team("designated => cdhash H\"0123\""), None);
}

#[test]
fn the_outcomes_are_tagged() {
    let placed = serde_json::to_value(Outcome::Placed {
        path: "/x/ovftool".into(),
        needs_rosetta: true,
    })
    .unwrap();
    assert_eq!(placed, serde_json::json!({ "outcome": "placed", "path": "/x/ovftool", "needsRosetta": true }));
    let refused = serde_json::to_value(Outcome::refused(Refusal::NotVmware, Some("ABCDE12345".into()))).unwrap();
    assert_eq!(
        refused,
        serde_json::json!({ "outcome": "refused", "reason": "notVmware", "detail": "ABCDE12345" })
    );
}

#[test]
fn usable_is_the_version_or_why_the_tool_cant_be_used() {
    let at = |needs_rosetta| Location {
        source: Source::Fusion,
        path: "/x/ovftool".into(),
        needs_rosetta,
    };
    let tool = |installed, version: Option<&str>| Tool {
        installed,
        version: version.map(str::to_owned),
    };
    assert_eq!(
        usable(tool(true, Some("VMware ovftool 5.1.0")), Some(&at(false))),
        Ok("VMware ovftool 5.1.0".into())
    );
    assert_eq!(
        usable(tool(false, None), Some(&at(true))),
        Err("VMware's OVF Tool at /x/ovftool is built for Intel Macs and needs Rosetta".into())
    );
    assert_eq!(
        usable(tool(false, None), Some(&at(false))),
        Err("VMware's OVF Tool at /x/ovftool doesn't run".into())
    );
    assert!(usable(tool(false, None), None).unwrap_err().contains("wasn't found"));
}

/// The real `xattr` lifts the quarantine a download carries from every file of the folder.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn lifting_the_quarantine_clears_it_from_every_file_of_the_folder() {
    let d = Dirs::new("xattr");
    let dir = download(&d.picked());
    let quarantined = |f: &Path| crate::testing::run(XATTR, &["-p", "com.apple.quarantine", f.to_str().unwrap()]).is_some_and(|o| o.status.success());
    for f in [dir.join(EXE), dir.join("lib").join("libvmomi.dylib")] {
        let set = crate::testing::run(XATTR, &["-w", "com.apple.quarantine", "0081;00000000;Safari;", f.to_str().unwrap()]).unwrap();
        assert!(set.status.success() && quarantined(&f));
    }
    let cx = Context::new(
        Arc::new(isothrum_runner::SystemProcesses::default()),
        host(HostOs::Macos, Arch::Arm64, &d.root()),
    );
    lift_quarantine(&cx, &dir).await.unwrap();
    assert!(!quarantined(&dir.join(EXE)) && !quarantined(&dir.join("lib").join("libvmomi.dylib")));
}
