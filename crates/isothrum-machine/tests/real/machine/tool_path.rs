//! The tool path as an app started with a bare `PATH` works it out: every tool installed here in a
//! folder the crate knows is found on it. On Windows, the `PATH` the installers wrote to the
//! registry is read back.

use std::path::{Path, PathBuf};

use isothrum_machine::HostOs;
use isothrum_machine::host::tool_path::{self, Env};

use crate::here;

/// The tools the crate's callers start, by the name `PATH` finds them by.
const TOOLS: [&str; 7] = ["docker", "vagrant", "VBoxManage", "utmctl", "ovftool", "virsh", "colima"];

/// `name` as a program file on `os`.
fn program(os: HostOs, name: &str) -> String {
    if os == HostOs::Windows { format!("{name}.exe") } else { name.to_owned() }
}

/// The folder of `dirs` where `name` is, the first.
fn found_in<'a>(dirs: &'a [PathBuf], os: HostOs, name: &str) -> Option<&'a Path> {
    dirs.iter().map(PathBuf::as_path).find(|d| d.join(program(os, name)).is_file())
}

/// The `PATH` an app opened from the desktop starts with, on `os`.
fn bare(os: HostOs) -> Vec<PathBuf> {
    match os {
        HostOs::Windows => vec![PathBuf::from(r"C:\Windows\system32")],
        _ => ["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(PathBuf::from).to_vec(),
    }
}

/// Each tool this process finds in one of the folders the crate knows is found on the path
/// worked out from a bare `PATH`; a tool found elsewhere only (a folder of the user's own) is
/// said, not required.
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_tools_installed_here_are_on_the_path_an_app_works_out() {
    let cx = here().await;
    let os = cx.host.os;
    let mine = Env::here();
    let env = Env {
        path: bare(os),
        ..mine.clone()
    };
    let worked_out = match os {
        HostOs::Windows => tool_path::refreshed(&cx, &env).await,
        _ => tool_path::augmented(os, &env, &[]),
    }
    .unwrap_or_else(|| env.path.clone());
    let known: Vec<PathBuf> = tool_path::tool_dirs(os, &mine).into_iter().chain(bare(os)).collect();
    let mut checked = Vec::new();
    for name in TOOLS {
        let Some(at) = found_in(&mine.path, os, name).or_else(|| found_in(&known, os, name)) else {
            continue;
        };
        if !known.iter().any(|d| d == at) && os != HostOs::Windows {
            println!("{name}: in {} only, a folder of this account's own", at.display());
            continue;
        }
        println!("{name}: in {}", at.display());
        assert!(found_in(&worked_out, os, name).is_some(), "{name} (in {}) is on {worked_out:?}", at.display());
        checked.push(name);
    }
    if checked.is_empty() {
        skip!("none of {TOOLS:?} is installed here")
    }
}

/// On Windows, the registry's `PATH` is read back and expanded: the system's own folder, which it
/// names as `%SystemRoot%\system32`, is on the refreshed path though the process's has nothing.
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_registry_path_is_read_back_on_windows() {
    let cx = here().await;
    if cx.host.os != HostOs::Windows {
        skip!("the registry's PATH is Windows'")
    }
    let env = Env {
        path: Vec::new(),
        ..Env::here()
    };
    let refreshed = tool_path::refreshed(&cx, &env).await.expect("the registry's PATH");
    println!("{refreshed:#?}");
    let system = env.var("SystemRoot").map(|r| Path::new(r).join("system32")).expect("SystemRoot");
    let lower = |p: &Path| p.to_string_lossy().trim_end_matches('\\').to_lowercase();
    assert!(refreshed.iter().any(|d| lower(d) == lower(&system)), "{} is on {refreshed:?}", system.display());
}
