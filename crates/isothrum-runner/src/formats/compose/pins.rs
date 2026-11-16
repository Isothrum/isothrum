//! The host ports a Compose run keeps for its life: Docker gives a port published without a host
//! port (`"80"`, `"127.0.0.1::80"`) a new ephemeral one each time its container starts, so a
//! resume or a restart moved the run's addresses. Each such publish is pinned instead, at the
//! run's first start, to a port the runner picks through the home's reservations, in an override
//! of the run's own (`ports: !override`, native Compose) passed after the project's files on every
//! call, until `down`. Ports the project pins itself are left as they are. An update keeps every
//! pin and gives a publish added since one of its own. A service under a profile the run attached
//! (an attack box) is pinned when it is attached, kept by every later start, and dropped when it is
//! retired.

use std::path::PathBuf;

use super::{Compose, READ_LIMIT, ToolCx, tool_error};
use crate::Result;
use crate::cli::compose::{self, ConfigPort};
use crate::error::Error;

/// The override Compose reads, in the run's state folder.
const OVERRIDE: &str = "compose-pins.yaml";

/// What the override is written from.
const PINNED: &str = "compose-pins.json";

/// One pinned publish: the port as the project's files publish it, and its host port.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Pin {
    port: ConfigPort,
    host: u16,
}

/// What a run keeps of its ports: the services under a profile it attached, every port its
/// services publish (the override lists each pinned service's ports whole), and the pins.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Kept {
    #[serde(default)]
    attached: Vec<String>,
    #[serde(default)]
    ports: Vec<ConfigPort>,
    #[serde(default)]
    pins: Vec<Pin>,
}

/// The run's override, when it has one.
pub(super) fn file(cx: &ToolCx<'_>) -> Option<PathBuf> {
    Some(cx.state_dir.join(OVERRIDE)).filter(|p| p.is_file())
}

/// Pins every publish of the services the run starts and of those it attached: their pins kept,
/// one added since given a free port no other run holds, one gone dropped, the override written
/// before the tool starts anything.
///
/// # Errors
///
/// The config can't be read, no free port is left, or the state folder can't be written.
pub(super) async fn pin(cx: &ToolCx<'_>, first: bool) -> Result<()> {
    let kept: Kept = crate::store::read_json(&cx.state_dir.join(PINNED))?;
    refresh(cx, kept, first).await
}

/// [`pin`], `service` (one under a profile, started on its own) attached.
pub(super) async fn attach(cx: &ToolCx<'_>, service: &str) -> Result<()> {
    let mut kept: Kept = crate::store::read_json(&cx.state_dir.join(PINNED))?;
    if !kept.attached.iter().any(|s| s == service) {
        kept.attached.push(service.to_owned());
    }
    refresh(cx, kept, false).await
}

/// The services under a profile the run attached, and hasn't retired.
pub(super) fn attached(cx: &ToolCx<'_>) -> Result<Vec<String>> {
    Ok(crate::store::read_json::<Kept>(&cx.state_dir.join(PINNED))?.attached)
}

/// Drops `service`'s pins: it is retired.
pub(super) fn retire(cx: &ToolCx<'_>, service: &str) -> Result<()> {
    let mut kept: Kept = crate::store::read_json(&cx.state_dir.join(PINNED))?;
    kept.attached.retain(|s| s != service);
    kept.ports.retain(|p| p.service != service);
    kept.pins.retain(|p| p.port.service != service);
    store(cx, &kept)
}

/// Reads the ports the run's services publish, pins each ephemeral one, and keeps them. At a
/// `first` start, the host ports the project fixes itself are claimed first ([`ToolCx::ports`]).
async fn refresh(cx: &ToolCx<'_>, mut kept: Kept, first: bool) -> Result<()> {
    let base = compose::Project {
        pins: None,
        ..Compose::project(cx)
    };
    let call = cx.read(READ_LIMIT);
    let mut ports = compose::config_ports(&call, &base, &[]).await.map_err(|e| tool_error("pins", e))?;
    if !kept.attached.is_empty() {
        let theirs = compose::config_ports(&call, &base, &kept.attached).await.map_err(|e| tool_error("pins", e))?;
        ports.extend(theirs.into_iter().filter(|p| !ports.contains(p)).collect::<Vec<_>>());
    }
    if first {
        let fixed: Vec<_> = ports
            .iter()
            .filter_map(|p| Some((crate::port::HostPort::new(p.published?), p.protocol)))
            .collect();
        cx.ports.claim(&fixed)?;
    }
    kept.pins.retain(|p| ports.contains(&p.port));
    // Each port picked is held until the override is written, so none is picked twice.
    let mut held = Vec::new();
    for port in ports.iter().filter(|p| p.published.is_none()) {
        if kept.pins.iter().any(|p| &p.port == port) {
            continue;
        }
        let picked = cx.ports.pick(port.protocol).map_err(Error::io("a port on 127.0.0.1", "bound"))?;
        let Some(picked) = picked else {
            return Err(Error::ToolFailed {
                step: "pins".to_owned(),
                reason: format!("no free host port is left to pin service `{}`'s port {} to", port.service, port.target),
            });
        };
        kept.pins.push(Pin {
            port: port.clone(),
            host: picked.port.get(),
        });
        held.push(picked);
    }
    kept.ports = ports;
    store(cx, &kept)?;
    drop(held);
    Ok(())
}

/// Writes what the run keeps, and the override it gives (none when nothing is pinned); nothing
/// when it keeps nothing.
fn store(cx: &ToolCx<'_>, kept: &Kept) -> Result<()> {
    if kept.attached.is_empty() && kept.pins.is_empty() {
        return forget(cx);
    }
    crate::store::write_json(&cx.state_dir.join(PINNED), kept)?;
    let text = compose::pins_override(&kept.ports, |p| kept.pins.iter().find(|pin| &pin.port == p).map(|pin| pin.host));
    match text {
        Some(text) => crate::store::write(&cx.state_dir.join(OVERRIDE), text.as_bytes()),
        None => crate::store::remove(&cx.state_dir.join(OVERRIDE)),
    }
}

/// Drops the run's pins: its ports go with it at `down`.
pub(super) fn forget(cx: &ToolCx<'_>) -> Result<()> {
    crate::store::remove(&cx.state_dir.join(OVERRIDE))?;
    crate::store::remove(&cx.state_dir.join(PINNED))
}
