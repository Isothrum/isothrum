//! Whether a node can take machines where they are to go: the storages for their disks, cloud
//! images and cloud-init snippets accept that content, and the bridge they attach to exists. Each
//! problem is a [`Finding`]; how it is told, and what to change, is the caller's.

use super::api::{Iface, Storage};
use super::{CallError, Session};

/// Where machines go on a node; [`Needs::default`] is Proxmox VE's own defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Needs {
    /// The storage for disks (`images` content).
    pub disk_storage: String,
    /// The storage cloud images are downloaded to (`iso` content).
    pub image_storage: String,
    /// The storage for cloud-init snippets (`snippets` content).
    pub snippet_storage: String,
    /// The bridge machines attach to.
    pub bridge: String,
}

/// Proxmox VE's default disk storage.
pub const DEFAULT_DISK_STORAGE: &str = "local-lvm";
/// Proxmox VE's default directory storage, for images and snippets.
pub const DEFAULT_DIR_STORAGE: &str = "local";
/// Proxmox VE's default bridge.
pub const DEFAULT_BRIDGE: &str = "vmbr0";

impl Default for Needs {
    fn default() -> Needs {
        Needs {
            disk_storage: DEFAULT_DISK_STORAGE.into(),
            image_storage: DEFAULT_DIR_STORAGE.into(),
            snippet_storage: DEFAULT_DIR_STORAGE.into(),
            bridge: DEFAULT_BRIDGE.into(),
        }
    }
}

/// A content a storage accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    /// Disk images (`images`).
    Images,
    /// ISO and cloud images (`iso`).
    Iso,
    /// Cloud-init snippets (`snippets`).
    Snippets,
}

impl Content {
    /// The API's name for it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Content::Images => "images",
            Content::Iso => "iso",
            Content::Snippets => "snippets",
        }
    }
}

/// A problem a node has for [`Needs`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Finding {
    /// The storage isn't on the node, or the user can't see it.
    NoStorage {
        /// Its id.
        storage: String,
    },
    /// The storage doesn't accept a content it is needed for.
    StorageLacks {
        /// Its id.
        storage: String,
        /// What it must accept.
        content: Content,
    },
    /// The bridge isn't on the node.
    NoBridge {
        /// Its name.
        bridge: String,
    },
    /// The node's storages couldn't be listed.
    StoragesUnread(CallError),
    /// The node's network couldn't be listed.
    NetworkUnread(CallError),
}

/// Every problem `node` has for `needs`; none when it is ready.
pub async fn check(session: &Session, node: &str, needs: &Needs) -> Vec<Finding> {
    let mut findings = match session.storages(node).await {
        Ok(storages) => storage_findings(&storages, needs),
        Err(e) => vec![Finding::StoragesUnread(e)],
    };
    match session.network(node).await {
        Ok(ifaces) => findings.extend(bridge_finding(&ifaces, &needs.bridge)),
        Err(e) => findings.push(Finding::NetworkUnread(e)),
    }
    findings
}

/// The storages `needs` names that are missing, or don't accept what they are needed for.
#[must_use]
pub fn storage_findings(storages: &[Storage], needs: &Needs) -> Vec<Finding> {
    let wanted = [
        (&needs.image_storage, Content::Iso),
        (&needs.snippet_storage, Content::Snippets),
        (&needs.disk_storage, Content::Images),
    ];
    wanted
        .into_iter()
        .filter_map(|(id, content)| match storages.iter().find(|s| s.id.as_deref() == Some(id.as_str())) {
            None => Some(Finding::NoStorage { storage: id.clone() }),
            Some(s) if !s.contents().iter().any(|c| c == content.as_str()) => Some(Finding::StorageLacks { storage: id.clone(), content }),
            Some(_) => None,
        })
        .collect()
}

/// `bridge` missing from the node's interfaces.
#[must_use]
pub fn bridge_finding(ifaces: &[Iface], bridge: &str) -> Option<Finding> {
    find_iface(ifaces, bridge).is_none().then(|| Finding::NoBridge { bridge: bridge.to_owned() })
}

/// An interface by name.
#[must_use]
pub fn find_iface<'a>(ifaces: &'a [Iface], name: &str) -> Option<&'a Iface> {
    ifaces.iter().find(|i| i.name.as_deref() == Some(name))
}

/// Whether `bridge` on `node` is host-internal: it has no physical port (a NAT bridge), so only the
/// node reaches machines on it, not this computer. Not knowing is an error, never a "no".
///
/// # Errors
///
/// The node's network couldn't be read.
pub async fn bridge_is_internal(session: &Session, node: &str, bridge: &str) -> Result<bool, CallError> {
    Ok(find_iface(&session.network(node).await?, bridge).is_some_and(Iface::has_no_ports))
}
