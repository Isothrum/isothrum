//! The crate's own test doubles.

use std::time::Duration;

use isothrum_runner::fake::FakeProcesses;
use isothrum_runner::{BoxFuture, Exit, OnLine, ProcessError, ProcessRunner, ProcessSpec};

/// A tool that never answers.
pub(crate) struct Stuck;

impl ProcessRunner for Stuck {
    fn run<'a>(&'a self, _spec: &'a ProcessSpec, _on_line: OnLine<'a>) -> BoxFuture<'a, Result<Exit, ProcessError>> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok(Exit::new(Some(0)))
        })
    }
}

/// One call that never answers (its whole command line), every other answered by a strict script.
pub(crate) struct StuckOn {
    /// The call that hangs.
    call: String,
    /// What answers the others.
    rest: FakeProcesses,
}

impl StuckOn {
    /// `call` hangs, `rest` answers the others.
    pub(crate) fn new(call: &str, rest: FakeProcesses) -> StuckOn {
        StuckOn { call: call.to_owned(), rest }
    }
}

impl ProcessRunner for StuckOn {
    fn run<'a>(&'a self, spec: &'a ProcessSpec, on_line: OnLine<'a>) -> BoxFuture<'a, Result<Exit, ProcessError>> {
        if spec.line() == self.call {
            Stuck.run(spec, on_line)
        } else {
            self.rest.run(spec, on_line)
        }
    }
}
