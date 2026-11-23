//! What a start or an attach fetched: the images of the services it starts, pulled by the start's
//! pull or by `up` itself (a service started on its own, an image the pull couldn't take) or
//! built by `up`, each one whose reference, once it is done, stands for an id the engine didn't
//! hold before. An image the engine held already, or one another program pulled, is never
//! recorded.

use std::collections::BTreeSet;

use super::{Compose, READ_LIMIT, ToolCx};
use crate::DownloadItem;
use crate::cli::{compose, docker};

/// `op` (a start, an attach), run with the images it fetched of `services` (all the project
/// starts when empty) recorded, however it ended.
pub(super) async fn recording<T>(cx: &ToolCx<'_>, services: &[String], op: impl Future<Output = T>) -> T {
    let before = Before::read(cx, services).await;
    let ended = op.await;
    if let Some(before) = before {
        before.record_fetched(cx).await;
    }
    ended
}

/// The images an operation may fetch, and the ids the engine held before it.
struct Before {
    images: Vec<String>,
    held: BTreeSet<String>,
}

impl Before {
    /// What the engine holds before `cx`'s operation on `services`, when `cx` records downloads
    /// and they run an image; none when either can't be read (nothing is recorded then).
    async fn read(cx: &ToolCx<'_>, services: &[String]) -> Option<Before> {
        cx.fetched?;
        let call = cx.read(READ_LIMIT);
        let images = compose::images(&call, &Compose::project(cx), services)
            .await
            .inspect_err(|e| tracing::warn!(run = %cx.run, error = %e, "the project's images couldn't be read: what its start fetches isn't recorded"))
            .ok()
            .filter(|images| !images.is_empty())?;
        let held = docker::image_ids(&call)
            .await
            .inspect_err(|e| tracing::warn!(run = %cx.run, error = %e, "the engine's images couldn't be listed: what the start fetches isn't recorded"))
            .ok()?;
        Some(Before { images, held })
    }

    /// Records each image whose reference now stands for an id the engine didn't hold before.
    async fn record_fetched(self, cx: &ToolCx<'_>) {
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
