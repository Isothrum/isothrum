//! The container engine's probe before a start: a stopped daemon, and one that exits 0 with no
//! version (Docker 26 and older), both refuse the start before anything is created.

use std::sync::Arc;

use super::{fixture, opts};
use crate::process::fake::{FakeProcesses, Reply};
use crate::{Error, Recorder, Runner};

/// A start on an engine whose probe (`docker info`) answers `reply`: the error, and the fake,
/// for what ran.
async fn start_with_engine_answering(reply: Reply) -> (Arc<FakeProcesses>, Error) {
    let f = fixture();
    let mut config = f.runner.config.clone();
    let fake = Arc::new(FakeProcesses::rules(&[("docker info", reply)]));
    config.processes = crate::testing::processes(&fake);
    let runner = Runner::new(config).unwrap();
    let run = runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    (fake, err)
}

#[tokio::test]
async fn a_stopped_engine_fails_the_start_before_anything_is_created() {
    let (fake, err) = start_with_engine_answering(Reply::fail(1, "Cannot connect to the Docker daemon")).await;
    assert_eq!(
        err.to_string(),
        "step `tools` failed: `docker` doesn't answer: Cannot connect to the Docker daemon (exited with code 1)"
    );
    assert_eq!(fake.lines(), ["docker info --format {{.ServerVersion}}"], "nothing but the probe ran");
}

/// Docker 26 and older exit 0 with an empty line when the daemon doesn't answer: the probe reads
/// the version printed, not the exit code alone. The reply is an
/// empty line, which the fake passes through (a bare empty success it answers as a running engine).
#[tokio::test]
async fn an_engine_answering_no_version_fails_the_start_as_a_stopped_one_does() {
    let (fake, err) = start_with_engine_answering(Reply::ok("\n")).await;
    assert!(err.to_string().contains("printed nothing (its daemon isn't running)"), "{err}");
    assert_eq!(fake.lines(), ["docker info --format {{.ServerVersion}}"], "nothing but the probe ran");
}
