//! The embedder's sidecars around lifecycle edges: what an embedder keeps beside a run (a
//! container of its own on the run's network, say) starts or rejoins after `up`, `update` and
//! `resume`, stops before `park`, and goes before `down` so the run's network is free. A hook's failure is
//! a warning event, never a failed operation.

use isoloom_manifest::Manifest;

use crate::BoxFuture;
use crate::name::RunName;
use crate::status::{NetworkView, OutputValue};

/// Why a hook failed, in its own words.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct HookError(String);

impl HookError {
    /// A hook's failure, in its words.
    pub fn new(message: impl Into<String>) -> HookError {
        HookError(message.into())
    }
}

impl From<String> for HookError {
    fn from(message: String) -> HookError {
        HookError(message)
    }
}

impl From<&str> for HookError {
    fn from(message: &str) -> HookError {
        HookError(message.to_owned())
    }
}

/// What a hook sees of the run.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunView<'a> {
    /// The run's name.
    pub name: &'a RunName,
    /// The manifest.
    pub manifest: &'a Manifest,
    /// The run's networks, with their native names.
    pub networks: &'a [NetworkView],
    /// The run's outputs (empty before it is up).
    pub outputs: &'a [OutputValue],
}

/// Hooks around a run's lifecycle edges; each defaults to doing nothing.
pub trait Hooks: Send + Sync {
    /// After a successful `up`, and after a successful `update`: an update brings the run up
    /// (its machines or networks may have changed), so what joins a run that came up joins it
    /// again. A hook that must run once per run keeps its own track.
    fn after_up<'a>(&'a self, run: &'a RunView<'a>) -> BoxFuture<'a, Result<(), HookError>> {
        let _ = run;
        Box::pin(async { Ok(()) })
    }

    /// Before `down`.
    fn before_down<'a>(&'a self, run: &'a RunView<'a>) -> BoxFuture<'a, Result<(), HookError>> {
        let _ = run;
        Box::pin(async { Ok(()) })
    }

    /// Before `park`.
    fn before_park<'a>(&'a self, run: &'a RunView<'a>) -> BoxFuture<'a, Result<(), HookError>> {
        let _ = run;
        Box::pin(async { Ok(()) })
    }

    /// After a successful `resume`.
    fn after_resume<'a>(&'a self, run: &'a RunView<'a>) -> BoxFuture<'a, Result<(), HookError>> {
        let _ = run;
        Box::pin(async { Ok(()) })
    }
}

/// How long a hook may take: the embedder's sidecar is an external call like any other.
const HOOK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// A lifecycle edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edge {
    AfterUp,
    BeforeDown,
    BeforePark,
    AfterResume,
}

impl Edge {
    fn name(self) -> &'static str {
        match self {
            Edge::AfterUp => "after_up",
            Edge::BeforeDown => "before_down",
            Edge::BeforePark => "before_park",
            Edge::AfterResume => "after_resume",
        }
    }

    /// Runs `hooks` at this edge; each failure is returned with the hook's edge.
    pub(crate) async fn run(self, hooks: &[std::sync::Arc<dyn Hooks>], run: &RunView<'_>) -> Vec<String> {
        let mut failures = Vec::new();
        for hook in hooks {
            let call = match self {
                Edge::AfterUp => hook.after_up(run),
                Edge::BeforeDown => hook.before_down(run),
                Edge::BeforePark => hook.before_park(run),
                Edge::AfterResume => hook.after_resume(run),
            };
            match tokio::time::timeout(HOOK_TIMEOUT, call).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => failures.push(format!("{}: {e}", self.name())),
                Err(_) => failures.push(format!("{}: no answer within {}s", self.name(), HOOK_TIMEOUT.as_secs())),
            }
        }
        failures
    }
}
