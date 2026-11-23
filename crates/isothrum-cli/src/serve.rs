//! `isothrum serve`: the home's runner service, in this process, until it is asked to stop
//! (Ctrl-C, SIGTERM, a client's `shutdown`) or idle.

use std::sync::Arc;
use std::time::Duration;

use isothrum_runner::{Runner, RunnerConfig, SystemProcesses};
use isothrum_service::{RunnerEngine, ServeOptions, Service};

use crate::args::ServeArgs;
use crate::cancel;
use crate::error::Result;
use crate::exit::Verdict;
use crate::term::Term;

/// Serves `home` as `args` say.
///
/// # Errors
///
/// Another service holds the home, the runner can't be built on it, or the endpoint can't be
/// opened.
pub(crate) async fn run(args: &ServeArgs, home: std::path::PathBuf, term: &Term) -> Result<Verdict> {
    let service = Service::hold(home)?;
    #[cfg(unix)]
    let _hangup = if args.detach { ignore_hangup(term) } else { None };
    #[cfg(not(unix))]
    let _ = term;
    let processes = SystemProcesses::default();
    let mut config = RunnerConfig::new(service.home());
    config.processes = Arc::new(processes.clone());
    let runner = Arc::new(Runner::new(config)?);
    let stop: isothrum_service::StopSignal = Box::new(|| {
        Box::pin(async {
            if cancel::requested().await.is_err() {
                // No handler: only a client's `shutdown` or the idle time stop the service.
                std::future::pending::<()>().await;
            }
        })
    });
    let mut options = ServeOptions::new(Arc::new(RunnerEngine::new(runner)), stop);
    options.tools = Some(processes);
    options.idle = (args.idle_timeout > 0).then(|| Duration::from_secs(args.idle_timeout));
    service.serve(options).await?;
    Ok(Verdict::Pass)
}

/// Takes SIGHUP, so the terminal a client started the service from can close under it.
#[cfg(unix)]
fn ignore_hangup(term: &Term) -> Option<tokio::signal::unix::Signal> {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
        Ok(hangup) => Some(hangup),
        Err(e) => {
            term.err(&format!("warning: a hang-up will stop the service: {e}"));
            None
        }
    }
}
