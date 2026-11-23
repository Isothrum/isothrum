//! Downloads fetched outside the runs (by an earlier program that kept its own record of what it
//! fetched), adopted into the home's record so they are listed and removed like the runs' own
//! ([`Runner::adopt_downloads`]). Only what is on this computer now, as it was fetched, is
//! adopted: an image whose reference stands for another id now, or a box no longer held, is
//! someone else's, or gone.

use serde::{Deserialize, Serialize};

use super::{Download, DownloadItem};
use crate::Result;
use crate::cli::{CliError, docker, vagrant};
use crate::runner::Runner;
use crate::tool_output::vagrant::InstalledBox;

/// A download to adopt, as the program that fetched it recorded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DownloadToAdopt {
    /// A container image, on this computer's engine (its current Docker context).
    Image {
        /// The reference it was pulled as (`nginx:1.27`).
        reference: String,
        /// The id the reference stood for once pulled, when recorded: one standing for another
        /// id now isn't adopted. None: whatever the reference stands for now.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image_id: Option<String>,
    },
    /// A Vagrant box version for a provider, of whichever architectures are held.
    Box {
        /// Its name (`bento/debian-12`).
        name: String,
        /// The provider it is for (`virtualbox`).
        provider: String,
        /// Its version.
        version: String,
    },
}

impl Runner {
    /// Records `adopted` as downloads with no run, each one on this computer now: an image while
    /// its reference stands for the id given (any, when none is), a box for each architecture
    /// Vagrant holds it in. One recorded already keeps its row. With no engine or no Vagrant
    /// installed, nothing of that kind is held, so nothing of it is adopted. The downloads recorded
    /// for `adopted`, new or not.
    ///
    /// # Errors
    ///
    /// The engine or Vagrant couldn't say what they hold (adopt again later: nothing was
    /// recorded); the store can't be written.
    pub async fn adopt_downloads(&self, adopted: &[DownloadToAdopt]) -> Result<Vec<Download>> {
        let mut items = Vec::new();
        let processes = self.docker(None);
        let images = self.call(&processes);
        let mut held_boxes: Option<Vec<InstalledBox>> = None;
        for one in adopted {
            match one {
                DownloadToAdopt::Image { reference, image_id } => match docker::image_id(&images, reference).await {
                    Ok(Some(now)) if image_id.as_ref().is_none_or(|was| *was == now) => items.push(DownloadItem::Image {
                        reference: reference.clone(),
                        image_id: now,
                        docker_context: None,
                    }),
                    Ok(_) | Err(CliError::Missing { .. }) => {}
                    Err(e) => return Err(crate::formats::tool_error("adopt", e)),
                },
                DownloadToAdopt::Box { name, provider, version } => {
                    let held = match &held_boxes {
                        Some(held) => held,
                        None => held_boxes.insert(match vagrant::boxes(&self.call(self.config.processes.as_ref())).await {
                            Ok(held) => held,
                            Err(CliError::Missing { .. }) => Vec::new(),
                            Err(e) => return Err(crate::formats::tool_error("adopt", e)),
                        }),
                    };
                    items.extend(
                        held.iter()
                            .filter(|b| b.name == *name && b.provider == *provider && b.version == *version)
                            .cloned()
                            .map(DownloadItem::of_box),
                    );
                }
            }
        }
        self.store
            .db()
            .write(|tx| items.iter().try_for_each(|item| tx.record_download(None, item).map(drop)))?;
        let recorded = self.store.db().read(super::recorded)?;
        Ok(recorded.into_iter().filter(|d| items.contains(&d.item)).collect())
    }
}
