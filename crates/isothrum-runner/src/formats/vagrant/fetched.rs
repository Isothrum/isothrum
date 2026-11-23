//! What an `up` fetched: each box `box list` lists after it and didn't before. A box Vagrant held
//! already, or one another program added meanwhile and the `up` didn't, can't be told apart from
//! one the `up` added only by time: what is listed after and wasn't before is the `up`'s.

use super::{READ_LIMIT, ToolCx, read};
use crate::DownloadItem;
use crate::cli::vagrant;
use crate::tool_output::vagrant::InstalledBox;

/// `up`, run with the boxes it added recorded, however it ended: those Vagrant holds after it and
/// didn't before.
pub(super) async fn recording<T>(cx: &ToolCx<'_>, up: impl Future<Output = T>) -> T {
    let before = before(cx).await;
    let ended = up.await;
    if let Some(before) = before {
        record_added(cx, &before).await;
    }
    ended
}

/// The boxes Vagrant holds before `cx`'s `up`, when `cx` records downloads; none when they can't
/// be listed (nothing is recorded then).
async fn before(cx: &ToolCx<'_>) -> Option<Vec<InstalledBox>> {
    cx.fetched?;
    vagrant::boxes(&read(cx, READ_LIMIT))
        .await
        .inspect_err(|e| tracing::warn!(run = %cx.run, error = %e, "Vagrant's boxes couldn't be listed: what the up fetches isn't recorded"))
        .ok()
}

/// Records each box Vagrant holds now and didn't hold `before`.
async fn record_added(cx: &ToolCx<'_>, before: &[InstalledBox]) {
    let Some(record) = cx.fetched else { return };
    match vagrant::boxes(&read(cx, READ_LIMIT)).await {
        Ok(after) => after
            .into_iter()
            .filter(|held| !before.contains(held))
            .for_each(|added| record(DownloadItem::of_box(added))),
        Err(e) => tracing::warn!(run = %cx.run, error = %e, "Vagrant's boxes couldn't be listed: what the up fetched isn't recorded"),
    }
}
