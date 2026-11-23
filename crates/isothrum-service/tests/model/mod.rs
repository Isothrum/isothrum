//! What the tests of the service over the real runner share (`round_trip.rs`,
//! `terraform_folder.rs`): a service over a runner whose tools are a model.

use std::path::Path;
use std::sync::Arc;

use isothrum_client::Client;
use isothrum_runner::fake::{Call, FakeProcesses, Reply};
use isothrum_runner::{Runner, RunnerConfig};
use isothrum_service::{RunnerEngine, ServeOptions};

use crate::common::{self, Outcome, Serving};

/// A service on `home` over a runner whose tools `answer` answers (a model of them), never
/// stopped but by a request; a client of it once it answers.
pub(crate) async fn serve(home: &Path, answer: impl Fn(&Call) -> Reply + Send + Sync + 'static) -> Outcome<(Client, Serving)> {
    let mut config = RunnerConfig::new(home);
    config.processes = Arc::new(FakeProcesses::model(answer));
    let engine = Arc::new(RunnerEngine::new(Arc::new(Runner::new(config)?)));
    common::serve_with(home, ServeOptions::new(engine, common::never())).await
}
