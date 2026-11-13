//! Scripted processes for tests (feature `fake`): every call is recorded and answered from the
//! test's script, so code that drives docker, vagrant or terraform runs without them and never
//! touches real ones. In this crate's tests a reply can take time (`Reply::after`) and print its
//! lines over time (`Reply::paced`): under Tokio's paused clock that time passes at once, so
//! timeouts are tested without waiting.

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
    /// How long it runs before each line it prints, after `delay` (a slow download's progress).
    pub pace: Duration,
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

    /// The same answer, each line printed `pace` after the one before.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn paced(mut self, pace: Duration) -> Reply {
        self.pace = pace;
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
    /// Each call answered by `script`. One answer is the fake's own: the engine probe (`docker
    /// info --format {{.ServerVersion}}`), which a script answers with a success that prints
    /// nothing (its catch-all), prints a version, as a running Docker does; the runner reads an
    /// empty answer as a daemon down (Docker 26 and older exit 0 then), so a script that means a
    /// down engine answers the probe itself, with a failure or an empty line (`"\n"`).
    pub fn new(script: impl Fn(&Call) -> Reply + Send + Sync + 'static) -> FakeProcesses {
        FakeProcesses {
            script: Box::new(move |call| {
                let reply = script(call);
                let silent_success = reply.code == 0 && !reply.missing && reply.stdout.is_empty();
                if silent_success && call.program == "docker" && call.args.first().map(String::as_str) == Some("info") {
                    return Reply::ok("29.0.0");
                }
                reply
            }),
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
        let lines = reply.stdout.iter().map(|l| (Stream::Stdout, l));
        for (stream, line) in lines.chain(reply.stderr.iter().map(|l| (Stream::Stderr, l))) {
            if !reply.pace.is_zero() {
                tokio::time::sleep(reply.pace).await;
            }
            on_line(stream, line);
        }
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

    #[tokio::test(start_paused = true)]
    async fn a_paced_reply_prints_its_lines_a_pace_apart() {
        let fake = FakeProcesses::rules(&[("pull", Reply::ok("a\nb").paced(Duration::from_secs(60)))]);
        let started = tokio::time::Instant::now();
        let mut seen = Vec::new();
        let spec = ProcessSpec::new("pull", Vec::<String>::new());
        fake.run(&spec, &mut |_, line| seen.push((line.to_owned(), started.elapsed().as_secs())))
            .await
            .unwrap();
        assert_eq!(seen, [("a".to_owned(), 60), ("b".to_owned(), 120)]);
    }
}
