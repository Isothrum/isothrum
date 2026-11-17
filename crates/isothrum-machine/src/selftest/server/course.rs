//! How the test drives its run: up step by step as the tool's lines end each ([`Course`],
//! [`UpSteps`]), SSH waited for, the command inside, and always the teardown ([`test`]).

use std::time::Duration;

use isothrum_runner::{BoxFuture, ErrorReport, RunOptions};

use super::super::Progress;
use super::{Failure, Found, RETRY, Ran, Step, StepProgress};

/// What the test drives: the run of its project (the runner's; a scripted one in tests).
pub(super) trait Deployment: Sync {
    /// Brings it up, each line its tool prints said to `line`.
    fn up<'a>(&'a self, line: &'a (dyn Fn(&str) + Send + Sync)) -> BoxFuture<'a, Result<(), Box<ErrorReport>>>;
    /// Runs `argv` inside the VM: its exit status and output.
    fn exec<'a>(&'a self, argv: &'a [&'a str]) -> BoxFuture<'a, Result<(i32, String), Box<ErrorReport>>>;
    /// Takes it down, each line its tool prints said to `line`.
    fn down<'a>(&'a self, line: &'a (dyn Fn(&str) + Send + Sync)) -> BoxFuture<'a, Result<(), Box<ErrorReport>>>;
}

/// The test's run in its runner, with its options and the VM's machine name.
pub(super) struct OnRunner<'r> {
    pub(super) run: isothrum_runner::Running<'r>,
    pub(super) opts: RunOptions,
    pub(super) machine: &'static str,
}

/// Events said to `line` as the lines their tool printed.
fn lines(line: &(dyn Fn(&str) + Send + Sync)) -> impl Fn(&isothrum_runner::Event) + Send + Sync + '_ {
    move |e: &isothrum_runner::Event| {
        if let isothrum_runner::EventKind::Log { line: said, .. } = &e.kind {
            line(said);
        }
    }
}

impl Deployment for OnRunner<'_> {
    fn up<'a>(&'a self, line: &'a (dyn Fn(&str) + Send + Sync)) -> BoxFuture<'a, Result<(), Box<ErrorReport>>> {
        Box::pin(async move { self.run.up(&self.opts, &lines(line)).await.map(drop).map_err(|e| Box::new(e.report())) })
    }

    fn exec<'a>(&'a self, argv: &'a [&'a str]) -> BoxFuture<'a, Result<(i32, String), Box<ErrorReport>>> {
        Box::pin(async move {
            let argv: Vec<String> = argv.iter().map(|a| (*a).to_owned()).collect();
            let ran = self
                .run
                .exec(self.machine, &argv, &self.opts, &super::super::quiet)
                .await
                .map_err(|e| Box::new(e.report()))?;
            Ok((ran.exit, ran.stdout))
        })
    }

    fn down<'a>(&'a self, line: &'a (dyn Fn(&str) + Send + Sync)) -> BoxFuture<'a, Result<(), Box<ErrorReport>>> {
        Box::pin(async move { self.run.down(&self.opts, &lines(line)).await.map_err(|e| Box::new(e.report())) })
    }
}

/// How one server's test VM comes up, from its tool's lines.
pub(super) trait Course: Send + Sync {
    /// The steps `up` goes through, in order: each ended by a line ([`Course::ends`]), the last
    /// by `up` succeeding. [`Step::Ssh`] among them when `up` waits for SSH itself.
    const UP: &'static [Step];

    /// The VM's machine name in the run.
    fn machine(&self) -> &'static str;

    /// The step `line` ends, as its index in [`Course::UP`]; what it says may be noted.
    fn ends(&mut self, line: &str) -> Option<usize>;

    /// How many bytes step `at` of [`Course::UP`] moves, once known.
    fn bytes(&self, _at: usize) -> Option<u64> {
        None
    }

    /// What step `at` of [`Course::UP`] found when it ends.
    fn found(&self, _at: usize) -> Found {
        Found::Nothing
    }

    /// A failure of `up`, as the step it belongs to (its index in [`Course::UP`]) and why.
    fn explain(&self, report: &ErrorReport) -> Option<(usize, Failure)>;
}

/// The test once its run is open: what an earlier test left taken down (`left`: the runner
/// records it), then up step by step, SSH waited for (within `ssh_wait`) when `up` doesn't, the
/// command, and always the teardown.
pub(super) async fn test<C: Course>(run: &impl Deployment, left: bool, course: C, progress: &(dyn Fn(StepProgress) + Send + Sync), ssh_wait: Duration) -> Ran {
    if left {
        progress(Progress::Passed(Step::Prepare, Found::Removing));
        if let Err(report) = run.down(&|_| {}).await {
            let why = Failure::Leftover(report);
            progress(Progress::Failed(Step::Prepare, why.clone()));
            return Ran {
                passed: Err(why),
                cleaned_up: None,
            };
        }
    }
    progress(Progress::Passed(Step::Prepare, Found::Nothing));
    let passed = match Box::pin(up(run, course, progress)).await {
        Ok(()) => Box::pin(inside(run, progress, C::UP.contains(&Step::Ssh), ssh_wait)).await,
        // Refused before it acted (a tool missing, a value unusable): nothing to take down.
        Err((why, false)) => {
            return Ran {
                passed: Err(why),
                cleaned_up: None,
            };
        }
        Err((why, true)) => Err(why),
    };
    progress(Progress::Running(Step::Down));
    let down = run
        .down(&|l| {
            progress(Progress::Passed(
                Step::Down,
                Found::Line {
                    text: l.to_owned(),
                    bytes: None,
                },
            ));
        })
        .await;
    let cleaned_up = match down {
        Ok(()) => {
            progress(Progress::Passed(Step::Down, Found::Nothing));
            Ok(())
        }
        Err(report) => {
            let why = Failure::Runner(report);
            progress(Progress::Failed(Step::Down, why.clone()));
            Err(why)
        }
    };
    Ran {
        passed,
        cleaned_up: Some(cleaned_up),
    }
}

/// Brings the run up along `course`, each step said as its line ends it: why not, and whether
/// the run may have changed (false: refused before it acted).
pub(super) async fn up<C: Course>(run: &impl Deployment, course: C, progress: &(dyn Fn(StepProgress) + Send + Sync)) -> Result<(), (Failure, bool)> {
    let course = std::sync::Mutex::new(UpSteps::new(course, progress));
    lock(&course).start();
    let up = run.up(&|line| lock(&course).line(line)).await;
    let mut course = course.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
    match up {
        Ok(()) => {
            course.finish();
            Ok(())
        }
        Err(report) => {
            let changed = report.changed;
            Err((course.fail(report), changed))
        }
    }
}

/// Inside the VM once up: SSH waited for (within `ssh_wait`) unless `up` did (`ssh_up`), then
/// the command. The first that fails ends them.
pub(super) async fn inside(run: &impl Deployment, progress: &(dyn Fn(StepProgress) + Send + Sync), ssh_up: bool, ssh_wait: Duration) -> Result<(), Failure> {
    let step = super::super::Steps(progress);
    if !ssh_up {
        step.run(Step::Ssh, wait_ssh(run, ssh_wait)).await?;
    }
    step.run(Step::Exec, async {
        let (exit, output) = run.exec(&["uname", "-sm"]).await.map_err(Failure::Runner)?;
        let said = output.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default().to_owned();
        if exit == 0 && said.contains("Linux") {
            Ok(Found::Uname(said))
        } else {
            Err(Failure::Command { exit, output })
        }
    })
    .await
}

/// `m`, locked whatever a panic left.
pub(super) fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Tries a command inside the VM until one runs, every [`RETRY`], within `within`: SSH answers.
pub(super) async fn wait_ssh(run: &impl Deployment, within: Duration) -> Result<Found, Failure> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match run.exec(&["true"]).await {
            Ok(_) => return Ok(Found::Nothing),
            Err(report) if tokio::time::Instant::now() >= deadline => return Err(Failure::NoSsh(report)),
            Err(_) => tokio::time::sleep(RETRY).await,
        }
    }
}

/// Where `up` is among a course's steps: the first not yet done, moved on by the lines that end
/// each.
pub(super) struct UpSteps<'p, C> {
    course: C,
    progress: &'p (dyn Fn(StepProgress) + Send + Sync),
    done: usize,
}

impl<'p, C: Course> UpSteps<'p, C> {
    fn new(course: C, progress: &'p (dyn Fn(StepProgress) + Send + Sync)) -> Self {
        UpSteps { course, progress, done: 0 }
    }

    /// Says the running step started.
    fn start(&self) {
        if let Some(step) = C::UP.get(self.done) {
            (self.progress)(Progress::Running(*step));
        }
    }

    /// The running step passed.
    fn pass(&mut self) {
        if let Some(step) = C::UP.get(self.done) {
            (self.progress)(Progress::Passed(*step, self.course.found(self.done)));
        }
        self.done += 1;
    }

    /// A line of `up`: the end of a step (and of any before it), else the running step's latest
    /// line.
    fn line(&mut self, line: &str) {
        match self.course.ends(line) {
            Some(at) if at >= self.done => {
                while self.done <= at.min(C::UP.len().saturating_sub(1)) {
                    self.pass();
                }
                self.start();
            }
            _ => {
                if let Some(step) = C::UP.get(self.done) {
                    let bytes = self.course.bytes(self.done);
                    (self.progress)(Progress::Passed(*step, Found::Line { text: line.to_owned(), bytes }));
                }
            }
        }
    }

    /// `up` succeeded: every step is done.
    fn finish(&mut self) {
        while self.done < C::UP.len() {
            self.pass();
        }
    }

    /// `up` failed with `report`: the step it failed at says why, the ones before it that no
    /// line ended are skipped.
    fn fail(&mut self, report: Box<ErrorReport>) -> Failure {
        let last = C::UP.len().saturating_sub(1);
        let (at, why) = match self.course.explain(&report) {
            Some((at, why)) => (at.max(self.done).min(last), why),
            None => (self.done.min(last), Failure::Runner(report)),
        };
        while self.done < at {
            if let Some(step) = C::UP.get(self.done) {
                (self.progress)(Progress::Skipped(*step, Found::Nothing));
            }
            self.done += 1;
        }
        if let Some(step) = C::UP.get(at) {
            (self.progress)(Progress::Failed(*step, why.clone()));
        }
        self.done = C::UP.len();
        why
    }
}
