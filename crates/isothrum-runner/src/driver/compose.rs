//! The `compose` driver: Docker Compose projects, named by the manifest's identity. Its five
//! actions run the native commands each one names; its status is the manifest's
//! `lifecycle.status` (`ps --all --format json`) matched to the manifest's machines.

mod actions;
mod config;
mod pins;
mod ps;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use isoloom_manifest::{Action, ComposeIdentity, Machine, Native, Port, Protocol, Step, access};

use super::{Driver, GuestUse, Observed};
use crate::Result;
use crate::console::ShellCommand;
use crate::error::Error;
use crate::executor::{Cx, Echo, call_ok};
use crate::process::ProcessSpec;
use crate::state::Op;
use crate::store::write_private;

pub(crate) use ps::UNREADABLE as UNREADABLE_PS;

/// The label Compose puts on each container naming its service, which finds a service's
/// container whatever project and file started it.
pub(crate) const COMPOSE_SERVICE_LABEL: &str = "com.docker.compose.service";

/// The label Compose puts on what a project creates.
const PROJECT_LABEL: &str = "com.docker.compose.project";

/// The label, with its value, Compose puts on a `docker compose run` container (a one-off).
const ONE_OFF_LABEL: &str = "com.docker.compose.oneoff=True";

/// Every machine of `manifest` as `ps --all --format json`'s output `out` shows it, and the host
/// port each published machine port got: a project on this computer's Docker, or one Compose runs
/// inside a VM (`docker-vm`'s guest). `None` when `out` lists no container this runner can read.
pub(crate) fn read_ps(manifest: &isoloom_manifest::Manifest, out: &str) -> Option<(Vec<crate::status::MachineStatus>, BTreeMap<crate::vars::PortKey, u16>)> {
    Some(ps::machines(manifest, &ps::parse_ps(out)?))
}

/// What an empty pinned-ports file holds: Compose reads it as no override.
const NO_PINS: &str = "services: {}\n";

/// How long one of an action's own Docker calls may go without progress (as every tool call's
/// limit counts).
const CALL_LIMIT: Duration = Duration::from_secs(120);

/// How long hot-adding the access machine may go without progress: its image may still need
/// pulling.
const ATTACH_LIMIT: Duration = Duration::from_secs(900);

/// The `compose` driver of one manifest.
#[derive(Debug, Clone)]
pub(crate) struct Compose {
    identity: ComposeIdentity,
}

impl Compose {
    pub(crate) fn new(identity: ComposeIdentity) -> Compose {
        Compose { identity }
    }

    /// The Compose project (`${run.name}`, filled).
    fn project(&self, cx: &Cx<'_>) -> Result<String> {
        cx.vars.render(&self.identity.project, "identity.compose.project")
    }

    /// The pinned-ports file, absolute, when the identity names one.
    fn pins(&self, cx: &Cx<'_>) -> Result<Option<PathBuf>> {
        let Some(file) = &self.identity.pinned_ports_file else {
            return Ok(None);
        };
        let path = cx.vars.render(file.template(), "identity.compose.pinned_ports_file")?;
        Ok(Some(cx.run.dir().join(path)))
    }

    /// `docker compose --project-name P --file F… <rest>`'s arguments.
    fn compose_args(&self, cx: &Cx<'_>, rest: &[&str]) -> Result<Vec<String>> {
        let mut args = vec!["compose".to_owned(), "--project-name".into(), self.project(cx)?];
        for file in &self.identity.files {
            args.push("--file".into());
            args.push(cx.vars.render(file.template(), "identity.compose.files")?);
        }
        args.extend(rest.iter().map(|s| (*s).to_owned()));
        Ok(args)
    }

    /// Runs `docker compose … <rest>` for `step` with `env`, returning its standard output.
    async fn compose(&self, cx: &Cx<'_>, step: &Step, rest: &[&str], env: &[(String, String)]) -> Result<String> {
        let args = self.compose_args(cx, rest)?;
        docker(cx, step, args, env).await
    }
}

/// Runs `docker <args>` in the manifest's folder for `step`, returning its standard output.
pub(super) async fn docker(cx: &Cx<'_>, step: &Step, args: Vec<String>, env: &[(String, String)]) -> Result<String> {
    let spec = docker_spec(cx, args, env.to_vec());
    call_ok(cx, step.id.as_str(), &spec, CALL_LIMIT, Echo::Quiet).await
}

/// A port's protocol as Compose writes it: `tcp` (also when it says none) or `udp`; `None` for
/// another (`sctp`), which no port of a manifest is.
fn protocol(said: &str) -> Option<Protocol> {
    match said {
        "tcp" | "" => Some(Protocol::Tcp),
        "udp" => Some(Protocol::Udp),
        _ => None,
    }
}

/// Machine `m`'s port that Compose publishes as container port `port` over `protocol`.
fn machine_port(m: &Machine, port: u16, protocol: Protocol) -> Option<&Port> {
    m.ports.iter().find(|q| q.port.get() == port && q.kind.transport() == protocol)
}

impl Driver for Compose {
    /// A Compose run has nothing to check before it starts (its tools are probed).
    fn preflight(&self, _cx: &Cx<'_>) -> impl Future<Output = Result<()>> {
        std::future::ready(Ok(()))
    }

    fn env(&self, _cx: &Cx<'_>) -> Result<Vec<(String, String)>> {
        Ok(Vec::new())
    }

    fn prepare(&self, cx: &Cx<'_>) -> Result<()> {
        // Every Compose call names the pinned-ports file: it must exist before the first.
        if let Some(pins) = self.pins(cx)?
            && !pins.exists()
        {
            write_private(&pins, NO_PINS.as_bytes()).map_err(Error::io(&pins, "written"))?;
        }
        Ok(())
    }

    async fn action(&self, cx: &Cx<'_>, list: &str, step: &Step, action: Action) -> Result<()> {
        match action {
            Action::ComposePinPorts => actions::pin_ports(self, cx, list, step).await,
            Action::ComposeCheckPortsFree => actions::check_ports_free(self, cx, list, step).await,
            Action::ComposeRemoveStaleNetworks => actions::remove_stale_networks(self, cx, list, step).await,
            Action::ComposeRemoveContainers => actions::remove_containers(self, cx, list, step).await,
            Action::ComposeRemoveNetworks => actions::remove_networks(self, cx, step).await,
            other => Err(super::foreign_action("Compose", step, other)),
        }
    }

    async fn observe(&self, cx: &Cx<'_>) -> Result<Observed> {
        let out = super::status_report(cx).await?;
        let (machines, published) = read_ps(cx.run.manifest(), &out).ok_or_else(|| ps::unreadable(super::STATUS))?;
        // Compose publishes on this computer, at the address the manifest binds.
        let publishing = cx
            .run
            .manifest()
            .machines
            .iter()
            .filter(|m| published.keys().any(|(name, _)| m.name.as_str() == name));
        let mut hosts = BTreeMap::new();
        for m in publishing {
            hosts.insert(m.name.to_string(), super::publish_host(cx, m)?);
        }
        Ok(Observed {
            machines,
            published,
            hosts,
            ..Observed::default()
        })
    }

    fn after_down(&self, cx: &Cx<'_>) -> Result<()> {
        // The next start is a new run: it picks its ports again.
        if let Some(pins) = self.pins(cx)? {
            write_private(&pins, NO_PINS.as_bytes()).map_err(Error::io(&pins, "written"))?;
        }
        Ok(())
    }

    /// Nothing: a service carries no boot digest; `up`'s steps recreate a changed one.
    fn restart(&self, _cx: &Cx<'_>, _machine: &Machine, _id: &str) -> impl Future<Output = Result<()>> {
        std::future::ready(Ok(()))
    }

    /// Hot-adds the access service through its Compose `access` profile
    /// ([`Compose::attach_profile`]).
    async fn attach(&self, cx: &Cx<'_>, machine: &Machine, image: Option<&str>, network: Option<&str>) -> Result<()> {
        self.attach_profile(cx, service_of(machine, "hot-adding")?, image, network).await
    }

    /// Stops and removes the machine's service with the services in its namespace
    /// ([`Compose::remove`]), through its profile (the `access` one for an on-demand machine, the
    /// default one otherwise). The update after a retire can't be relied on for it: whether its
    /// start removes a service gone from the Compose files is the manifest's (`--remove-orphans`),
    /// and it touches no service outside the default start, so one that only leaves the default
    /// start (back to an on-demand machine in the `access` profile) would keep running.
    async fn retire(&self, cx: &Cx<'_>, machine: &Machine) -> Result<()> {
        let profile = machine.on_demand.then_some(access::PROFILE);
        self.remove(cx, cx.events.call_id(), profile, service_of(machine, "taking out")?).await
    }

    /// `docker compose exec` into the machine's service, a shell there; as `user`, `exec --user`
    /// and a login shell in its home.
    fn console_command(&self, cx: &Cx<'_>, machine: &Machine, user: Option<&str>) -> Result<ShellCommand> {
        let service = service_of(machine, "a console into")?;
        let args = match user {
            None => self.compose_args(cx, &["exec", service, "sh", "-c", super::shells::CONTAINER_SHELL])?,
            Some(user) => self.compose_args(cx, &["exec", "--user", user, service, "sh", "-c", &super::shells::container_shell_as()])?,
        };
        Ok(super::console_in_run(cx, ProcessSpec::new("docker", args)))
    }

    /// Refused: a Compose run's containers run on this computer, in no VM.
    fn in_guest(&self, _cx: &Cx<'_>, vm: &Machine, _script: &str, _use: GuestUse) -> Result<ProcessSpec> {
        Err(Error::Unsupported {
            what: format!("running a script in `{}`: a Compose run has no VM its containers run in", vm.name),
        })
    }
}

/// The Compose service `machine` is, for `what` (refused when it isn't one).
fn service_of<'m>(machine: &'m Machine, what: &str) -> Result<&'m str> {
    match &machine.native {
        Some(Native::Compose { service }) => Ok(service.as_str()),
        _ => Err(Error::Unsupported {
            what: format!("{what} `{}`, which isn't a Compose service", machine.name),
        }),
    }
}

impl Compose {
    /// Brings the manifest's access service up in its profile, so the initial `up` left it down,
    /// with the services in its namespace (its routes through the run's router):
    /// `docker compose --profile access up -d --wait --no-deps <service> <its sidecars>`, with the
    /// picked image in [`access::IMAGE_ENV`] (the service's `${VAR:-default}` keeps its default
    /// otherwise). When a network is named (its native name), the started container is also
    /// plugged into it.
    async fn attach_profile(&self, cx: &Cx<'_>, native: &str, image: Option<&str>, network: Option<&str>) -> Result<()> {
        let mut env = Vec::new();
        if let Some(image) = image {
            env.push((access::IMAGE_ENV.to_owned(), image.to_owned()));
        }
        let services = self.with_namespace(cx, Op::Attach.as_str(), Some(access::PROFILE), native).await?;
        let mut rest = vec!["--profile", access::PROFILE, "up", "-d", "--wait", "--no-deps"];
        rest.extend(services.iter().map(String::as_str));
        let args = self.compose_args(cx, &rest)?;
        // Its image may still need pulling, so this gets the long boot budget, not a call's.
        call_ok(cx, Op::Attach.as_str(), &docker_spec(cx, args, env), ATTACH_LIMIT, Echo::Shown).await?;
        if let Some(network) = network {
            self.connect_network(cx, native, network).await?;
        }
        Ok(())
    }

    /// Stops and removes `native` with the services in its namespace, through `profile`, its
    /// calls reported under `id`:
    /// `docker compose [--profile <profile>] rm -sf <service> <its sidecars>`.
    async fn remove(&self, cx: &Cx<'_>, id: &str, profile: Option<&str>, native: &str) -> Result<()> {
        let services = self.with_namespace(cx, id, profile, native).await?;
        let mut rest = profiled(profile);
        rest.extend(["rm", "-sf"]);
        rest.extend(services.iter().map(String::as_str));
        let args = self.compose_args(cx, &rest)?;
        call_ok(cx, id, &docker_spec(cx, args, Vec::new()), CALL_LIMIT, Echo::Shown).await?;
        Ok(())
    }

    /// `native` and the services of `profile` (the default one: none named) in its network
    /// namespace ([`config::sharing_namespace`]): one machine, started and removed together.
    /// Refused when Compose prints a config it can't be read from: the machine would come up
    /// without its routes, or leave them running.
    async fn with_namespace(&self, cx: &Cx<'_>, id: &str, profile: Option<&str>, native: &str) -> Result<Vec<String>> {
        let mut rest = profiled(profile);
        rest.extend(["config", "--format", "json"]);
        let args = self.compose_args(cx, &rest)?;
        let config = call_ok(cx, id, &docker_spec(cx, args, Vec::new()), CALL_LIMIT, Echo::Quiet).await?;
        let sidecars = config::sharing_namespace(&config, native).map_err(|why| config::unreadable(id, "the services in the machine's namespace are", &why))?;
        let mut services = vec![native.to_owned()];
        services.extend(sidecars);
        Ok(services)
    }

    /// Plugs the just-started access machine's container into the network natively named
    /// `network`. A container already on it is where it should be; any other failure fails the
    /// attach.
    async fn connect_network(&self, cx: &Cx<'_>, native: &str, network: &str) -> Result<()> {
        let step = Op::Attach.as_str();
        let id_args = self.compose_args(cx, &["ps", "-q", native])?;
        let out = call_ok(cx, step, &docker_spec(cx, id_args, Vec::new()), CALL_LIMIT, Echo::Quiet).await?;
        let container = out.lines().map(str::trim).find(|s| !s.is_empty()).ok_or_else(|| Error::ToolFailed {
            step: step.to_owned(),
            reason: format!("`{native}` has no container to join to network `{network}`"),
        })?;
        let connect = ["network", "connect", network, container].map(String::from).to_vec();
        match call_ok(cx, step, &docker_spec(cx, connect, Vec::new()), CALL_LIMIT, Echo::Quiet).await {
            Err(Error::ToolFailed { reason, .. }) if reason.contains(ALREADY_CONNECTED) => Ok(()),
            done => done.map(drop),
        }
    }
}

/// What Docker says when a container is already on the network it is connected to.
const ALREADY_CONNECTED: &str = "already exists in network";

/// `--profile <profile>`, when one is named.
fn profiled(profile: Option<&str>) -> Vec<&str> {
    profile.map(|p| vec!["--profile", p]).unwrap_or_default()
}

/// A `docker <args>` spec in the run's folder, with `env`.
fn docker_spec(cx: &Cx<'_>, args: Vec<String>, env: Vec<(String, String)>) -> ProcessSpec {
    ProcessSpec {
        program: "docker".into(),
        args,
        cwd: Some(cx.run.dir().to_path_buf()),
        env,
        stdin: None,
    }
}
