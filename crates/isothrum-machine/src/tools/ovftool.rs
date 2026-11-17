//! VMware's OVF Tool (`ovftool`), which the ESXi Vagrant plugin uploads VMs with. It isn't
//! installed the way the other tools are: it comes inside VMware Fusion (Workstation on Windows),
//! or as a standalone download from Broadcom, which unpacks to a loose folder nothing looks in
//! (and, on macOS, quarantined). [`place`] copies such a folder, once verified as VMware's, into a
//! folder the caller keeps (its own tools folder, which it puts on its `PATH`).
//!
//! [`locate`] finds the one to report, in this order: a hypervisor's own copy
//! ([`crate::host::Devices::ovftool`]), then the caller's folder, then the first on the `PATH` it
//! passes. [`probe`] asks the found file its version, and says whether it can't run until Rosetta
//! is installed.
//!
//! Every program (the tool itself, `codesign`, `xattr`) runs through the context's runner; the
//! signature check and the quarantine are macOS's, chosen by the context's host, never by the OS
//! the crate is built for.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use super::{Tool, first_line};
use crate::context::Context;
use crate::failure::{self, Failure};
use crate::host::{Arch, HostOs};

#[cfg(test)]
mod tests;

/// The team identifier of VMware's Apple Developer ID (Broadcom still signs with it).
pub const VMWARE_TEAM: &str = "EG7KH642X6";

/// macOS's `codesign`, by its path: the system's own, never one on `PATH`.
const CODESIGN: &str = "/usr/bin/codesign";

/// macOS's `xattr`, by its path.
const XATTR: &str = "/usr/bin/xattr";

/// The most a picked folder may hold: the tool is about 60 files and 60 MB, so anything far
/// bigger is the wrong folder (a whole Downloads folder holding it), not a copy to make.
const MAX_FILES: usize = 2_000;
const MAX_BYTES: u64 = 1 << 30;

/// A placement left behind by a crash (a copy being made, or one being replaced) is removed once
/// it is this old: younger, it may be another placement still running.
const LEFTOVER_AFTER: Duration = Duration::from_secs(600);

/// What the names of a placement's own folders start with, beside the placed one.
const WORK_PREFIX: &str = ".ovftool-";

/// The tool's file name on `os`.
#[must_use]
pub fn exe(os: HostOs) -> &'static str {
    match os {
        HostOs::Windows => "ovftool.exe",
        HostOs::Macos | HostOs::Linux => "ovftool",
    }
}

/// The OVF Tool folder inside VMware Workstation, installed under the Program Files folder `pf`.
#[must_use]
pub fn workstation_dir(pf: &Path) -> PathBuf {
    pf.join("VMware").join("VMware Workstation").join("OVFTool")
}

/// Where the OVF Tool found comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS), ts(rename = "OvftoolSource"))]
#[serde(rename_all = "camelCase")]
pub enum Source {
    /// Inside VMware Fusion (macOS).
    Fusion,
    /// Inside VMware Workstation (Windows).
    Workstation,
    /// The copy placed in the caller's folder ([`place`]).
    Managed,
    /// The first on `PATH`.
    Path,
}

/// The OVF Tool found: where it comes from and its file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS), ts(rename = "OvftoolLocation"))]
#[serde(rename_all = "camelCase")]
pub struct Location {
    /// Where it comes from.
    pub source: Source,
    /// The program's path.
    pub path: String,
    /// It is built for Intel Macs only, this Mac has Apple silicon and Rosetta isn't installed:
    /// it can't run until Rosetta is (`softwareupdate --install-rosetta`).
    pub needs_rosetta: bool,
}

/// Why a picked folder wasn't placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS), ts(rename = "OvftoolRefusal"))]
#[serde(rename_all = "camelCase")]
pub enum Refusal {
    /// No `ovftool` program in the folder (nor in the one folder inside it).
    NoTool,
    /// The program is there, but not its libraries (`lib/` beside it): a partial copy.
    NoLibraries,
    /// The folder holds far more than the tool: the wrong folder.
    TooLarge,
    /// macOS: a program or library in it isn't validly signed.
    Unsigned,
    /// macOS: it is signed, but not by VMware.
    NotVmware,
}

/// What [`place`] did with the picked folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS), ts(rename = "OvftoolPlaced"))]
#[serde(tag = "outcome", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Outcome {
    /// Copied into the caller's folder, where the next [`locate`] finds it.
    Placed {
        /// The placed program's path.
        path: String,
        /// Whether it can't run until Rosetta is installed (see [`Location::needs_rosetta`]).
        needs_rosetta: bool,
    },
    /// Not placed, for `reason`.
    Refused {
        /// Why it wasn't placed.
        reason: Refusal,
        /// What macOS said of the signature, or the team that signed it instead of VMware, when
        /// known.
        detail: Option<String>,
    },
}

impl Outcome {
    fn refused(reason: Refusal, detail: Option<String>) -> Outcome {
        Outcome::Refused { reason, detail }
    }
}

/// Why [`place`] couldn't finish: not a folder that isn't VMware's OVF Tool (that is an
/// [`Outcome::Refused`]), but a copy that couldn't be made or swapped in, or a check that couldn't
/// run.
#[derive(Debug)]
#[non_exhaustive]
pub enum PlaceError {
    /// Copying, or swapping the copy in, failed (a full disk, a permission).
    Io(std::io::Error),
    /// A signature check or the quarantine's removal couldn't be run, or `xattr` failed.
    Tool(Failure),
    /// A path isn't UTF-8, so it can't be handed to a tool as an argument.
    NotUtf8(PathBuf),
}

impl std::fmt::Display for PlaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlaceError::Io(e) => write!(f, "couldn't place the OVF Tool: {e}"),
            PlaceError::Tool(e) => write!(f, "couldn't check the OVF Tool: {e}"),
            PlaceError::NotUtf8(p) => write!(f, "the path {} isn't UTF-8", p.display()),
        }
    }
}

impl std::error::Error for PlaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PlaceError::Io(e) => Some(e),
            PlaceError::Tool(e) => Some(e),
            PlaceError::NotUtf8(_) => None,
        }
    }
}

impl From<std::io::Error> for PlaceError {
    fn from(e: std::io::Error) -> PlaceError {
        PlaceError::Io(e)
    }
}

/// The hypervisor a copy in [`crate::host::Devices::ovftool`] comes with on `os`.
fn bundled_source(os: HostOs) -> Source {
    match os {
        HostOs::Windows => Source::Workstation,
        HostOs::Macos | HostOs::Linux => Source::Fusion,
    }
}

/// The OVF Tool to report, in order: a hypervisor's own copy, the one in `managed` (the caller's
/// folder [`place`] copies into), then the first on `path` (the caller's `PATH`). None when there
/// is none.
#[must_use]
pub fn locate(cx: &Context, managed: Option<&Path>, path: Option<&OsStr>) -> Option<(Source, PathBuf)> {
    let exe = exe(cx.host.os);
    let bundled = cx.host.devices.ovftool.iter().map(|f| (bundled_source(cx.host.os), f.clone()));
    let managed = managed.map(|d| (Source::Managed, d.join(exe)));
    let found = bundled.chain(managed).find(|(_, f)| f.is_file());
    found.or_else(|| {
        let on_path = std::env::split_paths(path?).map(|d| d.join(exe)).find(|f| f.is_file())?;
        Some((Source::Path, on_path))
    })
}

/// What a machine check reports of the OVF Tool: its version (when `--version` runs) and where
/// it was found ([`locate`]).
pub async fn probe(cx: &Context, managed: Option<&Path>, path: Option<&OsStr>) -> (Tool, Option<Location>) {
    let Some((source, file)) = locate(cx, managed, path) else {
        return (Tool::MISSING, None);
    };
    let tool = version_of(cx, &file).await;
    let location = Location {
        source,
        path: file.display().to_string(),
        needs_rosetta: needs_rosetta(cx, &file),
    };
    (tool, Some(location))
}

/// The OVF Tool's version, or why it can't be used, from what [`probe`] found: the tool's
/// answer, and where it is.
///
/// # Errors
///
/// Why it can't be used: built for Intel Macs and Rosetta missing, found but not running, or not
/// found.
pub fn usable(tool: Tool, location: Option<&Location>) -> Result<String, String> {
    match (tool, location) {
        (Tool { installed: true, version }, _) => Ok(version.unwrap_or_else(|| "ovftool".into())),
        (_, Some(at)) if at.needs_rosetta => Err(format!("VMware's OVF Tool at {} is built for Intel Macs and needs Rosetta", at.path)),
        (_, Some(at)) => Err(format!("VMware's OVF Tool at {} doesn't run", at.path)),
        (_, None) => Err("VMware's OVF Tool (`ovftool`) wasn't found (it comes with VMware Fusion or Workstation)".into()),
    }
}

/// `ovftool --version` of the program `file`, run by its path so it is that file that answers:
/// installed when it exits 0, its version the first line it printed.
pub async fn version_of(cx: &Context, file: &Path) -> Tool {
    match cx.read_ok(&file.to_string_lossy(), &["--version"]).await {
        Ok(out) => Tool::installed(first_line(&out)),
        Err(_) => Tool::MISSING,
    }
}

/// Places the OVF Tool unpacked at `picked` (the folder holding `ovftool`, or the one folder
/// inside it that does) at `dest`, the caller's folder for it, replacing an earlier copy: it is
/// copied first (beside `dest`, so swapping it in is a rename), the copy checked (on macOS,
/// signed by VMware, its quarantine then lifted), and only then swapped in. The picked folder is
/// only read.
///
/// # Errors
///
/// [`PlaceError`] when the copy can't be made or swapped in, or a check can't run. A folder that
/// isn't VMware's OVF Tool is an [`Outcome::Refused`], not an error.
pub async fn place(cx: &Context, picked: &Path, dest: &Path) -> Result<Outcome, PlaceError> {
    let os = cx.host.os;
    let Some(root) = tool_root(picked, exe(os)) else {
        return Ok(Outcome::refused(Refusal::NoTool, None));
    };
    if !has_libraries(&root, os) {
        return Ok(Outcome::refused(Refusal::NoLibraries, None));
    }
    let parent = dest.parent().map(Path::to_path_buf).unwrap_or_default();
    std::fs::create_dir_all(&parent)?;
    remove_leftovers(&parent);
    let work = tempfile::Builder::new().prefix(&format!("{WORK_PREFIX}new-")).tempdir_in(&parent)?;
    let staged = work.path().join("ovftool");
    let copied = {
        let (root, staged) = (root.clone(), staged.clone());
        tokio::task::spawn_blocking(move || copy_tree(&root, &staged, &mut Budget::default()))
            .await
            .map_err(|e| std::io::Error::other(format!("copying the OVF Tool stopped: {e}")))?
    };
    // Whatever happens, `work` is removed when dropped: once swapped in, the copy isn't in it.
    match copied {
        Ok(()) => finish(cx, &staged, dest).await,
        Err(e) if e.kind() == std::io::ErrorKind::FileTooLarge => Ok(Outcome::refused(Refusal::TooLarge, None)),
        Err(e) => Err(e.into()),
    }
}

/// Checks the copy `staged` and swaps it in at `dest`: the checks run on the copy, so what was
/// checked is exactly what is placed.
async fn finish(cx: &Context, staged: &Path, dest: &Path) -> Result<Outcome, PlaceError> {
    let exe = exe(cx.host.os);
    if !is_program(&staged.join(exe)) {
        return Ok(Outcome::refused(Refusal::NoTool, None));
    }
    // macOS only: Windows and Linux carry no such signature check, nor a quarantine.
    if cx.host.os == HostOs::Macos {
        if let Some((reason, detail)) = signature_refusal(cx, staged).await? {
            return Ok(Outcome::refused(reason, detail));
        }
        lift_quarantine(cx, staged).await?;
    }
    swap_in(staged, dest)?;
    let placed = dest.join(exe);
    Ok(Outcome::Placed {
        path: placed.display().to_string(),
        needs_rosetta: needs_rosetta(cx, &placed),
    })
}

/// The folder holding the tool `exe`: `picked` itself, or the one folder inside it that holds it
/// (the Windows and Linux downloads unpack into an `ovftool` folder).
fn tool_root(picked: &Path, exe: &str) -> Option<PathBuf> {
    if picked.join(exe).is_file() {
        return Some(picked.to_path_buf());
    }
    let inside: Vec<PathBuf> = std::fs::read_dir(picked)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join(exe).is_file())
        .collect();
    match inside.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// Whether the tool at `root` has its libraries: a `lib/` folder beside it (macOS, and the newer
/// Linux downloads), or, on Windows and Linux, the libraries themselves beside it.
fn has_libraries(root: &Path, os: HostOs) -> bool {
    if root.join("lib").is_dir() {
        return true;
    }
    let library = |name: &str| match os {
        HostOs::Windows => name.to_ascii_lowercase().ends_with(".dll"),
        HostOs::Linux => name.contains(".so"),
        HostOs::Macos => false,
    };
    std::fs::read_dir(root).is_ok_and(|entries| entries.flatten().any(|e| library(&e.file_name().to_string_lossy())))
}

/// Whether `file` is a program this OS will start: a file, executable on Unix.
fn is_program(file: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(file) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

/// What a copy may still take, so a wrong folder (a whole Downloads) isn't copied whole.
struct Budget {
    files: usize,
    bytes: u64,
}

impl Default for Budget {
    fn default() -> Budget {
        Budget {
            files: MAX_FILES,
            bytes: MAX_BYTES,
        }
    }
}

impl Budget {
    /// Takes one entry of `len` bytes; `FileTooLarge` once the budget is spent.
    fn take(&mut self, len: u64) -> std::io::Result<()> {
        if self.files == 0 || len > self.bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "the folder holds far more than the OVF Tool",
            ));
        }
        self.files -= 1;
        self.bytes -= len;
        Ok(())
    }
}

/// Copies the folder `from` to `to` (a new folder), its files with their permissions. A symbolic
/// link is copied as one when it points inside its own folder (the versioned names of a Linux
/// library); any other is left out.
fn copy_tree(from: &Path, to: &Path, budget: &mut Budget) -> std::io::Result<()> {
    std::fs::create_dir(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_dir() {
            budget.take(0)?;
            copy_tree(&src, &dst, budget)?;
        } else if kind.is_file() {
            budget.take(entry.metadata()?.len())?;
            std::fs::copy(&src, &dst)?;
        } else if kind.is_symlink() {
            budget.take(0)?;
            copy_link(&src, &dst)?;
        }
    }
    Ok(())
}

/// Copies the symbolic link `src` as `dst` when its target stays beside it (no `..`, not
/// absolute); otherwise leaves it out (and every link, on Windows).
fn copy_link(src: &Path, dst: &Path) -> std::io::Result<()> {
    let target = std::fs::read_link(src)?;
    let stays_inside = target.components().all(|c| matches!(c, Component::Normal(_)));
    if !stays_inside {
        return Ok(());
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, dst)
    }
    #[cfg(not(unix))]
    {
        let _ = dst;
        Ok(())
    }
}

/// Swaps the folder `staged` in at `dest`: the earlier copy is moved aside, the new one renamed in
/// its place (one rename each, on one file system), then the earlier one removed. If the new one
/// can't go in, the earlier one is put back.
fn swap_in(staged: &Path, dest: &Path) -> std::io::Result<()> {
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    let aside_dir = tempfile::Builder::new().prefix(&format!("{WORK_PREFIX}old-")).tempdir_in(parent)?;
    let aside = aside_dir.path().join("ovftool");
    let had = match std::fs::rename(dest, &aside) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e),
    };
    if let Err(e) = std::fs::rename(staged, dest) {
        if had {
            std::fs::rename(&aside, dest).ok();
        }
        return Err(e);
    }
    // The earlier copy goes with `aside_dir`; one still running (Windows keeps its files locked)
    // is removed with the next leftovers.
    drop(aside_dir);
    Ok(())
}

/// Removes, from `parent`, the folders an interrupted placement left behind (a copy being made,
/// or one being replaced), once they are old enough not to be another placement's still running.
fn remove_leftovers(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|at| now.duration_since(at).unwrap_or_default() >= LEFTOVER_AFTER);
        if entry.file_name().to_string_lossy().starts_with(WORK_PREFIX) && old {
            std::fs::remove_dir_all(entry.path()).ok();
        }
    }
}

/// `path` as the text a tool's argument is.
fn text(path: &Path) -> Result<&str, PlaceError> {
    path.to_str().ok_or_else(|| PlaceError::NotUtf8(path.to_path_buf()))
}

/// macOS: why the copy `staged` isn't placed, when one of its programs or libraries isn't validly
/// signed by VMware's team. Every Mach-O file in it is checked, so the quarantine lifted next is
/// lifted from VMware's code only.
///
/// `codesign --verify --strict` checks the signature itself; with `-R`, that it chains to Apple
/// and names VMware's team in its leaf certificate, which only Apple can issue. That is what
/// decides; the team a refusal names comes from the signature's designated requirement
/// (`-d -r-`).
async fn signature_refusal(cx: &Context, staged: &Path) -> Result<Option<(Refusal, Option<String>)>, PlaceError> {
    let requirement = format!("-R=anchor apple generic and certificate leaf[subject.OU] = \"{VMWARE_TEAM}\"");
    for file in mach_o_files(staged) {
        let shown = file.strip_prefix(staged).unwrap_or(&file).display().to_string();
        let path = text(&file)?;
        match failure::ask(cx, (CODESIGN, &["--verify", "--strict", path])).await {
            Ok(_) => {}
            Err(Failure::Refused { said, .. }) => return Ok(Some((Refusal::Unsigned, Some(format!("{shown}: {said}"))))),
            Err(e) => return Err(PlaceError::Tool(e)),
        }
        match failure::ask(cx, (CODESIGN, &["--verify", "--strict", &requirement, path])).await {
            Ok(_) => {}
            Err(Failure::Refused { .. }) => {
                let team = failure::ask(cx, (CODESIGN, &["-d", "-r-", path])).await.ok().and_then(|dr| signing_team(&dr));
                return Ok(Some((Refusal::NotVmware, team)));
            }
            Err(e) => return Err(PlaceError::Tool(e)),
        }
    }
    Ok(None)
}

/// The team a designated requirement (`codesign -d -r-`) names: its leaf certificate's
/// organizational unit.
fn signing_team(requirement: &str) -> Option<String> {
    let (_, after) = requirement.split_once("leaf[subject.OU] = ")?;
    let team: String = after.trim_start_matches('"').chars().take_while(char::is_ascii_alphanumeric).collect();
    (!team.is_empty()).then_some(team)
}

/// Every Mach-O file (a program or library) under `dir`, by its first bytes.
fn mach_o_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            found.extend(mach_o_files(&entry.path()));
        } else if kind.is_file() && head(&entry.path()).is_some_and(|h| is_mach_o(&h)) {
            found.push(entry.path());
        }
    }
    found.sort();
    found
}

/// macOS: lifts the quarantine from the copy `staged`, so Gatekeeper lets it run.
///
/// Only once [`signature_refusal`] passed on that very copy, and only on the copy: every program
/// and library in it is verified as signed by VMware's Developer ID team. The tool isn't
/// notarized (no hardened runtime), so with the quarantine on, Gatekeeper refuses to start it;
/// with it lifted, it runs as VMware signed it. The picked download keeps its quarantine.
async fn lift_quarantine(cx: &Context, staged: &Path) -> Result<(), PlaceError> {
    failure::ask(cx, (XATTR, &["-d", "-r", "com.apple.quarantine", text(staged)?]))
        .await
        .map(drop)
        .map_err(PlaceError::Tool)
}

/// The CPU kinds a Mach-O file is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cpu {
    X86_64,
    Arm64,
    Other,
}

/// The first bytes of `file`, enough for a Mach-O header and a universal binary's list.
fn head(file: &Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let mut buf = Vec::with_capacity(512);
    std::fs::File::open(file).ok()?.take(512).read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn word(bytes: &[u8], at: usize, big_endian: bool) -> Option<u32> {
    let b: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
    Some(if big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
}

const MH_MAGIC_64: u32 = 0xfeed_facf;
const MH_MAGIC: u32 = 0xfeed_face;
const FAT_MAGIC: u32 = 0xcafe_babe;
const FAT_MAGIC_64: u32 = 0xcafe_babf;

fn is_mach_o(head: &[u8]) -> bool {
    word(head, 0, false).is_some_and(|m| m == MH_MAGIC_64 || m == MH_MAGIC) || word(head, 0, true).is_some_and(|m| m == FAT_MAGIC || m == FAT_MAGIC_64)
}

fn cpu(kind: u32) -> Cpu {
    match kind {
        0x0100_0007 => Cpu::X86_64,
        0x0100_000c => Cpu::Arm64,
        _ => Cpu::Other,
    }
}

/// The CPUs the Mach-O file whose first bytes are `head` runs on: one for a thin file, each of a
/// universal binary's. Empty for anything else.
fn cpus(head: &[u8]) -> Vec<Cpu> {
    match (word(head, 0, false), word(head, 0, true)) {
        (Some(MH_MAGIC_64 | MH_MAGIC), _) => word(head, 4, false).map(cpu).into_iter().collect(),
        (_, Some(magic @ (FAT_MAGIC | FAT_MAGIC_64))) => {
            let entry = if magic == FAT_MAGIC { 20 } else { 32 };
            let count = word(head, 4, true).map_or(0, |c| usize::try_from(c).unwrap_or(0));
            (0..count.min(16)).filter_map(|i| word(head, 8 + i * entry, true)).map(cpu).collect()
        }
        _ => Vec::new(),
    }
}

/// Whether a program built for `cpus` runs on an Apple silicon Mac only through Rosetta.
fn intel_only(cpus: &[Cpu]) -> bool {
    !cpus.is_empty() && cpus.iter().all(|c| *c == Cpu::X86_64)
}

/// Whether the program `file` can't run on the context's host until Rosetta is installed: it is
/// built for Intel Macs only (the standalone OVF Tool is), the host is a Mac with Apple silicon,
/// and Rosetta's runtime isn't there.
fn needs_rosetta(cx: &Context, file: &Path) -> bool {
    cx.host.os == HostOs::Macos && cx.host.arch == Arch::Arm64 && !cx.host.devices.rosetta_runtime.exists() && head(file).is_some_and(|h| intel_only(&cpus(&h)))
}
