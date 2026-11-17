//! Self-tests: proof that this computer runs deployments, not only that its tools are there. Each
//! runs a throwaway deployment through a runner of its own (its home in the test's folder, so
//! nothing of it reaches a caller's runner), says each step as it goes, and always takes the
//! deployment down. What a test downloads is handed back, for a caller that keeps a record of its
//! downloads. A step's reason is the tool's or the runner's text; the UI words the steps.
//!
//! - [`engine`]: a two-container deployment on a container engine.

pub mod engine;

use std::path::{Path, PathBuf};

use isothrum_runner::{Event, RunName, Runner, RunnerConfig};

use crate::Context;

/// Where a step of a self-test stands.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Progress<S, F, E> {
    /// It started.
    Running(S),
    /// It passed, having found `F`.
    Passed(S, F),
    /// It failed: why.
    Failed(S, E),
}

/// Says each step's progress to its sink.
struct Steps<'a, S, F, E>(&'a (dyn Fn(Progress<S, F, E>) + Send + Sync));

impl<S: Copy, F, E: Clone> Steps<'_, S, F, E> {
    /// Runs `step`'s work, saying it started, then that it passed or why it failed.
    async fn run(&self, step: S, work: impl Future<Output = Result<F, E>>) -> Result<(), E> {
        (self.0)(Progress::Running(step));
        match work.await {
            Ok(found) => {
                (self.0)(Progress::Passed(step, found));
                Ok(())
            }
            Err(why) => {
                (self.0)(Progress::Failed(step, why.clone()));
                Err(why)
            }
        }
    }
}

/// A runner of the test's own, its home `dir/home`, and the test's project folder `dir/project`
/// holding `file` with `text`.
fn throwaway(cx: &Context, dir: &Path, file: &str, text: &str) -> Result<(Runner, PathBuf), String> {
    let project = dir.join("project");
    let written = std::fs::create_dir_all(&project).and_then(|()| std::fs::write(project.join(file), text));
    written.map_err(|e| format!("{} couldn't be written: {e}", project.display()))?;
    let mut config = RunnerConfig::new(dir.join("home"));
    config.processes = std::sync::Arc::clone(&cx.runner);
    let runner = Runner::new(config).map_err(|e| e.to_string())?;
    Ok((runner, project))
}

/// The test's run `name` in `runner`, its project `project`.
fn open<'a>(runner: &'a Runner, project: &Path, name: &str) -> Result<isothrum_runner::Running<'a>, String> {
    let name = RunName::new(name).map_err(|e| e.to_string())?;
    runner.open(project, Some(name)).map_err(|e| e.to_string())
}

/// Events nobody reads: a self-test says its own steps.
fn quiet(_: &Event) {}
