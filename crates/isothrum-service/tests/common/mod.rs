//! What the service's tests share: a real service on a home, a client of it once it answers, and
//! its end waited for.

use std::path::Path;
use std::time::{Duration, Instant};

use isothrum_client::Client;
use isothrum_client::proto::{Command, Reply};
use isothrum_service::{ServeOptions, Service, StopSignal};

/// What a helper's failure fails the test with.
pub(crate) type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

/// The serving task of a service.
pub(crate) type Serving = tokio::task::JoinHandle<Result<(), isothrum_service::ServeError>>;

/// How long a test waits for what the service does on its own (starting, stopping, ending an
/// operation): long, as it only bounds a failure, and a loaded machine is slow.
pub(crate) const PATIENCE: Duration = Duration::from_secs(60);

/// A stop signal that never fires.
pub(crate) fn never() -> StopSignal {
    Box::new(|| Box::pin(std::future::pending()))
}

/// A service on `home` with `options`, and a client of it once it answers.
pub(crate) async fn serve_with(home: &Path, options: ServeOptions) -> Outcome<(Client, Serving)> {
    let mut task = tokio::spawn(Service::hold(home)?.serve(options));
    let client = answering(home, &mut task).await?;
    Ok((client, task))
}

/// A client of `home`'s service once it listens: a service that ended first is its error, one
/// that never answers fails after [`PATIENCE`].
pub(crate) async fn answering(home: &Path, task: &mut Serving) -> Outcome<Client> {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Ok(client) = Client::connect_running(home).await {
            return Ok(client);
        }
        if Instant::now() >= deadline {
            return Err(format!("the service didn't answer in {PATIENCE:?}").into());
        }
        tokio::select! {
            ended = &mut *task => return Err(format!("the service ended before it answered: {ended:?}").into()),
            () = tokio::time::sleep(Duration::from_millis(10)) => {}
        }
    }
}

/// Ends a test's service: `client` sends `asking` (a `shutdown`, or a `shutdown_if_idle` that must
/// find it idle), then `task` is waited for until `serve` returned, when it no longer holds the
/// home's files. Dropping `task` would leave it running, and on Windows the test's home couldn't
/// be removed while its store is open.
pub(crate) async fn stopped(client: &Client, asking: Command, task: Serving) -> Outcome<()> {
    let reply = client.request(asking).await?;
    if !matches!(reply, Reply::Ok { .. }) {
        return Err(format!("the service didn't stop: {reply:?}").into());
    }
    ended(task).await
}

/// `task`, the service's, once it ended: how its `serve` ended, or a failure after [`PATIENCE`].
pub(crate) async fn ended(task: Serving) -> Outcome<()> {
    Ok(tokio::time::timeout(PATIENCE, task).await.map_err(|_| "the service didn't stop")???)
}
