//! The host ports a Compose run keeps for its life: Docker gives a port published without a host
//! port (`"80"`, `"127.0.0.1::80"`) a new ephemeral one each time its container starts, so a
//! resume or a restart moved the run's addresses. Each such publish is pinned instead, at the
//! run's first start, to a port the runner picks through the home's reservations, in an override
//! of the run's own (`ports: !override`, native Compose) passed after the project's files on every
//! call, until `down`. Ports the project pins itself are left as they are. An update keeps every
//! pin and gives a publish added since one of its own.

use std::path::PathBuf;

use super::{Compose, READ_LIMIT, ToolCx, tool_error};
use crate::Result;
use crate::cli::compose::{self, ConfigPort};
use crate::error::Error;

/// The override Compose reads, in the run's state folder.
const OVERRIDE: &str = "compose-pins.yaml";

/// The pins themselves, which the override is written from.
const PINNED: &str = "compose-pins.json";

/// One pinned publish: the port as the project's files publish it, and its host port.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Pin {
    port: ConfigPort,
    host: u16,
}

/// The run's override, when it has one.
pub(super) fn file(cx: &ToolCx<'_>) -> Option<PathBuf> {
    Some(cx.state_dir.join(OVERRIDE)).filter(|p| p.is_file())
}

/// The run's pins, none when it has none yet.
fn read(cx: &ToolCx<'_>) -> Result<Vec<Pin>> {
    let path = cx.state_dir.join(PINNED);
    match crate::store::if_there(std::fs::read_to_string(&path)).map_err(Error::io(&path, "read"))? {
        Some(text) => serde_json::from_str(&text).map_err(|e| Error::io(&path, "read")(std::io::Error::new(std::io::ErrorKind::InvalidData, e))),
        None => Ok(Vec::new()),
    }
}

/// Pins every publish of the project's files that has no host port: the run's pins kept, one
/// added since given a free port no other run holds, one gone dropped. The override is written
/// (or removed, when nothing is ephemeral) before the tool starts anything.
///
/// # Errors
///
/// The config can't be read, no free port is left, or the state folder can't be written.
pub(super) async fn pin(cx: &ToolCx<'_>) -> Result<()> {
    let base = compose::Project {
        pins: None,
        ..Compose::project(cx)
    };
    let ports = compose::config_ports(&cx.call(READ_LIMIT), &base).await.map_err(|e| tool_error("pins", e))?;
    let mut pins = read(cx)?;
    pins.retain(|p| ports.contains(&p.port));
    // Each port picked is held until the override is written, so none is picked twice.
    let mut held = Vec::new();
    for port in ports.iter().filter(|p| p.published.is_none()) {
        if pins.iter().any(|p| &p.port == port) {
            continue;
        }
        let picked = cx.ports.pick(port.protocol).map_err(Error::io("a port on 127.0.0.1", "bound"))?;
        let Some(picked) = picked else {
            return Err(Error::ToolFailed {
                step: "pins".to_owned(),
                reason: format!("no free host port is left to pin service `{}`'s port {} to", port.service, port.target),
            });
        };
        pins.push(Pin {
            port: port.clone(),
            host: picked.port.get(),
        });
        held.push(picked);
    }
    let text = compose::pins_override(&ports, |p| pins.iter().find(|pin| &pin.port == p).map(|pin| pin.host));
    let Some(text) = text else {
        return forget(cx);
    };
    crate::store::create_dir(cx.state_dir)?;
    let json = serde_json::to_string(&pins).map_err(|e| Error::io(cx.state_dir, "written")(std::io::Error::other(e)))?;
    crate::store::write(&cx.state_dir.join(PINNED), json.as_bytes())?;
    crate::store::write(&cx.state_dir.join(OVERRIDE), text.as_bytes())?;
    drop(held);
    Ok(())
}

/// Drops the run's pins: its ports go with it at `down`.
pub(super) fn forget(cx: &ToolCx<'_>) -> Result<()> {
    crate::store::remove(&cx.state_dir.join(OVERRIDE))?;
    crate::store::remove(&cx.state_dir.join(PINNED))
}
