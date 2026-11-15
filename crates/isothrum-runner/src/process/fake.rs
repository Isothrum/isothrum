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

type Answer = Box<dyn Fn(&Call) -> Reply + Send + Sync>;

/// How often a scripted call must be made, checked when the fake is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Times {
    /// At least once.
    AtLeastOnce,
    /// Exactly this many times.
    Exactly(usize),
    /// Any number of times, none included.
    Any,
}

/// One scripted call: a program and the arguments its call starts with (`*` stands for any one
/// argument), and the answer it gets.
#[derive(Debug, Clone)]
struct Expectation {
    program: String,
    args: Vec<String>,
    reply: Reply,
    times: Times,
}

impl Expectation {
    #[expect(clippy::panic, reason = "a script that names no program is the test's mistake, said at once")]
    fn new(call: &str, reply: Reply, times: Times) -> Expectation {
        let mut words = call.split_whitespace().map(str::to_owned);
        let Some(program) = words.next() else {
            panic!("a scripted call names its program: {call:?}");
        };
        Expectation {
            program,
            args: words.collect(),
            reply,
            times,
        }
    }

    fn matches(&self, call: &Call) -> bool {
        call.program == self.program && call.args.len() >= self.args.len() && self.args.iter().zip(&call.args).all(|(want, got)| want == "*" || want == got)
    }

    fn describe(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// A strict script for [`FakeProcesses`]: every call the code makes must match one of its
/// expectations, and every expectation must be met. A call no expectation matches panics (and is
/// reported again when the fake is dropped, for a call made on a task whose panic was swallowed),
/// an expectation not met panics when the fake is dropped, and when two expectations match a
/// call the one naming more arguments answers it (two naming as many panic): the order the script
/// is written in never matters.
#[derive(Debug, Clone, Default)]
#[must_use]
pub struct Script {
    expectations: Vec<Expectation>,
}

impl Script {
    /// `call` (`program arg…`, the leading arguments of the call; `*` is any one argument) is made
    /// at least once and answered `reply`.
    pub fn expect(mut self, call: &str, reply: Reply) -> Script {
        self.expectations.push(Expectation::new(call, reply, Times::AtLeastOnce));
        self
    }

    /// `call` is made exactly `times` times, each answered `reply`.
    pub fn expect_times(mut self, call: &str, times: usize, reply: Reply) -> Script {
        self.expectations.push(Expectation::new(call, reply, Times::Exactly(times)));
        self
    }

    /// `call` may be made, any number of times, each answered `reply`.
    pub fn allow(mut self, call: &str, reply: Reply) -> Script {
        self.expectations.push(Expectation::new(call, reply, Times::Any));
        self
    }

    /// The fake processes that answer from this script.
    #[must_use]
    pub fn build(self) -> FakeProcesses {
        FakeProcesses {
            answer: Strictness::Strict {
                used: Mutex::new(vec![0; self.expectations.len()]),
                script: self,
            },
            calls: Mutex::new(Vec::new()),
            unscripted: Mutex::new(Vec::new()),
        }
    }

    /// The expectation that answers `call` (its index and itself): the one naming the most
    /// arguments among those that match it.
    fn answering(&self, call: &Call) -> Result<(usize, &Expectation), String> {
        let matching: Vec<(usize, &Expectation)> = self.expectations.iter().enumerate().filter(|(_, e)| e.matches(call)).collect();
        let most = matching.iter().map(|(_, e)| e.args.len()).max();
        let best: Vec<(usize, &Expectation)> = matching.into_iter().filter(|(_, e)| Some(e.args.len()) == most).collect();
        match best.as_slice() {
            [one] => Ok(*one),
            [] => Err(format!("unscripted call `{}`; the script expects:\n{}", call.line(), self.listing())),
            many => Err(format!(
                "call `{}` matches {} expectations equally: {}",
                call.line(),
                many.len(),
                many.iter().map(|(_, e)| format!("`{}`", e.describe())).collect::<Vec<_>>().join(", ")
            )),
        }
    }

    fn listing(&self) -> String {
        self.expectations.iter().map(|e| format!("  {}", e.describe())).collect::<Vec<_>>().join("\n")
    }
}

/// How a fake answers.
enum Strictness {
    /// From a test's function: the model of an engine, which answers whatever it is asked.
    Modelled(Answer),
    /// From a [`Script`], strictly.
    Strict { script: Script, used: Mutex<Vec<usize>> },
}

/// Processes answered from a script.
pub struct FakeProcesses {
    answer: Strictness,
    calls: Mutex<Vec<Call>>,
    /// The calls a strict script had no answer for.
    unscripted: Mutex<Vec<String>>,
}

impl std::fmt::Debug for FakeProcesses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeProcesses").field("calls", &self.lines()).finish_non_exhaustive()
    }
}

impl FakeProcesses {
    /// Each call answered by `answer`, a model of the tools that answers whatever it is asked
    /// (an engine's state the test reads and changes). Nothing is added to its answers: a model
    /// that means a running Docker answers its probe (`docker info`) with a version itself.
    /// A test that scripts a few calls uses [`FakeProcesses::script`] instead.
    pub fn model(answer: impl Fn(&Call) -> Reply + Send + Sync + 'static) -> FakeProcesses {
        FakeProcesses {
            answer: Strictness::Modelled(Box::new(answer)),
            calls: Mutex::new(Vec::new()),
            unscripted: Mutex::new(Vec::new()),
        }
    }

    /// [`FakeProcesses::model`] as the tests written before it have it: the engine probe
    /// (`docker info --format {{.ServerVersion}}`) that `answer` answers with a silent success
    /// prints a version, as a running Docker does (the runner reads an empty answer as a daemon
    /// down). Kept for those tests until they move to [`FakeProcesses::model`] or
    /// [`FakeProcesses::script`], which add nothing.
    pub fn new(answer: impl Fn(&Call) -> Reply + Send + Sync + 'static) -> FakeProcesses {
        FakeProcesses::model(move |call| {
            let reply = answer(call);
            let silent_success = reply.code == 0 && !reply.missing && reply.stdout.is_empty();
            if silent_success && call.program == "docker" && call.args.first().map(String::as_str) == Some("info") {
                return Reply::ok("29.0.0");
            }
            reply
        })
    }

    /// A strict script to build: see [`Script`].
    pub fn script() -> Script {
        Script::default()
    }

    /// Strict fakes for `expected`, each `(call, reply)` made at least once (see [`Script`]).
    #[must_use]
    pub fn expect(expected: &[(&str, Reply)]) -> FakeProcesses {
        expected
            .iter()
            .fold(Script::default(), |s, (call, reply)| s.expect(call, reply.clone()))
            .build()
    }

    /// Lenient fakes, for the tests written before [`FakeProcesses::script`]: each call answered
    /// by the first rule whose text its line contains, any other with a silent success (the engine
    /// probe with a version, as [`FakeProcesses::new`]). Such a test can't see a call it didn't
    /// expect; new tests script their calls strictly.
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

    /// The reply to `call`; a call a strict script doesn't answer panics.
    #[expect(clippy::panic, reason = "an unscripted call fails the test that made it")]
    fn reply(&self, call: &Call) -> Reply {
        match &self.answer {
            Strictness::Modelled(answer) => answer(call),
            Strictness::Strict { script, used } => match script.answering(call) {
                Ok((i, expectation)) => {
                    if let Some(n) = used.lock().unwrap_or_else(PoisonError::into_inner).get_mut(i) {
                        *n += 1;
                    }
                    expectation.reply.clone()
                }
                Err(why) => {
                    self.unscripted.lock().unwrap_or_else(PoisonError::into_inner).push(why.clone());
                    panic!("{why}");
                }
            },
        }
    }

    /// What a strict script expected and didn't get, and the calls it had no answer for.
    fn unmet(&self) -> Vec<String> {
        let mut unmet = self.unscripted.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Strictness::Strict { script, used } = &self.answer {
            let used = used.lock().unwrap_or_else(PoisonError::into_inner);
            for (e, &n) in script.expectations.iter().zip(used.iter()) {
                let met = match e.times {
                    Times::AtLeastOnce => n > 0,
                    Times::Exactly(times) => n == times,
                    Times::Any => true,
                };
                if !met {
                    unmet.push(format!("`{}` was expected {:?}, made {n} time(s)", e.describe(), e.times));
                }
            }
        }
        unmet
    }

    /// Every call made to `call` (`program arg…`, its leading arguments; `*` is any one), in
    /// order: matched as a script matches, on the program and its arguments, never on a quoted
    /// command line. Read each by meaning with [`CallView`] and [`ProcessSpec::env`].
    pub fn made(&self, call: &str) -> Vec<Call> {
        let wanted = Expectation::new(call, Reply::default(), Times::Any);
        self.calls().into_iter().filter(|c| wanted.matches(c)).collect()
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
        let reply = self.reply(spec);
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

/// A call read by meaning rather than as a quoted line: its options and their values.
pub trait CallView {
    /// The value of option `name` (`--name value` or `--name=value`), the last when given twice.
    fn flag(&self, name: &str) -> Option<&str>;
    /// Whether option or argument `name` is given.
    fn has(&self, name: &str) -> bool;
    /// Whether the arguments `run` appear together, in order.
    fn has_run(&self, run: &[&str]) -> bool;
}

impl CallView for Call {
    fn flag(&self, name: &str) -> Option<&str> {
        let prefixed = format!("{name}=");
        let mut found = None;
        let mut args = self.args.iter().peekable();
        while let Some(arg) = args.next() {
            if arg == name {
                found = args.peek().map(|v| v.as_str());
            } else if let Some(value) = arg.strip_prefix(&prefixed) {
                found = Some(value);
            }
        }
        found
    }

    fn has(&self, name: &str) -> bool {
        self.args.iter().any(|a| a == name)
    }

    fn has_run(&self, run: &[&str]) -> bool {
        run.is_empty() || self.args.windows(run.len()).any(|w| w.iter().zip(run).all(|(a, b)| a == b))
    }
}

impl Drop for FakeProcesses {
    fn drop(&mut self) {
        let unmet = self.unmet();
        if std::thread::panicking() {
            return;
        }
        assert!(
            unmet.is_empty(),
            "the scripted processes weren't run as scripted:\n{}\ncalls made:\n{}",
            unmet.join("\n"),
            self.lines().join("\n")
        );
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
    use crate::process::{Captured, capture};

    fn spec(line: &str) -> ProcessSpec {
        let mut words = line.split_whitespace();
        ProcessSpec::new(words.next().unwrap(), words)
    }

    /// The message `f` panics with.
    fn panic_of(f: impl FnOnce()) -> String {
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).expect_err("it panics");
        err.downcast_ref::<String>()
            .cloned()
            .or_else(|| err.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap()
    }

    /// `line` run to its end on `fake`, on a paused clock.
    fn run_on(fake: &FakeProcesses, line: &str) -> Result<Captured, ProcessError> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap();
        rt.block_on(capture(fake, &spec(line)))
    }

    #[tokio::test(start_paused = true)]
    async fn a_scripted_call_is_answered_and_recorded() {
        let fake = FakeProcesses::script()
            .expect("docker compose ps", Reply::ok("[]"))
            .expect("vagrant up", Reply::missing())
            .expect("slow", Reply::fail(2, "boom").after(Duration::from_secs(600)))
            .build();
        assert_eq!(capture(&fake, &spec("docker compose ps --format json")).await.unwrap().stdout, ["[]"]);
        assert!(matches!(capture(&fake, &spec("vagrant up")).await, Err(ProcessError::Missing { ref program }) if program == "vagrant"));
        let slow = capture(&fake, &spec("slow")).await.unwrap();
        assert_eq!((slow.exit, slow.stderr_tail), (Exit { code: Some(2) }, vec!["boom".to_owned()]));
        assert_eq!(fake.lines(), ["docker compose ps --format json", "vagrant up", "slow"]);
        assert_eq!(fake.last("vagrant").map(|c| c.line()), Some("vagrant up".to_owned()));
        assert!(fake.last("terraform").is_none());
    }

    #[test]
    fn an_unscripted_call_panics_naming_it_and_the_script() {
        let fake = FakeProcesses::script().allow("docker compose ps", Reply::ok("[]")).build();
        let message = panic_of(|| drop(run_on(&fake, "docker info --format x")));
        assert!(message.contains("unscripted call `docker info --format x`"), "{message}");
        assert!(message.contains("docker compose ps"), "the script is listed: {message}");
        // Dropping the fake reports it again (a call on a task whose panic was swallowed).
        let again = panic_of(move || drop(fake));
        assert!(again.contains("unscripted call `docker info --format x`"), "{again}");
    }

    #[test]
    fn a_program_must_match_whole_and_its_arguments_in_order() {
        let fake = FakeProcesses::script().allow("docker compose ps", Reply::ok("")).build();
        for line in ["dockerd compose ps", "docker ps compose", "docker compose"] {
            let message = panic_of(|| drop(run_on(&fake, line)));
            assert!(message.contains("unscripted call"), "{line}: {message}");
        }
        panic_of(move || drop(fake));
    }

    #[test]
    fn a_star_stands_for_any_one_argument() {
        let fake = FakeProcesses::script().expect_times("docker compose -p * ps", 2, Reply::ok("[]")).build();
        assert_eq!(run_on(&fake, "docker compose -p lab ps").unwrap().stdout, ["[]"]);
        assert_eq!(run_on(&fake, "docker compose -p other ps -a").unwrap().stdout, ["[]"]);
    }

    #[test]
    fn an_expectation_never_met_panics_when_the_fake_goes() {
        let fake = FakeProcesses::script()
            .allow("docker info", Reply::ok("29.0.0"))
            .expect("vagrant up", Reply::ok(""))
            .build();
        run_on(&fake, "docker info").unwrap();
        let message = panic_of(move || drop(fake));
        assert!(message.contains("`vagrant up` was expected AtLeastOnce, made 0 time(s)"), "{message}");
        assert!(message.contains("docker info"), "the calls made are listed: {message}");
    }

    #[test]
    fn a_call_made_more_often_than_its_count_panics_when_the_fake_goes() {
        let fake = FakeProcesses::script().expect_times("vagrant status", 1, Reply::ok("")).build();
        run_on(&fake, "vagrant status").unwrap();
        run_on(&fake, "vagrant status").unwrap();
        let message = panic_of(move || drop(fake));
        assert!(message.contains("`vagrant status` was expected Exactly(1), made 2 time(s)"), "{message}");
    }

    #[test]
    fn the_expectation_naming_more_arguments_answers_whatever_the_order() {
        let general_first = FakeProcesses::script()
            .allow("docker compose", Reply::ok("general"))
            .allow("docker compose ps", Reply::ok("specific"))
            .build();
        let specific_first = FakeProcesses::script()
            .allow("docker compose ps", Reply::ok("specific"))
            .allow("docker compose", Reply::ok("general"))
            .build();
        for fake in [general_first, specific_first] {
            assert_eq!(run_on(&fake, "docker compose ps -q").unwrap().stdout, ["specific"]);
            assert_eq!(run_on(&fake, "docker compose up").unwrap().stdout, ["general"]);
        }
    }

    #[test]
    fn two_expectations_matching_a_call_equally_panic() {
        let fake = FakeProcesses::script()
            .allow("docker compose * ps", Reply::ok("a"))
            .allow("docker compose -p *", Reply::ok("b"))
            .build();
        let message = panic_of(|| drop(run_on(&fake, "docker compose -p ps")));
        assert!(message.contains("matches 2 expectations equally"), "{message}");
        panic_of(move || drop(fake));
    }

    #[test]
    fn the_calls_made_are_read_by_program_arguments_and_options() {
        let fake = FakeProcesses::script().allow("docker", Reply::ok("")).allow("vagrant", Reply::ok("")).build();
        for line in [
            "docker compose -p lab --profile access up -d",
            "docker info",
            "vagrant up web --provider=libvirt",
        ] {
            run_on(&fake, line).unwrap();
        }
        let up = fake.made("docker compose * * --profile");
        assert_eq!(up.len(), 1, "{up:?}");
        let up = up.first().unwrap();
        assert_eq!((up.flag("-p"), up.flag("--profile")), (Some("lab"), Some("access")));
        assert!(up.has("-d") && up.has_run(&["access", "up"]) && !up.has_run(&["up", "access"]));
        let vagrant = fake.made("vagrant up");
        assert_eq!(vagrant.first().and_then(|c| c.flag("--provider")), Some("libvirt"));
        assert!(fake.made("dockerd").is_empty() && fake.made("docker compose ps").is_empty());
    }

    #[test]
    fn a_model_answers_whatever_it_is_asked_and_adds_nothing() {
        // Regression: the fake answered a silent `docker info` with a version of its own, so no
        // test could see how the runner reads a daemon that answers nothing.
        let model = FakeProcesses::model(|_| Reply::ok(""));
        let out = run_on(&model, "docker info --format {{.ServerVersion}}").unwrap();
        assert!(out.stdout.is_empty(), "{:?}", out.stdout);
    }

    #[tokio::test(start_paused = true)]
    async fn a_paced_reply_prints_its_lines_a_pace_apart() {
        let fake = FakeProcesses::expect(&[("pull", Reply::ok("a\nb").paced(Duration::from_secs(60)))]);
        let started = tokio::time::Instant::now();
        let mut seen = Vec::new();
        fake.run(&spec("pull"), &mut |_, line| seen.push((line.to_owned(), started.elapsed().as_secs())))
            .await
            .unwrap();
        assert_eq!(seen, [("a".to_owned(), 60), ("b".to_owned(), 120)]);
    }
}
