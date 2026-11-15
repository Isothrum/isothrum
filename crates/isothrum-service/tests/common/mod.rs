//! What the service's tests share: a real service over a runner whose tools are a model.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use isothrum_client::Client;
use isothrum_runner::fake::{Call, FakeProcesses, Reply};
use isothrum_runner::{Runner, RunnerConfig};
use isothrum_service::{Engine, RunnerEngine, ServeOptions, Service, StopSignal};

/// What a helper's failure fails the test with.
pub(crate) type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

/// The serving task of a service.
pub(crate) type Serving = tokio::task::JoinHandle<Result<(), isothrum_service::ServeError>>;

/// A service on `home` over a runner whose tools `answer` answers, never stopped but by a
/// request; a client of it once it answers.
pub(crate) async fn serve(home: &Path, answer: impl Fn(&Call) -> Reply + Send + Sync + 'static) -> Outcome<(Client, Serving)> {
    let mut config = RunnerConfig::new(home);
    config.processes = Arc::new(FakeProcesses::model(answer));
    let engine: Arc<dyn Engine> = Arc::new(RunnerEngine::new(Arc::new(Runner::new(config)?)));
    let never: StopSignal = Box::new(|| Box::pin(std::future::pending()));
    let task = tokio::spawn(Service::hold(home)?.serve(ServeOptions::new(engine, never)));
    for _ in 0..200 {
        if let Ok(client) = Client::connect_running(home).await {
            return Ok((client, task));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err("the service didn't answer".into())
}
