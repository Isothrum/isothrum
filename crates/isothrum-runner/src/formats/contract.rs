//! The contract every format driver keeps, written once and run against each driver's model of
//! its tool ([`Model`]: a [`FakeProcesses::model`] whose world changes as the real tool's does,
//! answering in the tool's own recorded output). Each driver's tests run the whole suite with
//! [`suite!`]:
//!
//! - `probe` reads the tool's version: one recent enough passes, one too old is `tool_too_old`;
//! - `up`, then `observe`, reads the run ready, with its machines running;
//! - `found` lists exactly what the tool holds, by native id; `destroy_by_id` of each removes it,
//!   and of one already gone is done;
//! - `down`, then `observe`, reads the run absent, nothing left;
//! - a tool missing is `tool_missing`, at the probe and at `up`;
//! - what the driver refuses is `unsupported`, before any call.

use std::path::Path;
use std::sync::Arc;

use super::any::AnyFormat;
use super::{Found, ToolCx};
use crate::ErrorCode;
use crate::Result;
use crate::RunName;
use crate::process::fake::{Call, FakeProcesses, Reply};
use crate::project::platform::Provider;
use crate::project::{Format, Project};
use crate::state::{Park, RunState};
use crate::status::MachineState;

/// What every model of a tool names: the calls it answers and how, and the format and provider
/// it stands for.
pub(crate) use crate::process::fake::{Call as ToolCall, Reply as ToolReply};
pub(crate) use crate::project::{Format as RunFormat, platform::Provider as RunProvider};

/// What a driver may refuse outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    Park,
    Resume,
    Provision,
    Attach,
    Retire,
    Exec,
    Console,
}

/// A model of one driver's tool: its world (what it made), changed by the calls the driver makes
/// as the real tool's would be, and its answers, the tool's own recorded output.
pub(crate) trait Model: Send + Sync + 'static {
    /// The format whose driver runs the tool.
    const FORMAT: Format;
    /// The hypervisor the run asks for.
    const PROVIDER: Option<Provider>;
    /// The project's file: its name and what it says.
    const FILE: (&'static str, &'static str);
    /// The call that reads the tool's version (`program arg…`, as [`FakeProcesses::made`] takes it).
    const VERSION: &'static str;
    /// The machines a ready run has, sorted.
    const MACHINES: &'static [&'static str];
    /// What the driver refuses.
    const REFUSES: &'static [Op];
    /// The tool with nothing made yet, recent enough, or too old when `old`.
    fn new(old: bool) -> Self;
    /// Its answer to `call`, its world changed as the tool's own.
    fn answer(&self, call: &Call) -> Reply;
    /// The native ids of what it holds now, sorted.
    fn holds(&self) -> Vec<String>;
}

/// A project of the model's format, a run's state folder, and the model behind the processes.
struct Bench<M: Model> {
    _dir: tempfile::TempDir,
    state: tempfile::TempDir,
    project: Project,
    run: RunName,
    model: Arc<M>,
    fake: FakeProcesses,
}

fn quiet(_: crate::events::Stream, _: &str) {}

fn unwarned(_: crate::events::WarningCode, _: &str) {}

impl<M: Model> Bench<M> {
    /// The model `old` or not answers every call.
    fn new(old: bool) -> Bench<M> {
        let model = Arc::new(M::new(old));
        let answering = Arc::clone(&model);
        Bench::with(model, FakeProcesses::model(move |call| answering.answer(call)))
    }

    /// Nothing is installed: every call is answered as a program missing.
    fn missing() -> Bench<M> {
        Bench::with(Arc::new(M::new(false)), FakeProcesses::model(|_| Reply::missing()))
    }

    /// Nothing may run: any call fails the test.
    fn silent() -> Bench<M> {
        Bench::with(Arc::new(M::new(false)), FakeProcesses::script().build())
    }

    fn with(model: Arc<M>, fake: FakeProcesses) -> Bench<M> {
        let dir = tempfile::tempdir().expect("a folder");
        let (name, text) = M::FILE;
        std::fs::write(dir.path().join(name), text).expect("the project's file");
        let project = Project::open(dir.path(), Some(M::FORMAT)).expect("a project");
        Bench {
            _dir: dir,
            state: tempfile::tempdir().expect("a state folder"),
            project,
            run: RunName::new("lab").expect("a name"),
            model,
            fake,
        }
    }

    fn cx(&self) -> ToolCx<'_> {
        ToolCx {
            project: &self.project,
            run: &self.run,
            key: "k3y9ab",
            state_dir: self.state.path(),
            provider: M::PROVIDER,
            env: vec![
                ("ISOTHRUM_RUN_KEY".to_owned(), "k3y9ab".to_owned()),
                ("ISOLOOM_KEY".to_owned(), "k3y9ab".to_owned()),
            ],
            host_env: &crate::host_env::EMPTY,
            processes: &self.fake,
            sink: &quiet,
            warn: &unwarned,
        }
    }

    fn driver() -> AnyFormat {
        AnyFormat::of(M::FORMAT)
    }

    async fn up(&self) {
        Self::driver().up(&self.cx()).await.expect("up");
    }

    async fn found(&self) -> Vec<Found> {
        Self::driver().found(&self.cx()).await.expect("found")
    }
}

/// `op` tried on the run's machine `web`.
async fn attempt(cx: &ToolCx<'_>, op: Op) -> Result<()> {
    let d = AnyFormat::of(cx.project.format);
    match op {
        Op::Park => d.park(cx, Park::Pause).await,
        Op::Resume => d.resume(cx).await,
        Op::Provision => d.provision(cx, None, None).await,
        Op::Attach => d.attach(cx, "web").await,
        Op::Retire => d.retire(cx, "web").await,
        Op::Exec => d.exec(cx, "web", &["true".to_owned()]).await.map(drop),
        Op::Console => d.console(cx, "web", None).map(drop),
    }
}

/// A recent tool's version passes the probe, read once; one too old is refused as such.
pub(crate) async fn probe_reads_the_version<M: Model>() {
    let bench = Bench::<M>::new(false);
    Bench::<M>::driver().probe(&bench.cx()).await.expect("a recent tool");
    assert_eq!(bench.fake.made(M::VERSION).len(), 1, "{:?}", bench.fake.calls());
    let old = Bench::<M>::new(true);
    let e = Bench::<M>::driver().probe(&old.cx()).await.expect_err("too old");
    assert_eq!(e.code(), ErrorCode::ToolTooOld, "{e}");
}

/// After `up`, `observe` reads the run ready, its machines each running.
pub(crate) async fn up_then_observe_reads_it_ready<M: Model>() {
    let bench = Bench::<M>::new(false);
    bench.up().await;
    let (seen, state) = Bench::<M>::driver().observe(&bench.cx()).await.expect("observed");
    assert_eq!(state, RunState::Ready, "{seen:?}");
    let mut names: Vec<&str> = seen.machines.iter().map(|m| m.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, M::MACHINES);
    isothrum_test_support::assert_every!(&seen.machines, |m: &&super::SeenMachine| m.state == MachineState::Running);
}

/// `found` is what the tool holds, by native id; each removed by its id is gone from the tool and
/// from `found`, and removing it again is done.
pub(crate) async fn found_ids_are_destroyed_by_id_and_gone_is_done<M: Model>() {
    let bench = Bench::<M>::new(false);
    bench.up().await;
    let found = bench.found().await;
    let mut ids: Vec<String> = found.iter().map(|f| f.native_id.clone()).collect();
    ids.sort();
    assert!(!ids.is_empty(), "an up makes something");
    assert_eq!(ids, bench.model.holds(), "found is what the tool holds");
    let driver = Bench::<M>::driver();
    for f in &found {
        driver.destroy_by_id(&bench.cx(), &f.key, f.kind, &f.native_id).await.expect("removed by id");
        assert!(!bench.model.holds().contains(&f.native_id), "{f:?} is gone from the tool");
    }
    assert_eq!(bench.found().await, Vec::<Found>::new());
    for f in &found {
        driver
            .destroy_by_id(&bench.cx(), &f.key, f.kind, &f.native_id)
            .await
            .expect("already gone is done");
    }
}

/// After `down`, `observe` reads the run absent, and the tool holds nothing.
pub(crate) async fn down_then_observe_reads_it_absent<M: Model>() {
    let bench = Bench::<M>::new(false);
    bench.up().await;
    let driver = Bench::<M>::driver();
    driver.down(&bench.cx()).await.expect("down");
    let (seen, state) = driver.observe(&bench.cx()).await.expect("observed");
    assert_eq!(state, RunState::Absent, "{seen:?}");
    assert!(seen.machines.iter().all(|m| m.state == MachineState::Absent), "{seen:?}");
    assert_eq!(bench.model.holds(), Vec::<String>::new());
}

/// A tool not installed is `tool_missing`, at the probe and at the start.
pub(crate) async fn a_missing_tool_reads_as_tool_missing<M: Model>() {
    let bench = Bench::<M>::missing();
    let driver = Bench::<M>::driver();
    let probed = driver.probe(&bench.cx()).await.expect_err("nothing installed");
    assert_eq!(probed.code(), ErrorCode::ToolMissing, "{probed}");
    let started = driver.up(&bench.cx()).await.expect_err("nothing installed");
    assert_eq!(started.code(), ErrorCode::ToolMissing, "{started}");
}

/// What the driver refuses is `unsupported` and runs nothing; a console it doesn't refuse is
/// given without running anything either.
pub(crate) async fn what_it_refuses_is_unsupported<M: Model>() {
    let bench = Bench::<M>::silent();
    let cx = bench.cx();
    for op in M::REFUSES {
        let e = attempt(&cx, *op).await.expect_err("refused");
        assert_eq!(e.code(), ErrorCode::Unsupported, "{op:?}: {e}");
    }
    if !M::REFUSES.contains(&Op::Console) {
        attempt(&cx, Op::Console).await.expect("a console");
    }
    assert!(bench.fake.calls().is_empty(), "{:?}", bench.fake.lines());
}

/// The tool's argument after `flag=` among `call`'s, as a path.
pub(crate) fn flag_path<'c>(call: &'c Call, flag: &str) -> Option<&'c Path> {
    call.args.iter().find_map(|a| a.strip_prefix(flag)).map(Path::new)
}

/// Runs the contract against the model `$model`: one test per clause.
macro_rules! suite {
    ($model:ty) => {
        #[tokio::test]
        async fn contract_probe_reads_the_version() {
            $crate::formats::contract::probe_reads_the_version::<$model>().await;
        }

        #[tokio::test]
        async fn contract_up_then_observe_reads_it_ready() {
            $crate::formats::contract::up_then_observe_reads_it_ready::<$model>().await;
        }

        #[tokio::test]
        async fn contract_found_ids_are_destroyed_by_id_and_gone_is_done() {
            $crate::formats::contract::found_ids_are_destroyed_by_id_and_gone_is_done::<$model>().await;
        }

        #[tokio::test]
        async fn contract_down_then_observe_reads_it_absent() {
            $crate::formats::contract::down_then_observe_reads_it_absent::<$model>().await;
        }

        #[tokio::test]
        async fn contract_a_missing_tool_reads_as_tool_missing() {
            $crate::formats::contract::a_missing_tool_reads_as_tool_missing::<$model>().await;
        }

        #[tokio::test]
        async fn contract_what_it_refuses_is_unsupported() {
            $crate::formats::contract::what_it_refuses_is_unsupported::<$model>().await;
        }
    };
}
pub(crate) use suite;
