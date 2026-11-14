//! The format driver a project's format names, statically dispatched: one match per hook, the
//! one place a new format joins.

use super::compose::Compose;
use super::{Change, FormatDriver, Found, Ran, Seen, ToolCx};
use crate::Result;
use crate::console::ShellCommand;
use crate::error::Error;
use crate::project::Format;
use crate::project::platform::Provider;
use crate::state::{Park, RunState};
use crate::store::ResourceKind;

/// The driver of one format.
#[derive(Debug, Clone, Copy)]
pub(crate) enum AnyFormat {
    /// Compose.
    Compose(Compose),
}

/// Calls `$call` on the driver `$any` holds, bound to `$d`.
macro_rules! dispatch {
    ($any:expr, $d:ident => $call:expr) => {
        match $any {
            AnyFormat::Compose($d) => $call,
        }
    };
}

impl AnyFormat {
    /// The driver of `format`.
    ///
    /// # Errors
    ///
    /// `unsupported` for a format whose driver this runner doesn't have yet.
    pub(crate) fn of(format: Format) -> Result<AnyFormat> {
        match format {
            Format::Compose => Ok(AnyFormat::Compose(Compose)),
            other => Err(Error::Unsupported {
                what: format!("running a {other} project (its driver isn't wired in this build yet)"),
            }),
        }
    }

    pub(crate) async fn probe(&self, cx: &ToolCx<'_>) -> Result<()> {
        dispatch!(self, d => d.probe(cx).await)
    }

    pub(crate) fn elevates(self, provider: Option<Provider>) -> bool {
        dispatch!(self, d => d.elevates(provider))
    }

    pub(crate) async fn up(&self, cx: &ToolCx<'_>) -> Result<()> {
        dispatch!(self, d => d.up(cx).await)
    }

    pub(crate) async fn down(&self, cx: &ToolCx<'_>) -> Result<()> {
        dispatch!(self, d => d.down(cx).await)
    }

    pub(crate) async fn park(&self, cx: &ToolCx<'_>, mode: Park) -> Result<()> {
        dispatch!(self, d => d.park(cx, mode).await)
    }

    pub(crate) async fn resume(&self, cx: &ToolCx<'_>) -> Result<()> {
        dispatch!(self, d => d.resume(cx).await)
    }

    pub(crate) async fn update(&self, cx: &ToolCx<'_>) -> Result<()> {
        dispatch!(self, d => d.update(cx).await)
    }

    pub(crate) async fn machines(&self, cx: &ToolCx<'_>) -> Result<Vec<String>> {
        dispatch!(self, d => d.machines(cx).await)
    }

    pub(crate) async fn reapply(&self, cx: &ToolCx<'_>, change: &Change) -> Result<()> {
        dispatch!(self, d => d.reapply(cx, change).await)
    }

    pub(crate) async fn provision(&self, cx: &ToolCx<'_>, machine: Option<&str>) -> Result<()> {
        dispatch!(self, d => d.provision(cx, machine).await)
    }

    pub(crate) async fn attach(&self, cx: &ToolCx<'_>, machine: &str) -> Result<()> {
        dispatch!(self, d => d.attach(cx, machine).await)
    }

    pub(crate) async fn retire(&self, cx: &ToolCx<'_>, machine: &str) -> Result<()> {
        dispatch!(self, d => d.retire(cx, machine).await)
    }

    pub(crate) async fn exec(&self, cx: &ToolCx<'_>, machine: &str, argv: &[String]) -> Result<Ran> {
        dispatch!(self, d => d.exec(cx, machine, argv).await)
    }

    pub(crate) fn console(self, cx: &ToolCx<'_>, machine: &str, user: Option<&str>) -> Result<ShellCommand> {
        dispatch!(self, d => d.console(cx, machine, user))
    }

    pub(crate) async fn observe(&self, cx: &ToolCx<'_>) -> Result<(Seen, RunState)> {
        dispatch!(self, d => d.observe(cx).await)
    }

    pub(crate) async fn found(&self, cx: &ToolCx<'_>) -> Result<Vec<Found>> {
        dispatch!(self, d => d.found(cx).await)
    }

    pub(crate) async fn destroy_by_id(&self, cx: &ToolCx<'_>, key: &str, kind: ResourceKind, native_id: &str) -> Result<()> {
        dispatch!(self, d => d.destroy_by_id(cx, key, kind, native_id).await)
    }
}
