//! What a client asks of a home besides its operations: a run's record, forgetting it, the runs
//! past their lifetime, the way into a machine, its screen and its hypervisor's app, where its
//! services are reached, and the runner's key. The client never opens the home's files.

use isothrum_runner::{NativeConsole, RunName, ServiceEndpoint, ShellCommand};

use super::console::{self, HeldConsole};
use super::{Client, Reply, Request, Result, unexpected};
use crate::proto::{Command, OpOptions, Target};
use crate::v1::RunRow;
use crate::wire::write_line;

impl Client {
    /// `command`'s reply, as `pick` takes it; a reply of another kind is a protocol error.
    async fn answer<T>(&self, command: Command, pick: impl FnOnce(Reply) -> std::result::Result<T, Box<Reply>>) -> Result<T> {
        pick(self.request(command).await?).map_err(|other| unexpected(&other))
    }

    /// Run `run`'s record; none for a run never recorded.
    ///
    /// # Errors
    ///
    /// The store couldn't be read, or the connection ended.
    pub async fn record(&self, run: &RunName) -> Result<Option<RunRow>> {
        self.answer(Command::Record { run: run.clone() }, |reply| match reply {
            Reply::Run { run: row, .. } => Ok(row.map(|r| *r)),
            other => Err(Box::new(other)),
        })
        .await
    }

    /// Forgets run `run`'s record (what it created is left as it is).
    ///
    /// # Errors
    ///
    /// `locked` while an operation of the run is queued or running; the store couldn't be
    /// written, or the connection ended.
    pub async fn forget(&self, run: &RunName) -> Result<()> {
        self.request(Command::Forget { run: run.clone() }).await.map(drop)
    }

    /// The recorded runs whose lifetime passed and that may still hold something: a reaper's
    /// `down` each.
    ///
    /// # Errors
    ///
    /// The store couldn't be read, or the connection ended.
    pub async fn expired(&self) -> Result<Vec<RunRow>> {
        self.answer(Command::Expired, |reply| match reply {
            Reply::Runs { runs, .. } => Ok(runs),
            other => Err(Box::new(other)),
        })
        .await
    }

    /// The command that opens an interactive console into `machine` of run `run` (`target`:
    /// where to open it, else as recorded), as `user` when given, for this client to run in its
    /// own terminal.
    ///
    /// # Errors
    ///
    /// Where the run's format has no way in; the connection ended.
    pub async fn console_command(&self, run: &RunName, target: Option<Target>, machine: &str, user: Option<&str>) -> Result<ShellCommand> {
        let command = Command::ConsoleCommand {
            run: run.clone(),
            target,
            machine: machine.to_owned(),
            user: user.map(str::to_owned),
        };
        self.answer(command, |reply| match reply {
            Reply::Shell { command: shell, .. } => Ok(shell),
            other => Err(Box::new(other)),
        })
        .await
    }

    /// What opens running `machine` of run `run` in its hypervisor's own app, its hypervisor
    /// asked with `options`' values and secrets.
    ///
    /// # Errors
    ///
    /// `no_console` when the hypervisor has no app or the machine isn't running; the
    /// connection ended.
    pub async fn native_console(&self, run: &RunName, target: Option<Target>, options: OpOptions, machine: &str) -> Result<NativeConsole> {
        let command = Command::NativeConsole {
            run: run.clone(),
            target,
            options,
            machine: machine.to_owned(),
        };
        self.answer(command, |reply| match reply {
            Reply::Native { console, .. } => Ok(console),
            other => Err(Box::new(other)),
        })
        .await
    }

    /// Running `machine`'s graphical console, its hypervisor asked with `options`' values and
    /// secrets, on a connection of its own that keeps its tunnel (when one carries it) open until
    /// the [`HeldConsole`] is dropped.
    ///
    /// # Errors
    ///
    /// `no_console` when the machine has none, or it doesn't answer; `server_auth_failed` when
    /// a server refuses the credentials; the service doesn't answer.
    pub async fn graphical_console(&self, run: &RunName, target: Option<Target>, options: OpOptions, machine: &str) -> Result<HeldConsole> {
        let (mut reader, mut writer) = self.dedicated().await?;
        let command = Command::GraphicalConsole {
            run: run.clone(),
            target,
            options,
            machine: machine.to_owned(),
        };
        write_line(&mut writer, &Request { id: 1, command }).await?;
        let console = console::of_reply(reader.next::<Reply>().await?)?;
        Ok(HeldConsole {
            console,
            reader,
            _writer: writer,
        })
    }

    /// The service endpoints run `run` publishes, each where this computer reaches it: as the run
    /// gives it, or at a local forward through its jump host, which the service keeps open. One
    /// whose forward didn't open says why.
    ///
    /// # Errors
    ///
    /// The run's outputs can't be read; the connection ended.
    pub async fn service_endpoints(&self, run: &RunName, target: Option<Target>, options: OpOptions) -> Result<Vec<ServiceEndpoint>> {
        let command = Command::ServiceEndpoints {
            run: run.clone(),
            target,
            options,
        };
        self.answer(command, |reply| match reply {
            Reply::Endpoints { endpoints, .. } => Ok(endpoints),
            other => Err(Box::new(other)),
        })
        .await
    }

    /// The public half of the runner's SSH key, one line (made when it has none).
    ///
    /// # Errors
    ///
    /// The key couldn't be made (`tool_missing` without `ssh-keygen`) or read; the connection
    /// ended.
    pub async fn public_key(&self) -> Result<String> {
        self.answer(Command::PublicKey, |reply| match reply {
            Reply::PublicKey { key, .. } => Ok(key),
            other => Err(Box::new(other)),
        })
        .await
    }
}
