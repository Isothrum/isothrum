//! A project: the folder a run starts from, in one of the formats Isothrum drives. The format is
//! recognised from the folder's own files ([`Format::detect`]), never from a file another tool
//! writes for the runner: any Compose project, Vagrantfile or Terraform module runs, whoever
//! wrote it.
//!
//! - [`Format`]: which tool runs the folder, and the files that say so.
//! - [`Project`]: the folder, its format and those files, plus the digest that tells an update
//!   the files changed.
//! - [`platform`]: the host and hypervisor words the drivers and the embedder share.
//! - [`inputs`]: the values a run is given, written the way each tool reads them.

pub(crate) mod inputs;
pub(crate) mod platform;
pub(crate) mod variables;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::error::Error;

/// The format of a project, which is the tool that runs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Format {
    /// A Compose project (`docker compose`): a `compose.yaml` or one of the names Compose reads.
    Compose,
    /// A Vagrant project (`vagrant`): a `Vagrantfile`.
    Vagrant,
    /// A Terraform root module (`terraform`): one or more `*.tf` or `*.tf.json` files.
    Terraform,
}

/// The file names Compose reads in a folder by default, in its own order of precedence.
const COMPOSE_FILES: [&str; 4] = ["compose.yaml", "compose.yml", "docker-compose.yaml", "docker-compose.yml"];

/// The file Vagrant reads in a folder.
const VAGRANTFILE: &str = "Vagrantfile";

/// The files of a Terraform root module that pin its providers, part of what it runs.
const TERRAFORM_LOCK: &str = ".terraform.lock.hcl";

impl Format {
    /// Every format, in the order a folder is tried.
    pub const ALL: [Format; 3] = [Format::Compose, Format::Vagrant, Format::Terraform];

    /// The format as written on the command line and in events (`compose`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Format::Compose => "compose",
            Format::Vagrant => "vagrant",
            Format::Terraform => "terraform",
        }
    }

    /// The format named `name` ([`Format::as_str`]), if any.
    #[must_use]
    pub fn named(name: &str) -> Option<Format> {
        Format::ALL.into_iter().find(|f| f.as_str() == name)
    }

    /// The files of `dir` that make it a project of this format, sorted; none when it isn't one.
    /// Compose: the first default file Compose itself would read (one only, as Compose takes).
    /// Vagrant: the `Vagrantfile`. Terraform: every `*.tf` and `*.tf.json` file, and the provider
    /// lock when there is one.
    ///
    /// # Errors
    ///
    /// `io` when `dir` can't be listed.
    pub fn files_in(self, dir: &Path) -> Result<Vec<PathBuf>> {
        let found = match self {
            Format::Compose => COMPOSE_FILES.iter().map(|n| dir.join(n)).find(|p| p.is_file()).into_iter().collect(),
            Format::Vagrant => Some(dir.join(VAGRANTFILE)).filter(|p| p.is_file()).into_iter().collect(),
            Format::Terraform => terraform_files(dir)?,
        };
        Ok(found)
    }

    /// The format of `dir`: the one whose files it holds ([`Format::files_in`]).
    ///
    /// # Errors
    ///
    /// `project_invalid` when `dir` holds no format's files, or the files of several (a folder
    /// with both a Compose file and a Vagrantfile: the caller names the one to run); `io` when
    /// `dir` can't be listed.
    pub fn detect(dir: &Path) -> Result<Format> {
        let mut found = Vec::new();
        for format in Format::ALL {
            if !format.files_in(dir)?.is_empty() {
                found.push(format);
            }
        }
        match found.as_slice() {
            [one] => Ok(*one),
            [] => Err(Error::Project {
                dir: dir.to_path_buf(),
                reason: "it holds no Compose file (compose.yaml), Vagrantfile or Terraform module (*.tf)".to_owned(),
            }),
            many => Err(Error::Project {
                dir: dir.to_path_buf(),
                reason: format!(
                    "it holds the files of several formats ({}): name the one to run",
                    many.iter().map(|f| f.as_str()).collect::<Vec<_>>().join(", ")
                ),
            }),
        }
    }
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Every Terraform file at the top of `dir` (a root module's own; its sub-modules are what it
/// calls), sorted, with the provider lock.
fn terraform_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(dir, "list")(e)),
    };
    let mut found = Vec::new();
    for entry in entries {
        let path = entry.map_err(Error::io(dir, "list"))?.path();
        if is_terraform_file(&path) && path.is_file() {
            found.push(path);
        }
    }
    if !found.is_empty() {
        let lock = dir.join(TERRAFORM_LOCK);
        if lock.is_file() {
            found.push(lock);
        }
    }
    found.sort();
    Ok(found)
}

/// Whether `path` names a Terraform file: `*.tf` or `*.tf.json`, in lower case as Terraform reads
/// them.
fn is_terraform_file(path: &Path) -> bool {
    let tf = |p: &Path| p.extension().is_some_and(|e| e == "tf");
    tf(path) || (path.extension().is_some_and(|e| e == "json") && tf(&path.with_extension("")))
}

/// A project to run: its folder, its format and the files that make it one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Project {
    /// The folder, absolute: where the tool runs.
    pub dir: PathBuf,
    /// Its format.
    pub format: Format,
    /// The files that make it a project of that format ([`Format::files_in`]), absolute.
    pub files: Vec<PathBuf>,
}

impl Project {
    /// The project in `dir`, of format `format` when given, else the one detected
    /// ([`Format::detect`]).
    ///
    /// # Errors
    ///
    /// `project_invalid` when `dir` isn't a folder, when `format` is given and `dir` holds none
    /// of its files, or as [`Format::detect`] refuses; `io` when `dir` can't be read.
    pub fn open(dir: &Path, format: Option<Format>) -> Result<Project> {
        let dir = std::path::absolute(dir).map_err(Error::io(dir, "read"))?;
        if !dir.is_dir() {
            return Err(Error::Project {
                dir,
                reason: "it isn't a folder".to_owned(),
            });
        }
        let format = match format {
            Some(f) => f,
            None => Format::detect(&dir)?,
        };
        let files = format.files_in(&dir)?;
        if files.is_empty() {
            return Err(Error::Project {
                dir,
                reason: format!("it holds no {format} project"),
            });
        }
        Ok(Project { dir, format, files })
    }

    /// What the project's files hold, as a short digest: it changes when one of them changes, is
    /// added or goes (an update reads it to know the project changed). What those files only
    /// point at (a build context, a provisioner's script, a sub-module) isn't read: the tool reads
    /// it again at every operation anyway. Not a security check.
    ///
    /// # Errors
    ///
    /// `io` when one of the files can't be read.
    pub fn digest(&self) -> Result<String> {
        let mut hash = Fnv64::new();
        hash.write(self.format.as_str().as_bytes());
        for file in &self.files {
            let name = file.strip_prefix(&self.dir).unwrap_or(file);
            hash.write(&[0]);
            hash.write(name.to_string_lossy().as_bytes());
            hash.write(&[0]);
            hash.write(&std::fs::read(file).map_err(Error::io(file, "read"))?);
        }
        Ok(format!("fnv64:{:016x}", hash.0))
    }
}

/// FNV-1a over 64 bits: enough to tell that files changed, and nothing more.
struct Fnv64(u64);

impl Fnv64 {
    fn new() -> Fnv64 {
        Fnv64(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

#[cfg(test)]
mod tests;
