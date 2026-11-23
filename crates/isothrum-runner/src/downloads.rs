//! What the runs' own downloads fetched, and their removal: the images a Compose start's pull
//! pulled and the boxes a Vagrant `up` added, recorded in the home's store as they are fetched
//! ([`Runner::downloads`], [`Runner::remove_downloads`]). An image counts as fetched when, after
//! the pull, its reference stands for an id the engine didn't hold before it; a box when `box
//! list` lists it after the `up` and didn't before. What was there before, or what anything else
//! fetched, is never recorded, so never removed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::cli::{Call, docker, vagrant};
use crate::name::RunName;
use crate::process::{DockerContext, ProcessRunner};
use crate::runner::Runner;
use crate::tool_output::vagrant::InstalledBox;

mod adopt;

pub use adopt::DownloadToAdopt;

/// How long one call of a listing or a removal may take.
const CALL_LIMIT: Duration = Duration::from_secs(300);

/// A recorded download's id in the home's store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct DownloadId(pub i64);

/// What a run's tool fetched.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DownloadItem {
    /// A container image a Compose start's pull pulled.
    Image {
        /// The reference it was pulled as (`nginx:1.27`), as the project names it.
        reference: String,
        /// The id the reference stood for once pulled (`sha256:…`).
        image_id: String,
        /// The Docker context it was pulled through, when the run names one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        docker_context: Option<String>,
    },
    /// A Vagrant box an `up` added.
    Box {
        /// Its name (`bento/debian-12`).
        name: String,
        /// The provider it is for (`virtualbox`).
        provider: String,
        /// Its version.
        version: String,
        /// Its architecture (`amd64`), when it has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        architecture: Option<String>,
    },
}

impl DownloadItem {
    /// The box `held` names, as fetched.
    pub(crate) fn of_box(held: InstalledBox) -> DownloadItem {
        DownloadItem::Box {
            name: held.name,
            provider: held.provider,
            version: held.version,
            architecture: held.architecture,
        }
    }
}

/// One recorded download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Download {
    /// Its id: what a removal names it by.
    pub id: DownloadId,
    /// What was fetched.
    #[serde(flatten)]
    pub item: DownloadItem,
    /// The run whose operation fetched it; none for one adopted ([`Runner::adopt_downloads`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunName>,
    /// When (RFC 3339, UTC).
    pub fetched_at: String,
    /// Its size on disk in bytes, when it could be read (an image's as its engine says, a box's
    /// folder's); none in what the store keeps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

/// Which recorded downloads a removal takes: all, or those of the given ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadsToRemove {
    /// Every recorded download.
    All,
    /// Only these.
    Only(Vec<DownloadId>),
}

/// How the removal of one recorded download went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RemovalOutcome {
    /// Removed, and its record forgotten.
    Removed,
    /// Gone already (removed outside the runner): its record forgotten.
    Gone,
    /// The image's reference now stands for another image (pulled again, retagged): nothing
    /// removed, the record forgotten, the reference no longer the runner's.
    Moved {
        /// The id the reference stands for now.
        now: String,
    },
    /// Its tool refused (an image a container uses, a box a machine uses) or failed: kept, and
    /// its record too.
    Failed {
        /// What the tool said.
        reason: String,
    },
    /// No download of that id is recorded.
    NotRecorded,
}

/// The removal of one recorded download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DownloadRemoval {
    /// The download asked for.
    pub id: DownloadId,
    /// How it went.
    #[serde(flatten)]
    pub outcome: RemovalOutcome,
    /// What it was, when it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<Download>,
}

impl Runner {
    /// Every download the home's runs fetched and the store records, oldest first, each with its
    /// size when it can be read.
    ///
    /// # Errors
    ///
    /// The store can't be read.
    pub async fn downloads(&self) -> Result<Vec<Download>> {
        let mut listed = self.store.db().read(recorded)?;
        for download in &mut listed {
            download.size_bytes = self.size_of(&download.item).await;
        }
        Ok(listed)
    }

    /// Removes the recorded downloads `which` names, each on its own: an image by its reference
    /// (`docker image rm`, never forced) while the reference still stands for the recorded id, a
    /// box by its name, provider, version and architecture (`vagrant box remove`, never forced).
    /// One its tool refuses (in use) or fails to remove is said in its outcome and kept with its
    /// record; the others go on. The record of one removed, gone already, or whose reference now
    /// stands for another image is forgotten. Nothing the store doesn't record is removed.
    ///
    /// # Errors
    ///
    /// The store can't be read or written.
    pub async fn remove_downloads(&self, which: &DownloadsToRemove) -> Result<Vec<DownloadRemoval>> {
        let recorded = self.store.db().read(recorded)?;
        let asked: Vec<(DownloadId, Option<Download>)> = match which {
            DownloadsToRemove::All => recorded.into_iter().map(|d| (d.id, Some(d))).collect(),
            DownloadsToRemove::Only(ids) => ids.iter().map(|id| (*id, recorded.iter().find(|d| d.id == *id).cloned())).collect(),
        };
        let mut removals = Vec::with_capacity(asked.len());
        for (id, download) in asked {
            let outcome = match &download {
                Some(d) => self.remove(&d.item).await,
                None => RemovalOutcome::NotRecorded,
            };
            if matches!(outcome, RemovalOutcome::Removed | RemovalOutcome::Gone | RemovalOutcome::Moved { .. }) {
                self.store.db().write(|tx| tx.forget_download(id))?;
            }
            removals.push(DownloadRemoval { id, outcome, download });
        }
        Ok(removals)
    }

    /// Removes `item`, as [`Runner::remove_downloads`] says.
    async fn remove(&self, item: &DownloadItem) -> RemovalOutcome {
        let failed = |e: &dyn std::fmt::Display| RemovalOutcome::Failed { reason: e.to_string() };
        match item {
            DownloadItem::Image {
                reference,
                image_id,
                docker_context,
            } => {
                let processes = self.docker(docker_context.as_deref());
                let call = self.call(&processes);
                match docker::image_id(&call, reference).await {
                    Ok(None) => RemovalOutcome::Gone,
                    Ok(Some(now)) if now != *image_id => RemovalOutcome::Moved { now },
                    Ok(Some(_)) => match docker::remove_image(&call, reference).await {
                        Ok(()) => RemovalOutcome::Removed,
                        Err(e) => failed(&e),
                    },
                    Err(e) => failed(&e),
                }
            }
            DownloadItem::Box {
                name,
                provider,
                version,
                architecture,
            } => {
                let held = InstalledBox {
                    name: name.clone(),
                    provider: provider.clone(),
                    version: version.clone(),
                    architecture: architecture.clone(),
                };
                let call = self.call(self.config.processes.as_ref());
                match vagrant::boxes(&call).await {
                    Ok(boxes) if !boxes.contains(&held) => RemovalOutcome::Gone,
                    Ok(_) => match vagrant::remove_box(&call, &held).await {
                        Ok(()) => RemovalOutcome::Removed,
                        Err(e) => failed(&e),
                    },
                    Err(e) => failed(&e),
                }
            }
        }
    }

    /// `item`'s size on disk, when it can be read.
    async fn size_of(&self, item: &DownloadItem) -> Option<u64> {
        match item {
            DownloadItem::Image { image_id, docker_context, .. } => {
                let processes = self.docker(docker_context.as_deref());
                docker::image_size(&self.call(&processes), image_id).await.ok().flatten()
            }
            DownloadItem::Box {
                name,
                provider,
                version,
                architecture,
            } => {
                let home = vagrant_home(&self.config.host_env)?;
                let mut dir = home.join("boxes").join(name.replace('/', "-VAGRANTSLASH-")).join(version);
                if let Some(arch) = architecture {
                    dir.push(arch);
                }
                folder_size(&dir.join(provider))
            }
        }
    }

    /// The runner's processes, their `docker` calls sent to `context` when there is one.
    pub(super) fn docker<'a>(&'a self, context: Option<&'a str>) -> DockerContext<'a> {
        DockerContext {
            inner: self.config.processes.as_ref(),
            context,
        }
    }

    /// A call of a tool through `processes`, in the home (no project's folder: Vagrant reads the
    /// Vagrantfile of the folder it runs in), saying nothing.
    pub(super) fn call<'a>(&'a self, processes: &'a dyn ProcessRunner) -> Call<'a> {
        Call {
            runner: processes,
            cwd: Some(self.config.home.clone()),
            env: Vec::new(),
            limit: CALL_LIMIT,
            sink: &crate::formats::discard,
            host_env: &self.config.host_env,
        }
    }
}

/// Every recorded download ([`crate::store::Tx::downloads`]).
fn recorded(tx: &crate::store::Tx<'_>) -> Result<Vec<Download>> {
    tx.downloads()
}

/// Vagrant's own folder: `VAGRANT_HOME`, else `.vagrant.d` in the user's home (`HOME`, on Windows
/// `USERPROFILE`).
fn vagrant_home(env: &crate::host_env::HostEnv) -> Option<PathBuf> {
    if let Some(home) = env.get("VAGRANT_HOME").filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(home));
    }
    ["HOME", "USERPROFILE"]
        .into_iter()
        .find_map(|name| env.get(name).filter(|h| !h.is_empty()))
        .map(|home| Path::new(home).join(".vagrant.d"))
}

/// The size of every file under `dir`, links not followed; none when `dir` can't be read.
fn folder_size(dir: &Path) -> Option<u64> {
    let mut total = 0_u64;
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let kind = entry.file_type().ok()?;
        total = total.saturating_add(if kind.is_dir() {
            folder_size(&entry.path())?
        } else if kind.is_file() {
            entry.metadata().ok()?.len()
        } else {
            0
        });
    }
    Some(total)
}

#[cfg(test)]
mod tests;
