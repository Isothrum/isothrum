//! The Compose adapter: `docker compose` on one project, its failures read as [`Failure`] and its
//! `ps --all --format json` as the project's containers ([`Container`]).

use std::fmt;
use std::path::PathBuf;

use super::{Call, CliError, argv, said_any};
use crate::process::Captured;

/// The tool Compose runs as.
const TOOL: &str = "docker";

/// One container of the project, as `ps --all --format json` lists it (Compose 2.21 and later
/// print one object per line, older ones one array).
pub(crate) use crate::driver::compose::ps::PsEntry as Container;

/// A failed Compose call, as far as a driver acts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// A container name an earlier run's container holds (`is already in use by container`).
    NameInUse,
    /// A network of the project's name already there, or with other labels (`network with name
    /// … already exists`, `has incorrect label`).
    NetworkExists,
    /// A host port it publishes is taken (`port is already allocated`, `address already in use`).
    PortTaken,
    /// Any other failure: its reason.
    Other(String),
}

impl Failure {
    /// What a failed call that printed `out` failed with.
    pub(crate) fn of(out: &Captured) -> Failure {
        if said_any(out, &["is already in use by container"]) {
            Failure::NameInUse
        } else if said_any(out, &["has incorrect label", "network with name"]) {
            Failure::NetworkExists
        } else if said_any(out, &["port is already allocated", "address already in use"]) {
            Failure::PortTaken
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
            Failure::PortTaken => f.write_str("a host port it publishes is taken"),
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
    /// `--profile`.
    pub(crate) profiles: &'a [String],
}

impl Project<'_> {
    /// `compose -p <name> -f <file>… --profile <profile>…` then `rest`.
    fn args(&self, rest: impl IntoIterator<Item = String>) -> Vec<String> {
        let mut args = argv(["compose", "-p", self.name]);
        for file in self.files {
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
    rest.extend(how.services.iter().cloned());
    call.ok(TOOL, project.args(rest), Failure::of).await.map(drop)
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

/// A lifecycle verb with no options of its own on `services` (all when empty): `stop`, `start`,
/// `pause`, `unpause`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    /// `stop`.
    Stop,
    /// `start`.
    Start,
    /// `pause`.
    Pause,
    /// `unpause`.
    Unpause,
}

/// `verb` on `services`.
pub(crate) async fn act(call: &Call<'_>, project: &Project<'_>, verb: Verb, services: &[String]) -> Result<(), CliError<Failure>> {
    let word = match verb {
        Verb::Stop => "stop",
        Verb::Start => "start",
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
    crate::driver::compose::ps::parse_ps(&out.stdout.join("\n")).ok_or_else(|| CliError::Unexpected {
        tool: TOOL,
        what: "a `compose ps` listing that isn't JSON containers".into(),
    })
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
        assert_eq!(failed(include_str!("../tool_output/fixtures/compose/2.29-name-in-use.txt")), Failure::NameInUse);
        assert_eq!(
            failed(include_str!("../tool_output/fixtures/compose/2.29-network-incorrect-label.txt")),
            Failure::NetworkExists
        );
        assert_eq!(
            failed(include_str!("../tool_output/fixtures/compose/2.29-port-allocated.txt")),
            Failure::PortTaken
        );
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
            profiles: &["access".to_owned()],
        };
        up(&call, &project, &Up { wait: true, ..Up::default() }).await.unwrap();
        assert_eq!(fake.lines(), ["docker compose -p lab -f compose.yaml --profile access up --detach --wait"]);
        assert!(matches!(ps(&call, &project).await, Err(CliError::Unexpected { .. })));
    }
}
