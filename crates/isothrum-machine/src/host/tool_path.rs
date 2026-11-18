//! The folders a process searches for the tools machine setup and a run start, worked out as a
//! value its caller puts on its own `PATH`: the crate never sets a variable.
//!
//! An app opened from macOS's Finder (and from some Linux desktops) starts with a minimal `PATH`
//! (`/usr/bin:/bin:/usr/sbin:/sbin`), not the user's shell's, so the folders the tools are
//! installed in (Homebrew's, Docker's, Vagrant's, the tools inside app bundles: Docker's CLI,
//! `utmctl`, Fusion's OVF Tool) are appended ([`augmented`]). The entries already there keep
//! priority. The folders of tools the caller places itself (a managed OVF Tool) come last, and
//! before they exist: a tool placed while the process runs is then found without a restart.
//!
//! On Windows the `PATH` is complete at start, but goes stale: an installer (winget, Docker
//! Desktop, VirtualBox) writes the new one to the registry, which a running process never reads
//! again. [`refreshed`] reads it back (`reg query`, machine then user, its `%VAR%`s expanded
//! with the caller's variables) and merges it in, with the tool folders that exist now.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use isothrum_runner::ProcessSpec;

use super::HostOs;
use crate::context::Context;

/// How long one `reg query` of the registry's `Path` may take.
const REG_TIMEOUT: Duration = Duration::from_secs(10);

/// The registry keys whose `Path` value a refresh reads, machine then user.
const REGISTRY_PATHS: [&str; 2] = [r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment", r"HKCU\Environment"];

/// What the tool path is worked out from: the process's `PATH`, the user's home folder and the
/// variables it reads (Program Files, the system drive, VirtualBox's install folder, and any a
/// registry `PATH` names), as values, so a test hands its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    /// The `PATH`'s folders, in order.
    pub path: Vec<PathBuf>,
    /// The user's home folder.
    pub home: Option<PathBuf>,
    /// The variables, by name.
    pub vars: Vec<(String, OsString)>,
}

impl Env {
    /// This process's: its `PATH` split as this OS splits it, its home (`HOME`, else
    /// `USERPROFILE`) and its variables (those with a UTF-8 name).
    #[must_use]
    pub fn here() -> Env {
        let path = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .filter(|h| !h.is_empty())
            .map(PathBuf::from);
        let vars = std::env::vars_os().filter_map(|(k, v)| k.into_string().ok().map(|k| (k, v))).collect();
        Env { path, home, vars }
    }

    /// The value of variable `name`: its name compared regardless of case, as Windows does (the
    /// names read here are never two variables that differ in case alone).
    #[must_use]
    pub fn var(&self, name: &str) -> Option<&OsStr> {
        self.vars
            .iter()
            .rev()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_os_str())
            .filter(|v| !v.is_empty())
    }
}

/// The folders the tools are installed in on `os`, whether they exist or not ([`augmented`]
/// adds those that do).
#[must_use]
pub fn tool_dirs(os: HostOs, env: &Env) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if os != HostOs::Windows {
        dirs.extend(["/usr/local/bin", "/usr/local/sbin", "/opt/homebrew/bin", "/opt/homebrew/sbin"].map(PathBuf::from));
        if let Some(home) = &env.home {
            // Docker Desktop's per-user CLI install, and user-level installs.
            dirs.push(home.join(".docker/bin"));
            dirs.push(home.join(".local/bin"));
        }
    }
    match os {
        HostOs::Macos => dirs.extend(mac_bundle_dirs(env.home.as_deref())),
        HostOs::Windows => dirs.extend(windows_dirs(env)),
        HostOs::Linux => {}
    }
    dirs
}

/// The macOS tools that live inside app bundles: Docker's CLI, `utmctl` (in UTM, in
/// `/Applications` or the user's own `~/Applications`), and Fusion's OVF Tool.
fn mac_bundle_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/Applications/Docker.app/Contents/Resources/bin"),
        PathBuf::from("/Applications/UTM.app/Contents/MacOS"),
    ];
    dirs.extend(home.map(|h| h.join("Applications/UTM.app/Contents/MacOS")));
    dirs.push(PathBuf::from("/Applications/VMware Fusion.app/Contents/Library/VMware OVF Tool"));
    dirs.push(PathBuf::from("/Applications/VMware Fusion.app/Contents/Library"));
    dirs
}

/// The Windows tool folders. None of these installers adds itself to `PATH`: VirtualBox says
/// where it went in `VBOX_MSI_INSTALL_PATH` (first: a VirtualBox moved elsewhere wins), VMware
/// Workstation puts `vmrun` under the 32-bit Program Files and its OVF Tool in a folder of its
/// own, and QEMU's installer adds nothing either.
fn windows_dirs(env: &Env) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    dirs.extend(env.var("VBOX_MSI_INSTALL_PATH").map(PathBuf::from));
    if let Some(pf) = env.var("ProgramFiles").map(PathBuf::from) {
        dirs.push(pf.join("Oracle").join("VirtualBox"));
        dirs.push(pf.join("VMware").join("VMware OVF Tool"));
        dirs.push(pf.join("Vagrant").join("bin"));
        dirs.push(pf.join("Docker").join("Docker").join("resources").join("bin"));
        dirs.push(pf.join("qemu"));
    }
    if let Some(pf86) = env.var("ProgramFiles(x86)").map(PathBuf::from) {
        dirs.push(pf86.join("VMware").join("VMware Workstation"));
    }
    for pf in ["ProgramFiles(x86)", "ProgramFiles"].into_iter().filter_map(|v| env.var(v)) {
        dirs.push(crate::tools::ovftool::workstation_dir(Path::new(pf)));
    }
    dirs
}

/// The folders added even before they exist, so a tool installed there while the process runs
/// is found without a restart: on Linux the per-user Google Cloud SDK earlier installs made, on
/// Windows Vagrant's folder under the system drive (where its installer puts it).
#[must_use]
pub fn future_dirs(os: HostOs, env: &Env) -> Vec<PathBuf> {
    match os {
        HostOs::Linux => env.home.iter().map(|h| h.join("google-cloud-sdk/bin")).collect(),
        HostOs::Windows => {
            let drive = env.var("SystemDrive").map_or_else(|| "C:".to_owned(), |d| d.to_string_lossy().into_owned());
            vec![PathBuf::from(format!("{drive}\\HashiCorp\\Vagrant\\bin"))]
        }
        HostOs::Macos => Vec::new(),
    }
}

/// Whether two `PATH` entries on `os` name the same folder: on Windows regardless of case and of
/// a trailing separator (`C:\Tools\` and `c:\tools` are one entry).
fn same_dir(os: HostOs, a: &Path, b: &Path) -> bool {
    if os == HostOs::Windows {
        let norm = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
        norm(a) == norm(b)
    } else {
        a == b
    }
}

/// `current` with each of `dirs` appended that it doesn't hold yet (an empty one never); None
/// when nothing was added. The entries already there keep their order, so they keep priority.
#[must_use]
pub fn appended(os: HostOs, current: &[PathBuf], dirs: impl IntoIterator<Item = PathBuf>) -> Option<Vec<PathBuf>> {
    let mut paths = current.to_vec();
    for dir in dirs {
        if !dir.as_os_str().is_empty() && !paths.iter().any(|p| same_dir(os, p, &dir)) {
            paths.push(dir);
        }
    }
    (paths.len() > current.len()).then_some(paths)
}

/// The `PATH` an app opened from Finder (or a desktop's launcher) needs: `env`'s, then each
/// [`tool_dirs`] folder that exists, each [`future_dirs`] one, then `own`, the folders of the
/// tools the caller places itself (which may not exist yet): a hypervisor's own copy of a tool
/// comes before the caller's. None when nothing is added. The caller joins it
/// (`std::env::join_paths`) and sets it.
#[must_use]
pub fn augmented(os: HostOs, env: &Env, own: &[PathBuf]) -> Option<Vec<PathBuf>> {
    augmented_where(os, env, own, Path::is_dir)
}

/// [`augmented`], `is_dir` saying which folders exist.
pub(crate) fn augmented_where(os: HostOs, env: &Env, own: &[PathBuf], is_dir: impl Fn(&Path) -> bool) -> Option<Vec<PathBuf>> {
    let existing = tool_dirs(os, env).into_iter().filter(|d| is_dir(d));
    appended(os, &env.path, existing.chain(future_dirs(os, env)).chain(own.iter().cloned()))
}

/// On Windows, `env`'s `PATH` with the one installers have since written to the registry
/// (machine, then user, expanded) and the tool folders that exist now merged in: each folder
/// that exists and isn't there yet, appended. None when nothing is added, and on every other OS
/// (no installer there changes a running process's `PATH`). Two `reg query` runs: cheap enough
/// before each machine check. A registry value `reg` can't read is left out.
pub async fn refreshed(cx: &Context, env: &Env) -> Option<Vec<PathBuf>> {
    refreshed_where(cx, env, Path::is_dir).await
}

/// [`refreshed`], `is_dir` saying which folders exist.
pub(crate) async fn refreshed_where(cx: &Context, env: &Env, is_dir: impl Fn(&Path) -> bool) -> Option<Vec<PathBuf>> {
    if cx.host.os != HostOs::Windows {
        return None;
    }
    let mut registry: Vec<PathBuf> = Vec::new();
    for key in REGISTRY_PATHS {
        let spec = ProcessSpec::new("reg", ["query", key, "/v", "Path"]);
        let Ok(captured) = cx.probe_within(REG_TIMEOUT, &spec).await else { continue };
        if !captured.success() {
            continue;
        }
        if let Some(value) = registry_value(&captured.stdout.join("\n")) {
            registry.extend(expanded(&value, env).split(';').map(str::trim).filter(|d| !d.is_empty()).map(PathBuf::from));
        }
    }
    let dirs: Vec<PathBuf> = registry.into_iter().chain(tool_dirs(HostOs::Windows, env)).filter(|d| is_dir(d)).collect();
    appended(HostOs::Windows, &env.path, dirs)
}

/// [`refreshed`] as a `PATH` value a Windows program is started with (`;` between its folders,
/// whatever this crate was built for): what a step runs with ([`crate::install::run_step`]), so
/// it finds a tool installed since this process started. None when nothing is added, and on
/// every other OS.
pub async fn refreshed_value(cx: &Context, env: &Env) -> Option<String> {
    refreshed_value_where(cx, env, Path::is_dir).await
}

/// [`refreshed_value`], `is_dir` saying which folders exist.
pub(crate) async fn refreshed_value_where(cx: &Context, env: &Env, is_dir: impl Fn(&Path) -> bool) -> Option<String> {
    let dirs = refreshed_where(cx, env, is_dir).await?;
    Some(dirs.iter().map(|d| d.to_string_lossy()).collect::<Vec<_>>().join(";"))
}

/// The value `reg query <key> /v Path` printed: the text after its type (`REG_EXPAND_SZ` or
/// `REG_SZ`) on the value's line, which may hold spaces.
fn registry_value(out: &str) -> Option<String> {
    out.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        if !words.next()?.eq_ignore_ascii_case("path") {
            return None;
        }
        let kind = words.next()?;
        if kind != "REG_EXPAND_SZ" && kind != "REG_SZ" {
            return None;
        }
        let (_, rest) = line.split_once(kind)?;
        Some(rest.trim().to_owned())
    })
}

/// `value` with each `%NAME%` replaced by `env`'s variable `NAME`, as Windows expands a
/// `REG_EXPAND_SZ`; one with no such variable, and a lone `%`, stay as written.
fn expanded(value: &str, env: &Env) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        let (before, from) = rest.split_at(start);
        out.push_str(before);
        let after = from.get(1..).unwrap_or_default();
        let Some(end) = after.find('%') else {
            out.push_str(from);
            return out;
        };
        let name = after.get(..end).unwrap_or_default();
        if let Some(v) = env.var(name).filter(|_| !name.is_empty()) {
            out.push_str(&v.to_string_lossy());
        } else {
            out.push('%');
            out.push_str(name);
            out.push('%');
        }
        rest = after.get(end + 1..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests;
