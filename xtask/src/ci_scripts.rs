//! The tests of the scripts the workflows run (`.github/scripts/`), on Unix: they are Bash.

use std::process::Command;

use crate::repo;

/// `declared-args.sh` without its manifest fails, rather than print no argument as if the
/// manifest declared none.
#[test]
fn declared_args_refuses_a_missing_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new("bash")
        .arg(repo::root().join(".github/scripts/declared-args.sh"))
        .arg(dir.path().join("run.json"))
        .arg("param:region=eu-west-3")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("no run manifest"), "{out:?}");
}

/// A stand-in `llvm-objdump` that lists the DLLs in `$ISOTHRUM_TEST_DLLS` as an executable's
/// imports.
const OBJDUMP: &str = r#"#!/bin/sh
echo 'isothrum.exe: file format coff-x86-64'
for dll in $ISOTHRUM_TEST_DLLS; do echo "    DLL Name: $dll"; done
"#;

/// `no-vc-runtime.sh` over the stand-in `llvm-objdump` listing `dlls` as the executable's
/// imports: its exit status and what it printed. The stand-in is installed once under the build
/// folder's `tmp` (where cargo's `CARGO_TARGET_TMPDIR` is), never written by this process: a new
/// executable for each call waited, on a loaded Mac, for the system's check of its first run.
fn no_vc_runtime(dlls: &[&str]) -> std::io::Result<std::process::Output> {
    let exe = std::env::current_exe()?;
    let tmp = exe.ancestors().nth(3).ok_or_else(|| std::io::Error::other("the build folder"))?.join("tmp");
    let objdump = isothrum_test_support::installed_executable(&tmp, "llvm-objdump", OBJDUMP)?;
    Command::new("bash")
        .arg(repo::root().join(".github/scripts/no-vc-runtime.sh"))
        .arg(&objdump)
        .arg("isothrum.exe")
        .env("ISOTHRUM_TEST_DLLS", dlls.join(" "))
        .output()
}

/// `no-vc-runtime.sh` refuses an executable that imports the Visual C++ runtime (v2.1.1's
/// isothrum.exe imported VCRUNTIME140.dll, and a Windows without the redistributable couldn't
/// start it), and passes one with the C runtime linked in.
#[test]
fn no_vc_runtime_refuses_an_executable_that_imports_the_visual_cpp_runtime() {
    let windows = ["kernel32.dll", "ntdll.dll", "bcryptprimitives.dll", "WS2_32.dll", "USERENV.dll"];
    for runtime in ["VCRUNTIME140.dll", "vcruntime140_1.dll", "MSVCP140.dll"] {
        let out = no_vc_runtime(&[&windows[..], &[runtime, "api-ms-win-crt-heap-l1-1-0.dll"]].concat()).unwrap();
        assert_eq!(out.status.code(), Some(1), "{runtime}: {out:?}");
        assert!(String::from_utf8_lossy(&out.stdout).contains(&format!("imports {runtime}")), "{out:?}");
    }
    let linked_in = no_vc_runtime(&windows).unwrap();
    assert_eq!(linked_in.status.code(), Some(0), "{linked_in:?}");
}
