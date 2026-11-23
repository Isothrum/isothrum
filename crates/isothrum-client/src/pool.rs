//! One connection per home, kept for every call of a process, and made again once when it breaks:
//! a service killed or restarted since the connection was made answers a broken pipe, not a
//! refusal, and the call is worth one more try on a new connection (the service started again
//! when none runs). Only a second failure is the caller's. An operation whose sending went
//! unanswered isn't sent again but followed again ([`Pool::follow`]): the service may have
//! queued it.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use isothrum_runner::BoxFuture;
use tokio::sync::Mutex;

use crate::proto::Command;
use crate::{Client, ClientError, Followed};

/// Makes a connection to a home's service.
type Connect<C> = Box<dyn Fn(PathBuf) -> BoxFuture<'static, Result<C, ClientError>> + Send + Sync>;

/// The connections of a process to its homes' services, one per home ([`Pool::new`]: real
/// [`Client`]s; a test hands its own connection type to [`Pool::with`]).
pub struct Pool<C = Client> {
    clients: Mutex<HashMap<PathBuf, Arc<C>>>,
    connect: Connect<C>,
}

impl<C> std::fmt::Debug for Pool<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pool").finish_non_exhaustive()
    }
}

impl Pool<Client> {
    /// Connections made with [`Client::connect_exact`]: the service of this release, started
    /// (`binary`, else the `isothrum` beside this program, else the one on `PATH`) when none
    /// runs.
    #[must_use]
    pub fn new(binary: Option<PathBuf>) -> Pool<Client> {
        Pool::with(move |home| {
            let binary = binary.clone();
            Box::pin(async move { Client::connect_exact(&home, binary.as_deref()).await })
        })
    }
}

impl<C: Send + Sync + 'static> Pool<C> {
    /// Connections made by `connect`, given the home.
    pub fn with(connect: impl Fn(PathBuf) -> BoxFuture<'static, Result<C, ClientError>> + Send + Sync + 'static) -> Pool<C> {
        Pool {
            clients: Mutex::new(HashMap::new()),
            connect: Box::new(connect),
        }
    }

    /// `home`'s connection: the one kept, else a new one, kept.
    ///
    /// # Errors
    ///
    /// The connection couldn't be made (no service answers and none could be started).
    pub async fn client(&self, home: &Path) -> Result<Arc<C>, ClientError> {
        let mut clients = self.clients.lock().await;
        if let Some(client) = clients.get(home) {
            return Ok(Arc::clone(client));
        }
        let client = Arc::new((self.connect)(home.to_path_buf()).await?);
        clients.insert(home.to_path_buf(), Arc::clone(&client));
        Ok(client)
    }

    /// Forgets `home`'s connection: the next call makes a new one.
    pub async fn forget(&self, home: &Path) {
        self.clients.lock().await.remove(home);
    }

    /// Forgets every connection kept but those of the homes `keep` keeps.
    pub async fn retain(&self, keep: impl Fn(&Path) -> bool) {
        self.clients.lock().await.retain(|home, _| keep(home));
    }

    /// `ask` of `home`'s service. A connection found broken ([`ClientError::is_broken`]) is
    /// forgotten and made again, and `ask` tried once more on the new one; a second broken one is
    /// forgotten too, for the next call. For a question; an operation is [`Pool::follow`]ed.
    ///
    /// # Errors
    ///
    /// No connection could be made; the service refused or failed; the connection broke twice.
    pub async fn call<T, F>(&self, home: &Path, ask: impl Fn(Arc<C>) -> F) -> Result<T, ClientError>
    where
        F: Future<Output = Result<T, ClientError>>,
    {
        self.call_then(home, &ask, |client, _| ask(client)).await
    }

    /// `ask` of `home`'s service; a connection found broken is forgotten and made again, and
    /// `again` asked on the new one, told why the first broke; a second broken one is forgotten
    /// too.
    async fn call_then<T, F, G>(&self, home: &Path, ask: impl FnOnce(Arc<C>) -> F, again: impl FnOnce(Arc<C>, ClientError) -> G) -> Result<T, ClientError>
    where
        F: Future<Output = Result<T, ClientError>>,
        G: Future<Output = Result<T, ClientError>>,
    {
        match ask(self.client(home).await?).await {
            Err(e) if e.is_broken() => {
                tracing::info!(home = %home.display(), error = %e, "the service's connection broke: connecting again");
                self.forget(home).await;
                let again = again(self.client(home).await?, e).await;
                if again.as_ref().is_err_and(ClientError::is_broken) {
                    self.forget(home).await;
                }
                again
            }
            done => done,
        }
    }
}

impl Pool<Client> {
    /// Queues the operation `command` with `home`'s service and follows it
    /// ([`Client::follow`]), on a connection made again once when found broken, as
    /// [`Pool::call`] does. A sending the connection ended before the service answered
    /// ([`ClientError::Unanswered`]) may have been queued: it is followed again
    /// ([`Client::follow_again`]), not sent again, which would queue a second one behind it, or
    /// be refused as the run's operation already queued.
    ///
    /// # Errors
    ///
    /// As [`Pool::call`] and [`Client::follow`].
    pub async fn follow(&self, home: &Path, command: Command) -> Result<Followed, ClientError> {
        let first = command.clone();
        self.call_then(
            home,
            |client| async move { client.follow(first).await },
            |client, why| async move {
                match why {
                    ClientError::Unanswered => client.follow_again(command).await,
                    _ => client.follow(command).await,
                }
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests;
