//! What the crate's tests share: a runner configuration on fakes with a fixed clock, a test's
//! model of an engine, what a test traces, the checks every named enum takes, and an HTTPS server
//! whose certificate is self-signed ([`https`]).

pub(crate) mod https;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use crate::clock::FixedClock;
use crate::net::FakePorts;
use crate::process::fake::{Call, FakeProcesses, Reply};
use crate::project::platform::HostOs;
use crate::{HostPorts, ProcessRunner, RunnerConfig};

/// When the tests' clock stands: 2026-10-15T12:00:00Z.
pub(crate) const TEST_TIME: u64 = 1_792_065_600;

/// A test's model of an engine (a Compose project, a Vagrantfile's VMs), and fake processes that
/// `answer` from it: the test reads and changes the model, the runner's calls see it. The model
/// is of a running Docker: its probe (`docker info`), when `answer` gives it a silent success,
/// prints a version (the runner reads an empty answer as a daemon down).
pub(crate) fn modelled<E: Default + Send + 'static>(answer: fn(&Mutex<E>, &Call) -> Reply) -> (Arc<Mutex<E>>, Arc<FakeProcesses>) {
    let engine = Arc::new(Mutex::new(E::default()));
    let fake = Arc::new(FakeProcesses::model({
        let engine = Arc::clone(&engine);
        move |call| {
            let reply = answer(&engine, call);
            let silent = reply.code == 0 && !reply.missing && reply.stdout.is_empty();
            if silent && call.program == "docker" && call.args.first().map(String::as_str) == Some("info") {
                return Reply::ok("29.0.0");
            }
            reply
        }
    }));
    (engine, fake)
}

/// `fake`, as the port the runner starts tools through.
pub(crate) fn processes(fake: &Arc<FakeProcesses>) -> Arc<dyn ProcessRunner> {
    Arc::<FakeProcesses>::clone(fake)
}

/// A runner at `home` on `host`, its tools `processes`, its ports `ports`, its clock at
/// [`TEST_TIME`].
pub(crate) fn runner_config(home: &Path, processes: Arc<dyn ProcessRunner>, ports: Arc<dyn HostPorts>, host: HostOs) -> RunnerConfig {
    RunnerConfig {
        home: home.to_path_buf(),
        processes,
        ports,
        clock: Arc::new(FixedClock(UNIX_EPOCH + Duration::from_secs(TEST_TIME))),
        host,
        tf_plugin_cache: None,
        host_env: crate::host_env::HostEnv::default(),
    }
}

/// No host port to give.
pub(crate) fn no_ports() -> Arc<dyn HostPorts> {
    Arc::new(FakePorts::new([]))
}

/// What `f` traces while it runs (every level, as text), with what it gives. One global test
/// subscriber writes each event to the buffer of the thread that is capturing, if any: a
/// subscriber set per test (`set_default`) races `tracing`'s global interest cache with the
/// tests running beside it, and a test then sometimes captured nothing at all. `f` runs on this
/// thread (a test's own current-thread runtime), so what it traces lands here.
pub(crate) async fn traced<T>(f: impl Future<Output = T>) -> (T, String) {
    use std::cell::RefCell;
    use std::sync::OnceLock;

    thread_local! {
        static CAPTURE: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
    }
    /// Writes to this thread's capture, or nowhere.
    struct ThisThread;
    impl std::io::Write for ThisThread {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            CAPTURE.with(|c| {
                if let Some(buf) = c.borrow_mut().as_mut() {
                    buf.extend_from_slice(bytes);
                }
            });
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(|| ThisThread)
            .finish();
        // Another test binary's subscriber can't be here: this is the only global one in tests.
        tracing::subscriber::set_global_default(subscriber).expect("the test subscriber is set once");
    });
    CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
    let given = f.await;
    let bytes = CAPTURE.with(|c| c.borrow_mut().take()).unwrap_or_default();
    (given, String::from_utf8_lossy(&bytes).into_owned())
}

/// Every variant of the unit enum `T`, read back from the names its JSON schema lists (serde's
/// own): a list nobody keeps, so a variant added is in it.
pub(crate) fn every_variant<T: schemars::JsonSchema + serde::de::DeserializeOwned>() -> Vec<T> {
    crate::every_name::<T>().iter().map(|n| crate::from_wire_name(n).unwrap()).collect()
}

/// Every variant of `T` is written as `name` writes it (as serde does), reads back as itself, and
/// no two share a name.
pub(crate) fn assert_named_as_serde<T>(name: fn(T) -> &'static str)
where
    T: Copy + PartialEq + std::fmt::Debug + serde::Serialize + serde::de::DeserializeOwned + schemars::JsonSchema,
{
    let all: Vec<T> = every_variant();
    assert!(!all.is_empty(), "no variant found in the schema");
    for &value in &all {
        let json = serde_json::to_value(value).unwrap();
        assert_eq!(json, serde_json::json!(name(value)), "the name disagrees with serde for {value:?}");
        let back: T = serde_json::from_value(json).unwrap();
        assert_eq!(back, value, "{value:?} didn't read back as itself");
    }
    let mut names: Vec<&str> = all.iter().map(|&v| name(v)).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), all.len(), "two variants share a name");
}

#[test]
fn every_variant_is_read_from_the_schema_in_order() {
    use crate::{ErrorCode, Park};
    assert_eq!(every_variant::<Park>(), [Park::Pause, Park::Shutdown]);
    let codes = every_variant::<ErrorCode>();
    assert!(codes.contains(&ErrorCode::ProjectInvalid), "{codes:?}");
}

#[tokio::test]
async fn a_capture_holds_its_own_threads_events_and_none_of_another_threads() {
    // A callsite first hit on another thread, with nothing capturing there, still traces here.
    let elsewhere = || std::thread::spawn(|| tracing::warn!(what = "elsewhere", "a trace")).join().unwrap();
    elsewhere();
    let ((), here) = traced(async {
        tracing::warn!(what = "here", "a trace");
        elsewhere();
    })
    .await;
    assert!(here.contains("what=\"here\"") || here.contains("what=here"), "{here}");
    assert!(!here.contains("elsewhere"), "another thread's trace isn't this capture's: {here}");
}

/// Runs this test binary's test `exact` alone, as a child process, with `var` set to `home`: the
/// other process of a test across processes (it acts only when it finds `var`). Returns the child
/// and whether it printed a line holding `said` (read until it did, or ended its output).
pub(crate) fn child_test_saying(exact: &str, var: &str, home: &Path, said: &str) -> (std::process::Child, bool) {
    use std::io::BufRead as _;
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", exact, "--nocapture", "--test-threads=1"])
        .env(var, home)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let saying = lines.by_ref().map_while(Result::ok).any(|line| line.contains(said));
    (child, saying)
}
