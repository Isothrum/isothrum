use std::path::{Path, PathBuf};
use std::sync::Arc;

use isothrum_runner::fake::{Call, FakeProcesses, Reply};

use super::*;
use crate::host::{Devices, Host};
use crate::testing::fresh_dir;

mod place;

/// A Mach-O header: a thin 64-bit file for `cpu`.
pub(super) fn thin(cpu: u32) -> Vec<u8> {
    let mut h = mach_o::MH_MAGIC_64.to_le_bytes().to_vec();
    h.extend(cpu.to_le_bytes());
    h.resize(64, 0);
    h
}

pub(super) const X86_64: u32 = 0x0100_0007;
pub(super) const ARM64: u32 = 0x0100_000c;
pub(super) const EXE: &str = "ovftool";

pub(super) fn write(path: &Path, body: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

pub(super) fn executable(path: &Path) {
    #[cfg(unix)]
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    #[cfg(not(unix))]
    let _ = path;
}

/// The OVF Tool as Broadcom's macOS download unpacks: the program, its libraries in `lib/`, and
/// data.
pub(super) fn download(root: &Path) -> PathBuf {
    let exe = root.join(EXE);
    write(&exe, &thin(X86_64));
    executable(&exe);
    write(&root.join("lib").join("libvmomi.dylib"), &thin(X86_64));
    write(&root.join("env").join("en").join("messages.vmsg"), b"messages");
    write(&root.join("certs").join("cacert.pem"), b"-----BEGIN CERTIFICATE-----");
    root.to_path_buf()
}

/// Every file under `dir`, relative, sorted.
pub(super) fn files(dir: &Path) -> Vec<String> {
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
pub(super) fn host(os: HostOs, arch: Arch, root: &Path) -> Host {
    Host {
        os,
        arch,
        packages: None,
        devices: Devices::under(root),
    }
}

/// A context on a host on `os` (Apple silicon, devices under `root`) whose tools `fake` answers;
/// the fake is kept to read its calls.
pub(super) fn on(os: HostOs, root: &Path, fake: FakeProcesses) -> (Context, Arc<FakeProcesses>) {
    let fake = Arc::new(fake);
    let runner: Arc<dyn isothrum_runner::ProcessRunner> = Arc::<FakeProcesses>::clone(&fake);
    (Context::new(runner, host(os, Arch::Arm64, root)), fake)
}

/// `codesign` as macOS answers for VMware's own signature; `xattr` succeeds.
pub(super) fn vmware_signed() -> FakeProcesses {
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

pub(super) fn lines(fake: &FakeProcesses) -> Vec<String> {
    fake.calls().iter().map(Call::line).collect()
}

pub(super) fn placed_path(outcome: &Outcome) -> PathBuf {
    match outcome {
        Outcome::Placed { path, .. } => PathBuf::from(path),
        refused @ Outcome::Refused { .. } => panic!("refused: {refused:?}"),
    }
}

/// A test's folders: the picked download, the caller's tools folder (`dest`'s parent) and the
/// devices' root, all under one folder removed at the end.
pub(super) struct Dirs {
    pub(super) top: PathBuf,
}

impl Dirs {
    pub(super) fn new(tag: &str) -> Dirs {
        Dirs {
            top: fresh_dir(&format!("ovftool-{tag}")),
        }
    }
    pub(super) fn picked(&self) -> PathBuf {
        self.top.join("picked")
    }
    pub(super) fn tools(&self) -> PathBuf {
        self.top.join("tools")
    }
    pub(super) fn dest(&self) -> PathBuf {
        self.tools().join("ovftool")
    }
    pub(super) fn root(&self) -> PathBuf {
        self.top.join("root")
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.top).ok();
    }
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
