//! The `compose` driver: Docker Compose projects, named by the manifest's identity. Its five
//! actions run the native commands run-manifest-v0.md §6 fixes; its status is the manifest's
//! `lifecycle.status` (`ps --all --format json`) matched to the manifest's machines.

mod actions;
mod adhoc;
mod config;
mod pins;
mod ps;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use isoloom_manifest::{Action, ComposeIdentity, SnakeName, Step, Template, access};

use super::{Attached, Attacker, Driver, Observed};
use crate::Result;
use crate::error::Error;
use crate::executor::{Cx, Echo, call_ok};
use crate::process::ProcessSpec;
use crate::state::Op;
use crate::status::ShellCommand;
use crate::store::write_private;

/// Every machine of `manifest` as `ps --all --format json`'s output `out` shows it, and the host
/// port each published machine port got: a project on this computer's Docker, or one Compose runs
/// inside a VM (`docker-vm`'s guest).
pub(crate) fn read_ps(manifest: &isoloom_manifest::Manifest, out: &str) -> (Vec<crate::status::MachineStatus>, BTreeMap<crate::vars::PortKey, u16>) {
    ps::machines(manifest, &ps::parse_ps(out))
}

/// What an empty pinned-ports file holds: Compose reads it as no override.
const NO_PINS: &str = "services: {}\n";

/// How long one of an action's own Docker calls may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// How long hot-adding the access box may take: its image may still need pulling.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(900);

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
    call_ok(cx, step.id.as_str(), &spec, CALL_TIMEOUT, Echo::Quiet).await
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

    async fn action(&self, cx: &Cx<'_>, step: &Step, action: Action, with: &BTreeMap<SnakeName, Template>) -> Result<()> {
        match action {
            Action::ComposePinPorts => actions::pin_ports(self, cx, step).await,
            Action::ComposeCheckPortsFree => actions::check_ports_free(self, cx, step).await,
            Action::ComposeRemoveStaleNetworks => actions::remove_stale_networks(self, cx, step).await,
            Action::ComposeRemoveContainers => actions::remove_containers(self, cx, step, with).await,
            Action::ComposeRemoveNetworks => actions::remove_networks(self, cx, step).await,
            other => Err(super::foreign_action("Compose", step, other)),
        }
    }

    async fn observe(&self, cx: &Cx<'_>) -> Result<Observed> {
        let out = super::status_report(cx).await?;
        let (mut machines, published) = ps::machines(cx.run.manifest(), &ps::parse_ps(&out));
        // The ad-hoc attacker box has no Compose service, so `ps` never lists it. When the lab
        // declares no access machine of its own, probe for the box the runner hot-added, so a run
        // that has one shows it like any other machine (its state, its address, the shell into it);
        // a lab with its own access machine already has it from `ps`.
        if super::takes_adhoc_box(cx.run.manifest())
            && let Some(access) = self.adhoc_status(cx).await
        {
            machines.push(access);
        }
        // Compose publishes on this computer.
        let hosts = published.keys().map(|(machine, _)| (machine.clone(), "127.0.0.1".to_owned())).collect();
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

    /// Hot-adds the attacker box. A manifest machine goes up through its Compose `access` profile
    /// (the backward-compatible path, [`Compose::attach_profile`]); an ad-hoc box with no machine
    /// is stood up on the lab's network with no Compose service ([`Compose::attach_adhoc`]).
    async fn attach(&self, cx: &Cx<'_>, attacker: Attacker<'_>, image: Option<&str>, network: Option<&str>) -> Result<Attached> {
        match attacker {
            Attacker::Machine(native) => {
                self.attach_profile(cx, native, image, network).await?;
                Ok(Attached::default())
            }
            Attacker::AdHoc => self.attach_adhoc(cx, image, network).await,
        }
    }

    /// Hot-removes the attacker box: a manifest machine through its profile
    /// (`docker compose --profile access rm -sf <service>`), an ad-hoc box by `docker rm -f` of
    /// its container (best effort, idempotent: a box already gone is no error).
    async fn detach(&self, cx: &Cx<'_>, attacker: Attacker<'_>) -> Result<()> {
        match attacker {
            Attacker::Machine(native) => {
                let args = self.compose_args(cx, &["--profile", access::PROFILE, "rm", "-sf", native])?;
                call_ok(cx, Op::Detach.as_str(), &docker_spec(cx, args, Vec::new()), CALL_TIMEOUT, Echo::Shown).await?;
                Ok(())
            }
            Attacker::AdHoc => {
                let name = adhoc::container_name(&self.project(cx)?);
                drop(self.remove_container(cx, &name).await);
                Ok(())
            }
        }
    }

    /// The command that opens an interactive shell into the attacker box. A manifest machine is
    /// addressed by its Compose service (`docker compose … exec <service> bash`); an ad-hoc box by
    /// its container (`docker exec -it <name> …`, falling back from `bash` to `sh`).
    /// `docker compose exec` into the machine's service, a shell there.
    fn console_command(&self, cx: &Cx<'_>, machine: &isoloom_manifest::Machine) -> Result<ShellCommand> {
        let Some(isoloom_manifest::Native::Compose { service }) = &machine.native else {
            return Err(Error::Unsupported {
                what: format!("a console into `{}`, which isn't a Compose service", machine.name),
            });
        };
        Ok(ShellCommand {
            program: "docker".into(),
            args: self.compose_args(cx, &["exec", service.as_str(), "sh", "-c", super::console::CONTAINER_SHELL])?,
            cwd: Some(cx.run.dir().to_path_buf()),
            env: Vec::new(),
        })
    }

    fn shell_command(&self, cx: &Cx<'_>, attacker: Attacker<'_>) -> Result<ShellCommand> {
        let args = match attacker {
            Attacker::Machine(native) => self.compose_args(cx, &["exec", native, "bash"])?,
            Attacker::AdHoc => adhoc::shell_args(&adhoc::container_name(&self.project(cx)?)),
        };
        Ok(ShellCommand {
            program: "docker".into(),
            args,
            cwd: Some(cx.run.dir().to_path_buf()),
            env: Vec::new(),
        })
    }
}

impl Compose {
    /// Brings the manifest's access service up in its profile, so the initial `up` left it down:
    /// `docker compose --profile access up -d --no-deps <service>`, with the picked image in
    /// [`access::IMAGE_ENV`] (the service's `${VAR:-default}` keeps its default otherwise). When a
    /// network is named, the started container is also plugged into it, best effort.
    async fn attach_profile(&self, cx: &Cx<'_>, native: &str, image: Option<&str>, network: Option<&str>) -> Result<()> {
        let mut env = Vec::new();
        if let Some(image) = image {
            env.push((access::IMAGE_ENV.to_owned(), image.to_owned()));
        }
        let args = self.compose_args(cx, &["--profile", access::PROFILE, "up", "-d", "--no-deps", native])?;
        // The box's image may still need pulling, so this gets the long boot budget, not a call's.
        call_ok(cx, Op::Attach.as_str(), &docker_spec(cx, args, env), ATTACH_TIMEOUT, Echo::Shown).await?;
        if let Some(network) = network {
            self.connect_network(cx, native, network).await;
        }
        Ok(())
    }

    /// Plugs the just-started access container into the lab network `network` (its project
    /// network), best effort: a box already on it, or a network it can't name, is left as it is.
    async fn connect_network(&self, cx: &Cx<'_>, native: &str, network: &str) {
        let Ok(project) = self.project(cx) else { return };
        let Ok(id_args) = self.compose_args(cx, &["ps", "-q", native]) else { return };
        let Ok(out) = call_ok(cx, Op::Attach.as_str(), &docker_spec(cx, id_args, Vec::new()), CALL_TIMEOUT, Echo::Quiet).await else {
            return;
        };
        let Some(container) = out.lines().map(str::trim).find(|s| !s.is_empty()) else {
            return;
        };
        let net = format!("{project}_{network}");
        let connect = vec!["network".into(), "connect".into(), net, container.to_owned()];
        // Already-connected is a non-zero exit we deliberately ignore: the box is where we want it.
        if let Err(e) = call_ok(cx, Op::Attach.as_str(), &docker_spec(cx, connect, Vec::new()), CALL_TIMEOUT, Echo::Quiet).await {
            tracing::debug!(%e, "access box already on the network, or it can't be joined");
        }
    }
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
