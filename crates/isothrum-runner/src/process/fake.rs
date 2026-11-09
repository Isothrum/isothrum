//! Scripted processes for tests (feature `fake`): every call is recorded and answered from the
//! test's script, so code that drives docker, vagrant or terraform runs without them and never
//! touches real ones. In this crate's tests a reply can take time (`Reply::after`): under Tokio's
//! paused clock that time passes at once, so timeouts are tested without waiting.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use super::{Exit, OnLine, ProcessError, ProcessRunner, ProcessSpec};
use crate::BoxFuture;
use crate::events::Stream;

/// One call a test's code made: the process it asked for, as it asked.
pub type Call = ProcessSpec;

/// A scripted process's answer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct Reply {
    /// Its standard output lines.
    pub stdout: Vec<String>,
    /// Its standard error lines.
    pub stderr: Vec<String>,
    /// Its exit code.
    pub code: i32,
    /// Not installed.
    pub missing: bool,
    /// How long it runs before answering.
    pub delay: Duration,
}

impl Reply {
    /// Exits 0, printing `stdout`'s lines.
    pub fn ok(stdout: &str) -> Reply {
        Reply {
            stdout: stdout.lines().map(str::to_owned).collect(),
            ..Reply::default()
        }
    }

    /// Exits with `code`, printing `stderr`'s lines.
    pub fn fail(code: i32, stderr: &str) -> Reply {
        Reply {
            stderr: stderr.lines().map(str::to_owned).collect(),
            code,
            ..Reply::default()
        }
    }

    /// Isn't installed.
    #[must_use]
    pub fn missing() -> Reply {
        Reply {
            missing: true,
            ..Reply::default()
        }
    }

    /// The same answer, after running for `delay`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn after(mut self, delay: Duration) -> Reply {
        self.delay = delay;
        self
    }
}

type Script = Box<dyn Fn(&Call) -> Reply + Send + Sync>;

/// Processes answered from a script.
pub struct FakeProcesses {
    script: Script,
    calls: Mutex<Vec<Call>>,
}

impl std::fmt::Debug for FakeProcesses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeProcesses").field("calls", &self.lines()).finish_non_exhaustive()
    }
}

impl FakeProcesses {
    /// Each call answered by `script`.
    pub fn new(script: impl Fn(&Call) -> Reply + Send + Sync + 'static) -> FakeProcesses {
        FakeProcesses {
            script: Box::new(script),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Each call answered by the first rule whose text its line contains; any other succeeds
    /// with no output.
    #[must_use]
    pub fn rules(rules: &[(&str, Reply)]) -> FakeProcesses {
        let rules: Vec<(String, Reply)> = rules.iter().map(|(k, r)| ((*k).to_owned(), r.clone())).collect();
        FakeProcesses::new(move |call| {
            let line = call.line();
            rules
                .iter()
                .find(|(k, _)| line.contains(k.as_str()))
                .map_or_else(|| Reply::ok(""), |(_, r)| r.clone())
        })
    }

    /// Every call so far.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Every call so far, as lines.
    pub(crate) fn lines(&self) -> Vec<String> {
        self.calls().iter().map(Call::line).collect()
    }

    /// The last call whose line contains `text`.
    pub fn last(&self, text: &str) -> Option<Call> {
        self.calls().into_iter().rev().find(|c| c.line().contains(text))
    }

    async fn answer(&self, spec: &ProcessSpec, on_line: OnLine<'_>) -> Result<Exit, ProcessError> {
        let reply = (self.script)(spec);
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).push(spec.clone());
        if !reply.delay.is_zero() {
            tokio::time::sleep(reply.delay).await;
        }
        if reply.missing {
            return Err(ProcessError::Missing { program: spec.program.clone() });
        }
        reply.stdout.iter().for_each(|l| on_line(Stream::Stdout, l));
        reply.stderr.iter().for_each(|l| on_line(Stream::Stderr, l));
        Ok(Exit { code: Some(reply.code) })
    }
}

impl ProcessRunner for FakeProcesses {
    fn run<'a>(&'a self, spec: &'a ProcessSpec, on_line: OnLine<'a>) -> BoxFuture<'a, Result<Exit, ProcessError>> {
        Box::pin(self.answer(spec, on_line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::capture;

    #[tokio::test(start_paused = true)]
    async fn calls_are_recorded_and_answered_by_rule() {
        let fake = FakeProcesses::rules(&[
            ("compose ps", Reply::ok("[]")),
            ("vagrant", Reply::missing()),
            ("slow", Reply::fail(2, "boom").after(Duration::from_secs(600))),
        ]);
        assert_eq!(capture(&fake, &ProcessSpec::new("docker", ["compose", "ps"])).await.unwrap().stdout, ["[]"]);
        assert!(matches!(
            capture(&fake, &ProcessSpec::new("vagrant", ["up"])).await,
            Err(ProcessError::Missing { .. })
        ));
        let slow = capture(&fake, &ProcessSpec::new("slow", Vec::<String>::new())).await.unwrap();
        assert_eq!((slow.exit, slow.stderr), (Exit { code: Some(2) }, vec!["boom".to_owned()]));
        assert_eq!(fake.lines(), ["docker compose ps", "vagrant up", "slow"]);
        assert!(fake.last("vagrant").is_some() && fake.last("terraform").is_none());
    }
}
