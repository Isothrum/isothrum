//! VMware's OVF Tool as it is installed here, and its placement into a folder of the caller's.
//! The tool itself is never downloaded (Broadcom's download is behind a licence): a placement is
//! tested from the copy installed here, and macOS's signature check on programs that aren't
//! VMware's.

use std::path::Path;

use isothrum_machine::HostOs;
use isothrum_machine::tools::ovftool::{self, Outcome, Refusal};

use crate::{answer, here};

/// Why a test needs the tool installed.
const NOT_HERE: &str = "VMware's OVF Tool isn't installed here (it comes with VMware Fusion or Workstation, or Broadcom's download behind a licence)";

/// The OVF Tool found here (in a hypervisor, or on `PATH`) answers its version.
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_ovftool_found_here_answers_its_version() {
    let cx = here().await;
    let path = std::env::var_os("PATH");
    let (tool, location) = ovftool::probe(&cx, None, path.as_deref()).await;
    println!("{tool:?} {location:?}");
    let Some(at) = location else { skip!("{NOT_HERE}") };
    if at.needs_rosetta {
        assert!(!tool.installed, "a tool that needs Rosetta doesn't run: {tool:?}");
        skip!("the OVF Tool at {} needs Rosetta, which isn't installed", at.path)
    }
    assert!(ovftool::usable(tool, Some(&at)).is_ok(), "{}", at.path);
}

/// The OVF Tool installed here, placed into a folder of the caller's, is found there and runs.
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_ovftool_installed_here_is_placed_into_a_folder_of_its_own() {
    let cx = here().await;
    let path = std::env::var_os("PATH");
    let Some((_, file)) = ovftool::locate(&cx, None, path.as_deref()) else {
        skip!("{NOT_HERE}")
    };
    let picked = file.parent().expect("its folder");
    let dir = tempfile::tempdir().expect("a folder");
    let dest = dir.path().join("tools").join("ovftool");
    let placed = ovftool::place(&cx, picked, &dest).await.expect("placed");
    println!("{placed:?}");
    assert!(matches!(placed, Outcome::Placed { .. }), "{placed:?}");
    let (source, found) = ovftool::locate(&cx, Some(&dest), None).expect("found");
    if !cx.host.devices.ovftool.iter().any(|f| f.is_file()) {
        assert_eq!(
            (source, found.as_path()),
            (ovftool::Source::Managed, dest.join(ovftool::exe(cx.host.os)).as_path())
        );
    }
    assert!(
        ovftool::version_of(&cx, &dest.join(ovftool::exe(cx.host.os))).await.installed,
        "the placed copy runs"
    );
}

/// A folder laid out as the tool's, holding a program `program` copied there as `ovftool`.
fn lookalike(dir: &Path, program: &Path) -> std::path::PathBuf {
    let root = dir.join("vmware-ovftool");
    std::fs::create_dir_all(root.join("lib")).expect("its folders");
    std::fs::copy(program, root.join("ovftool")).expect("the program");
    root
}

/// On macOS a program signed by another than VMware (Apple's own `true`) is refused, and so is
/// one not signed at all: `codesign` is asked, and nothing is placed.
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn a_program_not_signed_by_vmware_is_refused_on_macos() {
    let cx = here().await;
    if cx.host.os != HostOs::Macos {
        skip!("the signature check is macOS's: {:?} has none", cx.host.os)
    }
    let dir = tempfile::tempdir().expect("a folder");
    let dest = dir.path().join("tools").join("ovftool");
    let apples = lookalike(&dir.path().join("signed"), Path::new("/usr/bin/true"));
    let refused = ovftool::place(&cx, &apples, &dest).await.expect("checked");
    println!("{refused:?}");
    assert!(
        matches!(
            refused,
            Outcome::Refused {
                reason: Refusal::NotVmware,
                ..
            }
        ),
        "{refused:?}"
    );
    let unsigned = lookalike(&dir.path().join("unsigned"), Path::new("/usr/bin/true"));
    let program = unsigned.join("ovftool").display().to_string();
    answer(&cx, "/usr/bin/codesign", &["--remove-signature", &program])
        .await
        .expect("its signature removed");
    let stripped = ovftool::place(&cx, &unsigned, &dest).await.expect("checked");
    println!("{stripped:?}");
    assert!(matches!(stripped, Outcome::Refused { reason: Refusal::Unsigned, .. }), "{stripped:?}");
    assert!(!dest.exists(), "nothing placed");
}
