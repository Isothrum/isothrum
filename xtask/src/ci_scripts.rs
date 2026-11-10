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
