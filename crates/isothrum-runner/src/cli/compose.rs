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

pub(crate) use config::{ConfigPort, pins_override, ports as config_ports};
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
        if said_any(out, &["is already in use by container"]) {
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
            Failure::Other(reason) => f.write_str(reason),
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
    /// `--no-recreate`: start what exists as it is (a resume), never make it again.
    pub(crate) no_recreate: bool,
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
    if how.no_recreate {
        rest.push("--no-recreate".into());
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

/// The oldest Compose the adapter drives (`ps --format json` one object per line from 2.21 is
/// read too, `--wait` and profiles need 2.17).
pub(crate) const MIN_VERSION: &str = "2.17.0";

/// `compose version --short`, checked against [`MIN_VERSION`].
pub(crate) async fn version(call: &Call<'_>) -> Result<crate::project::platform::Version, CliError<Failure>> {
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

/// `exec` of `what`, its output captured (a failed command is `Failed` with its reason).
pub(crate) async fn exec(call: &Call<'_>, project: &Project<'_>, what: &Exec<'_>) -> Result<Captured, CliError<Failure>> {
    call.ok(TOOL, exec_args(project, what), Failure::of).await
}

/// `run --rm` of `service` with `argv` (a one-off, a check runner), its output captured; the
/// project's profiles are the call's.
pub(crate) async fn run_rm(call: &Call<'_>, project: &Project<'_>, service: &str, argv_: &[String]) -> Result<Captured, CliError<Failure>> {
    let rest = argv(["run", "--rm", "-T", service]).into_iter().chain(argv_.iter().cloned());
    call.ok(TOOL, project.args(rest), Failure::of).await
}

/// `rm --stop --force --volumes` of `services`: their containers and anonymous volumes gone.
pub(crate) async fn rm_stop_force(call: &Call<'_>, project: &Project<'_>, services: &[String]) -> Result<(), CliError<Failure>> {
    let rest = argv(["rm", "--stop", "--force", "--volumes"]).into_iter().chain(services.iter().cloned());
    call.ok(TOOL, project.args(rest), Failure::of).await.map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::testing;
    use crate::process::Exit;
    use crate::process::fake::{FakeProcesses, Reply};

    fn failed(stderr: &str) -> Failure {
        Failure::of(&Captured::new(
            TOOL,
            Exit::new(Some(1)),
            Vec::new(),
            stderr.lines().map(str::to_owned).collect(),
        ))
    }

    #[test]
    fn failures_read_as_compose_prints_them() {
        use crate::project::platform::Protocol::Tcp;
        assert_eq!(failed(include_str!("../tool_output/fixtures/compose/2.29-name-in-use.txt")), Failure::NameInUse);
        assert_eq!(
            failed(include_str!("../tool_output/fixtures/compose/2.29-network-incorrect-label.txt")),
            Failure::NetworkExists
        );
        for (said, port) in [
            (include_str!("../tool_output/fixtures/compose/2.29-port-allocated.txt"), 8080),
            (include_str!("../tool_output/fixtures/compose/2.29-port-allocated-v6.txt"), 8180),
            (include_str!("../tool_output/fixtures/compose/2.29-port-allocated-loopback.txt"), 8180),
            (include_str!("../tool_output/fixtures/compose/29.9-bind-host-port-in-use.txt"), 18180),
            (include_str!("../tool_output/fixtures/compose/29.9-bind-loopback-port-in-use.txt"), 18181),
            (include_str!("../tool_output/fixtures/compose/4.30-desktop-listen-in-use.txt"), 8180),
        ] {
            assert_eq!(failed(said), Failure::PortTaken(Some((port, Tcp))), "{said}");
        }
        assert_eq!(
            failed("Error: listen udp 0.0.0.0:5353: bind: address already in use"),
            Failure::PortTaken(Some((5353, crate::project::platform::Protocol::Udp)))
        );
        for refused in [
            include_str!("../tool_output/fixtures/compose/5.6-pull-access-denied.txt"),
            include_str!("../tool_output/fixtures/compose/5.6-tag-not-found.txt"),
            include_str!("../tool_output/fixtures/compose/2.29-pull-rate-limited.txt"),
        ] {
            assert!(matches!(failed(refused), Failure::PullRefused(_)), "{refused}");
        }
        assert!(matches!(failed("no such service: nope"), Failure::Other(r) if r.contains("no such service")));
    }

    #[tokio::test]
    async fn the_project_is_named_and_an_unreadable_listing_isnt_an_empty_one() {
        let fake = FakeProcesses::new(|c| {
            if c.line().contains(" ps ") {
                Reply::ok("{\"Service\": \"web\", \"State\"")
            } else {
                Reply::ok("")
            }
        });
        let call = testing::call(&fake);
        let files = [PathBuf::from("compose.yaml")];
        let project = Project {
            name: "lab",
            files: &files,
            pins: None,
            profiles: &["access".to_owned()],
        };
        up(&call, &project, &Up { wait: true, ..Up::default() }).await.unwrap();
        assert_eq!(fake.lines(), ["docker compose -p lab -f compose.yaml --profile access up --detach --wait"]);
        assert!(matches!(ps(&call, &project).await, Err(CliError::Unexpected { .. })));
        let console = exec_spec(
            &call,
            &project,
            &Exec {
                service: "web",
                user: Some("root"),
                argv: &["sh".to_owned()],
                tty: true,
            },
        );
        assert_eq!(console.line(), "docker compose -p lab -f compose.yaml --profile access exec --user root web sh");
    }

    #[tokio::test]
    async fn a_compose_older_than_2_17_is_too_old() {
        for (said, ok) in [("2.29.7\n", true), ("v2.17.0", true), ("2.16.9-desktop.1", false)] {
            let fake = FakeProcesses::rules(&[("docker compose version", Reply::ok(said))]);
            let read = version(&testing::call(&fake)).await;
            assert_eq!(read.is_ok(), ok, "{said}");
            if !ok {
                assert!(matches!(read, Err(CliError::TooOld { min: m, .. }) if m == MIN_VERSION));
            }
        }
        let empty = FakeProcesses::rules(&[("docker compose version", Reply::ok(""))]);
        assert!(
            matches!(version(&testing::call(&empty)).await, Err(CliError::Unexpected { .. })),
            "no version isn't any version"
        );
    }
}
