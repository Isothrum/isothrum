//! What a start's pull fetched: the images of the project's services not built, each one whose
//! reference, once pulled, stands for an id the engine didn't hold before the pull. An image the
//! engine held already, or one another program pulled, is never recorded.

use std::collections::BTreeSet;

use super::{Compose, READ_LIMIT, ToolCx};
use crate::DownloadItem;
use crate::cli::{compose, docker};

/// The images a pull may fetch, and the ids the engine held before it.
pub(super) struct Before {
    images: Vec<String>,
    held: BTreeSet<String>,
}

impl Before {
    /// What the engine holds before `cx`'s pull, when `cx` records downloads and the project
    /// pulls an image; none when either can't be read (nothing is recorded then).
    pub(super) async fn read(cx: &ToolCx<'_>) -> Option<Before> {
        cx.fetched?;
        let call = cx.read(READ_LIMIT);
        let images = compose::images(&call, &Compose::project(cx))
            .await
            .inspect_err(|e| tracing::warn!(run = %cx.run, error = %e, "the project's images couldn't be read: what its pull fetches isn't recorded"))
            .ok()
            .filter(|images| !images.is_empty())?;
        let held = docker::image_ids(&call)
            .await
            .inspect_err(|e| tracing::warn!(run = %cx.run, error = %e, "the engine's images couldn't be listed: what the pull fetches isn't recorded"))
            .ok()?;
        Some(Before { images, held })
    }

    /// Records each image whose reference now stands for an id the engine didn't hold before.
    pub(super) async fn record_pulled(self, cx: &ToolCx<'_>) {
        let Some(record) = cx.fetched else { return };
        let call = cx.read(READ_LIMIT);
        for reference in self.images {
            match docker::image_id(&call, &reference).await {
                Ok(Some(image_id)) if !self.held.contains(&image_id) => record(DownloadItem::Image {
                    reference,
                    image_id,
                    docker_context: None,
                }),
                Ok(_) => {}
                Err(e) => tracing::warn!(run = %cx.run, image = %reference, error = %e, "the image's id couldn't be read: it isn't recorded"),
            }
        }
    }
}
