//! The Compose adapter: `docker compose` on one project, its failures read as [`Failure`] and its
//! `ps --all --format json` as the project's containers ([`Container`]).

use std::fmt;
use std::path::PathBuf;

use super::{Call, CliError, argv, said_any};
use crate::process::Captured;

/// The tool Compose runs as.
const TOOL: &str = "docker";

mod config;
mod ps;

pub(crate) use config::{ConfigPort, images, namespace_hosts, pins_override, ports as config_ports};
pub(crate) use ps::Container;

/// A failed Compose call, as far as a driver acts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// A container name an earlier run's container holds (`is already in use by container`).
    NameInUse,
    /// A network of the project's name already there, or with other labels (`network with name
    /// … already exists`, `has incorrect label`).
    NetworkExists,
    /// A host port it publishes is taken (`port is already allocated`, `address already in use`):
    /// the port and its protocol, when Docker names them.
    PortTaken(Option<(u16, crate::project::platform::Protocol)>),
    /// The registry refused an image's pull: none of that name, a tag it doesn't have, a sign-in
    /// it needs, its pull rate limit (`pull access denied`, `manifest unknown`, `failed to
    /// resolve reference … not found`, `toomanyrequests`, `no matching manifest`): what the
    /// tool said.
    PullRefused(String),
    /// A pull the network cut short, worth another try (a reset connection, a registry's TLS
    /// handshake or its gateway timing out): what the tool said.
    PullTransient(String),
    /// The engine didn't answer: stopped, or quit while the call ran (`Cannot connect to the
    /// Docker daemon`; on Windows, `error during connect` to a pipe that is gone): what the tool
    /// said.
    EngineDown(String),
    /// The engine refused this account its socket (unix) or named pipe (Windows): what the tool
    /// said.
    EngineDenied(String),
    /// A network the project makes needs an address block another network already holds (`Pool
    /// overlaps with other one on this address space`): what the tool said.
    PoolOverlap(String),
    /// Any other failure: its reason.
    Other(String),
}

/// The host port (and its protocol) Docker says is taken, in each of its wordings:
/// `Bind for ADDRESS:PORT failed`, `failed to bind host port ADDRESS:PORT/PROTOCOL`, and
/// `listen PROTOCOL ADDRESS:PORT: bind` (Docker Desktop's). The port is what follows the
/// address's last `:` (`0.0.0.0:8180`, `[::]:8180`); a protocol Docker doesn't say is TCP's.
fn taken_port(out: &Captured) -> Option<(u16, crate::project::platform::Protocol)> {
    use crate::project::platform::Protocol;
    let protocol_of = |word: &str| {
        if word.to_ascii_lowercase().starts_with("udp") {
            Protocol::Udp
        } else {
            Protocol::Tcp
        }
    };
    let port_of = |address: &str| address.rsplit_once(':').and_then(|(_, port)| port.parse::<u16>().ok());
    out.stdout.iter().chain(&out.stderr_tail).find_map(|line| {
        if let Some((_, rest)) = line.split_once("Bind for ") {
            let address = rest.split_whitespace().next()?;
            return Some((port_of(address)?, Protocol::Tcp));
        }
        if let Some((_, rest)) = line.split_once("failed to bind host port ") {
            let bound = rest.split_whitespace().next()?.trim_end_matches(':');
            let (address, protocol) = bound.rsplit_once('/').map_or((bound, "tcp"), |(a, p)| (a, p));
            return Some((port_of(address)?, protocol_of(protocol)));
        }
        let (_, rest) = line.split_once("listen ")?;
        let mut words = rest.split_whitespace();
        let protocol = words.next()?;
        let address = words.next()?.trim_end_matches(':');
        Some((port_of(address)?, protocol_of(protocol)))
    })
}

/// What Docker's CLI says when its engine doesn't answer, as [`said_any`] reads it (lowercase):
/// "Cannot connect to the Docker daemon … Is the docker daemon running?" up to 28, "failed to
/// connect to the docker API at … if the daemon is running" since 29 (Docker Desktop stopped
/// during an `up`). Its socket's refusal says "permission denied" with it: [`engine_denied`],
/// read first.
const ENGINE_DOWN: &[&str] = &[
    "cannot connect to the docker daemon",
    "is the docker daemon running",
    "error during connect",
    "failed to connect to the docker api",
];

/// Whether `out` says the engine refused this account: its socket's `permission denied` (`… the
/// docker API at unix:///var/run/docker.sock`, `… the Docker daemon socket`), or its named pipe's
/// `Access is denied` on Windows. A refusal of anything else (a bind mount's file) isn't one.
fn engine_denied(out: &Captured) -> bool {
    out.stdout.iter().chain(&out.stderr_tail).any(|l| {
        let l = l.to_lowercase();
        let socket = l.contains("permission denied") && ["docker.sock", "docker api", "docker daemon"].iter().any(|w| l.contains(w));
        let pipe = l.contains("access is denied") && l.contains("pipe");
        socket || pipe
    })
}

/// What Docker says of a pull the network cut short, as [`said_any`] reads it (lowercase): Go's
/// net/http and TLS wordings, and a registry's gateway that answered for it.
const PULL_TRANSIENT: &[&str] = &[
    "connection reset by peer",
    "tls handshake timeout",
    "i/o timeout",
    "unexpected eof",
    "502 bad gateway",
    "503 service unavailable",
    "504 gateway timeout",
    "request canceled while waiting for connection",
];

/// What Docker says of a pull the registry refused, as [`said_any`] reads it (lowercase).
const PULL_REFUSED: &[&str] = &[
    "pull access denied",
    "manifest unknown",
    "toomanyrequests",
    "no matching manifest",
    "unauthorized: authentication required",
    "failed to resolve reference",
];

impl Failure {
    /// What a failed call that printed `out` failed with.
    pub(crate) fn of(out: &Captured) -> Failure {
        if engine_denied(out) {
            Failure::EngineDenied(out.reason())
        } else if said_any(out, ENGINE_DOWN) {
            Failure::EngineDown(out.reason())
        } else if said_any(out, &["pool overlaps with other one on this address space"]) {
            Failure::PoolOverlap(out.reason())
        } else if said_any(out, &["is already in use by container"]) {
            Failure::NameInUse
        } else if said_any(out, &["has incorrect label", "network with name"]) {
            Failure::NetworkExists
        } else if said_any(out, &["port is already allocated", "address already in use"]) {
            Failure::PortTaken(taken_port(out))
        } else if said_any(out, PULL_REFUSED) {
            Failure::PullRefused(out.reason())
        } else if said_any(out, PULL_TRANSIENT) {
            Failure::PullTransient(out.reason())
        } else {
            Failure::Other(out.reason())
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::NameInUse => f.write_str("a container name is already in use"),
            Failure::NetworkExists => f.write_str("a network of the project's name is already there"),
            Failure::PortTaken(_) => f.write_str("a host port it publishes is taken"),
            Failure::PullRefused(reason) => write!(f, "an image's pull was refused: {reason}"),
            Failure::PullTransient(reason) => write!(f, "an image's pull was cut short: {reason}"),
            Failure::EngineDown(reason) | Failure::EngineDenied(reason) | Failure::PoolOverlap(reason) | Failure::Other(reason) => f.write_str(reason),
        }
    }
}

/// The project a call is on: its name, its Compose files and its profiles.
pub(crate) struct Project<'a> {
    /// `-p`.
    pub(crate) name: &'a str,
    /// `-f`, in order.
    pub(crate) files: &'a [PathBuf],
    /// The run's own override, after the project's files (its pinned host ports), when it has
    /// one.
    pub(crate) pins: Option<PathBuf>,
    /// `--profile`.
    pub(crate) profiles: &'a [String],
}

impl Project<'_> {
    /// `compose -p <name> -f <file>… --profile <profile>…` then `rest`.
    fn args(&self, rest: impl IntoIterator<Item = String>) -> Vec<String> {
        let mut args = argv(["compose", "-p", self.name]);
        for file in self.files.iter().chain(&self.pins) {
            args.extend(["-f".to_owned(), file.to_string_lossy().into_owned()]);
        }
        for profile in self.profiles {
            args.extend(["--profile".to_owned(), profile.clone()]);
        }
        args.extend(rest);
        args
    }
}

/// How an `up` runs.
#[derive(Debug, Clone, Default)]
pub(crate) struct Up<'a> {
    /// The services to start (all when empty).
    pub(crate) services: &'a [String],
    /// `--wait`: until each service is running and healthy.
    pub(crate) wait: bool,
    /// `--remove-orphans`.
    pub(crate) remove_orphans: bool,
    /// What becomes of the containers that exist, and of the images they run.
    pub(crate) existing: Existing,
}

/// What an `up` does with what an earlier one made.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Existing {
    /// Compose's own: a container whose configuration changed is made again.
    #[default]
    Remade,
    /// `--build`: as [`Existing::Remade`], each service that has a `build` built first, so an
    /// image made from an earlier copy of its files (kept under the same name) never runs in
    /// its place. Compose's build cache makes it quick when nothing changed.
    Rebuilt,
    /// `--no-recreate`: what exists is started as it is (a resume), never made again.
    Kept,
}

/// `up --detach`.
pub(crate) async fn up(call: &Call<'_>, project: &Project<'_>, how: &Up<'_>) -> Result<(), CliError<Failure>> {
    let mut rest = argv(["up", "--detach"]);
    if how.wait {
        rest.push("--wait".into());
    }
    if how.remove_orphans {
        rest.push("--remove-orphans".into());
    }
    match how.existing {
        Existing::Remade => {}
        Existing::Rebuilt => rest.push("--build".into()),
        Existing::Kept => rest.push("--no-recreate".into()),
    }
    rest.extend(how.services.iter().cloned());
    call.ok(TOOL, project.args(rest), Failure::of).await.map(drop)
}

/// `pull --ignore-buildable`: the images the project's started services run (a built one is
/// built by its `up`), ended as stalled after `stall` with no line printed.
pub(crate) async fn pull(call: &Call<'_>, project: &Project<'_>, stall: std::time::Duration) -> Result<(), CliError<Failure>> {
    let args = project.args(argv(["pull", "--ignore-buildable"]));
    call.ok_watched(TOOL, args, stall, Failure::of).await.map(drop)
}

/// `down`, with the project's volumes (`-v`) and orphans when asked.
pub(crate) async fn down(call: &Call<'_>, project: &Project<'_>, volumes: bool, remove_orphans: bool) -> Result<(), CliError<Failure>> {
    let mut rest = argv(["down"]);
    if volumes {
        rest.push("--volumes".into());
    }
    if remove_orphans {
        rest.push("--remove-orphans".into());
    }
    call.ok(TOOL, project.args(rest), Failure::of).await.map(drop)
}

/// A lifecycle verb with no options of its own on `services` (all when empty): `stop`, `pause`,
/// `unpause`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    /// `stop`.
    Stop,
    /// `pause`.
    Pause,
    /// `unpause`.
    Unpause,
}

/// `verb` on `services`.
pub(crate) async fn act(call: &Call<'_>, project: &Project<'_>, verb: Verb, services: &[String]) -> Result<(), CliError<Failure>> {
    let word = match verb {
        Verb::Stop => "stop",
        Verb::Pause => "pause",
        Verb::Unpause => "unpause",
    };
    let rest = std::iter::once(word.to_owned()).chain(services.iter().cloned());
    call.ok(TOOL, project.args(rest), Failure::of).await.map(drop)
}

/// `ps --all --format json`: every container of the project, a one-off's included; a listing it
/// can't read whole is [`CliError::Unexpected`], never fewer containers.
pub(crate) async fn ps(call: &Call<'_>, project: &Project<'_>) -> Result<Vec<Container>, CliError<Failure>> {
    let out = call.ok(TOOL, project.args(argv(["ps", "--all", "--format", "json"])), Failure::of).await?;
    ps::parse(&out.stdout.join("\n")).ok_or_else(|| CliError::Unexpected {
        tool: TOOL,
        what: "a `compose ps` listing that isn't JSON containers".into(),
    })
}

/// `config --services`: the services the project's files declare, under its profiles (a service
/// under another profile only when its profile is given), in the files' order.
pub(crate) async fn services(call: &Call<'_>, project: &Project<'_>) -> Result<Vec<String>, CliError<Failure>> {
    let out = call.ok(TOOL, project.args(argv(["config", "--services"])), Failure::of).await?;
    Ok(out.stdout.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).map(str::to_owned).collect())
}

/// Whether the engine answers this account now: `docker version`'s server part. Asked after a
/// call failed in words that leave the engine's state open, since a stopped engine is said
/// differently by each Docker (`Cannot connect to the Docker daemon` up to 28, `failed to connect
/// to the docker API` since 29) and at each moment of its quitting (Docker Desktop's socket
/// answering `502 Bad Gateway` for the engine's API while it shuts down).
pub(crate) async fn engine_answers(call: &Call<'_>) -> bool {
    call.ok(TOOL, argv(["version", "--format", "{{.Server.Version}}"]), Failure::of).await.is_ok()
}

/// The oldest Compose the adapter drives (`ps --format json` one object per line from 2.21 is
/// read too, `--wait` and profiles need 2.17).
pub(crate) const MIN_VERSION: &str = "2.17.0";

/// `compose version --short`, checked against [`MIN_VERSION`].
pub(crate) async fn version(call: &Call<'_>) -> Result<Option<crate::project::platform::Version>, CliError<Failure>> {
    let out = call.ok(TOOL, argv(["compose", "version", "--short"]), Failure::of).await?;
    super::version(TOOL, &out.stdout.join("\n"), Some(MIN_VERSION))
}

/// What an `exec` runs in a service's container.
pub(crate) struct Exec<'a> {
    /// The service.
    pub(crate) service: &'a str,
    /// `--user`, when given.
    pub(crate) user: Option<&'a str>,
    /// The command and its arguments.
    pub(crate) argv: &'a [String],
    /// A terminal (`-T` when not).
    pub(crate) tty: bool,
}

/// `exec` arguments for `what`.
fn exec_args(project: &Project<'_>, what: &Exec<'_>) -> Vec<String> {
    let mut rest = argv(["exec"]);
    if !what.tty {
        rest.push("-T".into());
    }
    if let Some(user) = what.user {
        rest.extend(["--user".to_owned(), user.to_owned()]);
    }
    rest.push(what.service.to_owned());
    rest.extend(what.argv.iter().cloned());
    project.args(rest)
}

/// The console that `exec`s `what`, for the embedder to run: nothing is started.
pub(crate) fn exec_spec(call: &Call<'_>, project: &Project<'_>, what: &Exec<'_>) -> crate::process::ProcessSpec {
    call.spec(TOOL, exec_args(project, what))
}

/// `exec` of `what`, its output and exit status captured, whatever the status: a command that
/// failed is its result, not the adapter's failure.
pub(crate) async fn exec(call: &Call<'_>, project: &Project<'_>, what: &Exec<'_>) -> Result<Captured, CliError<Failure>> {
    call.run(TOOL, exec_args(project, what)).await
}

/// `run --rm` of `service` with `argv` (a one-off, a check runner), its output and exit status
/// captured as [`exec`]'s; the project's profiles are the call's.
pub(crate) async fn run_rm(call: &Call<'_>, project: &Project<'_>, service: &str, argv_: &[String]) -> Result<Captured, CliError<Failure>> {
    let rest = argv(["run", "--rm", "-T", service]).into_iter().chain(argv_.iter().cloned());
    call.run(TOOL, project.args(rest)).await
}

/// `rm --stop --force --volumes` of `services`: their containers and anonymous volumes gone.
pub(crate) async fn rm_stop_force(call: &Call<'_>, project: &Project<'_>, services: &[String]) -> Result<(), CliError<Failure>> {
    let rest = argv(["rm", "--stop", "--force", "--volumes"]).into_iter().chain(services.iter().cloned());
    call.ok(TOOL, project.args(rest), Failure::of).await.map(drop)
}

#[cfg(test)]
mod tests;
