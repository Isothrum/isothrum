//! What the runs' own downloads fetched: one row per image a Compose start's pull pulled and per
//! box a Vagrant `up` added, each recorded once (a second fetch of the same one keeps the first
//! row), forgotten once it is removed or found gone.

use rusqlite::{Row, params};

use super::{Tx, failed, now};
use crate::Result;
use crate::downloads::{Download, DownloadId, DownloadItem};
use crate::name::RunName;

/// The columns a [`Download`] is read from, in [`raw_download`]'s order.
const DOWNLOAD_COLUMNS: &str = "id, kind, name, image_id, docker_context, provider, version, architecture, run, fetched_at";

/// The text columns of a download's row, before they are parsed.
struct RawDownload {
    id: i64,
    kind: String,
    name: String,
    image_id: Option<String>,
    docker_context: Option<String>,
    provider: Option<String>,
    version: Option<String>,
    architecture: Option<String>,
    run: String,
    fetched_at: String,
}

fn raw_download(r: &Row<'_>) -> rusqlite::Result<RawDownload> {
    Ok(RawDownload {
        id: r.get(0)?,
        kind: r.get(1)?,
        name: r.get(2)?,
        image_id: r.get(3)?,
        docker_context: r.get(4)?,
        provider: r.get(5)?,
        version: r.get(6)?,
        architecture: r.get(7)?,
        run: r.get(8)?,
        fetched_at: r.get(9)?,
    })
}

fn download(raw: RawDownload) -> Result<Download> {
    let missing = |column: &str| super::corrupt(column, "nothing");
    let item = match raw.kind.as_str() {
        "image" => DownloadItem::Image {
            reference: raw.name,
            image_id: raw.image_id.ok_or_else(|| missing("download's image id"))?,
            docker_context: raw.docker_context,
        },
        "box" => DownloadItem::Box {
            name: raw.name,
            provider: raw.provider.ok_or_else(|| missing("download's provider"))?,
            version: raw.version.ok_or_else(|| missing("download's version"))?,
            architecture: raw.architecture,
        },
        other => return Err(super::corrupt("download kind", other)),
    };
    Ok(Download {
        id: DownloadId(raw.id),
        item,
        run: RunName::new(&raw.run).map_err(|_| super::corrupt("run name", &raw.run))?,
        fetched_at: raw.fetched_at,
        size_bytes: None,
    })
}

impl Tx<'_> {
    /// Records that run `run`'s tool fetched `item`; one recorded already keeps its row. Whether
    /// it was new to the record.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn record_download(&mut self, run: &RunName, item: &DownloadItem) -> Result<bool> {
        let (kind, name, image_id, context, provider, version, arch) = match item {
            DownloadItem::Image {
                reference,
                image_id,
                docker_context,
            } => ("image", reference, Some(image_id), docker_context.as_ref(), None, None, None),
            DownloadItem::Box {
                name,
                provider,
                version,
                architecture,
            } => ("box", name, None, None, Some(provider), Some(version), architecture.as_ref()),
        };
        let added = self
            .tx
            .execute(
                "INSERT OR IGNORE INTO downloads (kind, name, image_id, docker_context, provider, version, architecture, run, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![kind, name, image_id, context, provider, version, arch, run.as_str(), now()],
            )
            .map_err(failed("written"))?;
        Ok(added > 0)
    }

    /// Every recorded download, oldest first (its size unread).
    ///
    /// # Errors
    ///
    /// The store can't be read, or a row holds what this runner doesn't write.
    pub fn downloads(&self) -> Result<Vec<Download>> {
        let sql = format!("SELECT {DOWNLOAD_COLUMNS} FROM downloads ORDER BY id");
        let mut stmt = self.tx.prepare(&sql).map_err(failed("read"))?;
        let found = stmt.query_map([], raw_download).map_err(failed("read"))?;
        let mut rows = Vec::new();
        for raw in found {
            rows.push(download(raw.map_err(failed("read"))?)?);
        }
        Ok(rows)
    }

    /// Forgets download `id` (removed, or found gone); one not recorded is fine.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn forget_download(&mut self, id: DownloadId) -> Result<()> {
        self.tx.execute("DELETE FROM downloads WHERE id = ?1", [id.0]).map_err(failed("written"))?;
        Ok(())
    }
}
