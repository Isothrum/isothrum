//! The Compose driver: a Compose project's lifecycle, through `docker compose` on the project named
//! after the run (`-p <run>`, so two copies of one project never share a container, network or
//! volume) and `docker` for the objects Compose's own label marks as the project's.
//!
//! | Operation | Compose |
//! |---|---|
//! | up, update | `pull --ignore-buildable` ([`pull`]: stopped as stalled, tried again when cut short), then `up --detach --wait --remove-orphans`: ready once every service runs healthy and every one-shot service it waits on exited 0; an update then removes (`rm --stop --force --volumes`) the containers of services the files declare but no longer start (moved under a profile) and the run hasn't attached |
//! | down | `down --volumes --remove-orphans`, then whatever still carries the project's label, by id |
//! | park | `pause` (Pause), `stop` (Shutdown) |
//! | resume | `unpause` of the paused services, `stop` of those left in the network namespace of one that stopped, then `up --detach --wait --no-recreate` |
//! | attach, retire | `up --detach --wait <service>` (a service under a profile starts when named), `rm --stop --force --volumes <service>` |
//! | exec, console | `exec -T`, or `run --rm` for a service with no container (a profile's one-off); a console is `exec` with a terminal |
//! | observe | `ps --all --format json` |
//! | found | the containers, networks and volumes labelled `com.docker.compose.project=<run>` |
//! | usage | `ps`, then `docker stats --no-stream`: each running service's containers, summed |
//! | reapply | `up --detach --wait --remove-orphans` on the files: Compose makes or remakes only what changed |
//!
//! Recovery: an `up` refused because a name of the project is taken (a container or network an
//! earlier start left) removes the project's labelled leftovers by id and starts once more. A
//! host port taken is `port_in_use`. A first start claims the host ports the project fixes before
//! anything is made (one another run holds or something on this computer binds is refused there);
//! one refused by Docker anyway (taken since, or seen only by Docker) removes what the start made
//! (`down`), so the next start, the port free, finds nothing in its way. Compose has no set-up to run again:
//! `provision` is refused.

mod failures;
mod fetched;
mod namespaces;
mod pins;
mod pull;

use std::time::Duration;

use self::failures::engine_error;
use super::{Change, FormatDriver, Found, Ran, Seen, SeenMachine, SeenPort, ToolCx, tool_error};
use crate::Result;
use crate::cli::CliError;
use crate::cli::compose::{self, Exec, Failure, Project, Up, Verb};
use crate::cli::docker::{self, Object};
use crate::console::ShellCommand;
use crate::console::shells;
use crate::error::Error;
use crate::state::{Park, RunState};
use crate::status::MachineState;
use crate::store::{Lifetime, ResourceKind};

/// The label Compose puts on every object of a project, with the project's name.
pub(crate) const PROJECT_LABEL: &str = "com.docker.compose.project";

/// How long a start may take: images pulled, built and every service healthy.
const UP_LIMIT: Duration = Duration::from_secs(1800);

/// How long a lifecycle call that makes nothing may take (stop, start, down, rm).
const CALL_LIMIT: Duration = Duration::from_secs(600);

/// How long a read may take (`ps`, `config`, a version, a label listing).
const READ_LIMIT: Duration = Duration::from_secs(30);

/// The Compose driver.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Compose;

impl Compose {
    /// The project a call is on: the run's name and the project's own file.
    fn project<'a>(cx: &'a ToolCx<'_>) -> Project<'a> {
        Project {
            name: cx.run.as_str(),
            files: &cx.project.files,
            pins: pins::file(cx),
            profiles: &[],
        }
    }

    /// The `label=` filter that finds the project's objects.
    fn label(cx: &ToolCx<'_>) -> String {
        format!("{PROJECT_LABEL}={}", cx.run.as_str())
    }

    /// [`Compose::prepare`], then [`Compose::launch`]: an update, a reapply.
    async fn start(&self, cx: &ToolCx<'_>) -> Result<()> {
        fetched::recording(cx, &[], async {
            self.prepare(cx, false).await?;
            self.launch(cx).await
        })
        .await
    }

    /// What a start does before it makes anything: the project's ports pinned (its fixed host
    /// ports claimed first, at a `first` start), its images pulled.
    async fn prepare(&self, cx: &ToolCx<'_>, first: bool) -> Result<()> {
        pins::pin(cx, first).await?;
        self.pull(cx).await
    }

    /// `up`, which makes and starts the project's containers.
    async fn launch(&self, cx: &ToolCx<'_>) -> Result<()> {
        let how = Up {
            wait: true,
            remove_orphans: true,
            ..Up::default()
        };
        self.up_recovering(cx, "up", &how).await
    }

    /// `up` as `how`, with one recovery: a name the project's leftovers hold is cleared by id, then
    /// the start is made once more. A taken host port is `port_in_use`.
    async fn up_recovering(&self, cx: &ToolCx<'_>, step: &str, how: &Up<'_>) -> Result<()> {
        let call = cx.call(UP_LIMIT);
        let first = compose::up(&call, &Self::project(cx), how).await;
        let retry = match first {
            Ok(()) => return Ok(()),
            Err(CliError::Failed {
                failure: Failure::NameInUse | Failure::NetworkExists,
                ..
            }) => true,
            Err(e) => return Err(up_error(step, e)),
        };
        if retry {
            tracing::info!(run = %cx.run, "a name of the project is taken by what an earlier start left: removing it, then starting again");
            self.remove_labelled(cx).await?;
        }
        compose::up(&call, &Self::project(cx), how).await.map_err(|e| up_error(step, e))
    }

    /// Removes every object the project's label marks, containers first (a network or volume a
    /// container holds can't go before it).
    async fn remove_labelled(&self, cx: &ToolCx<'_>) -> Result<()> {
        let call = cx.call(CALL_LIMIT);
        for kind in [Object::Container, Object::Network, Object::Volume] {
            let ids = docker::labelled(&call, kind, &Self::label(cx)).await.map_err(|e| tool_error("leftovers", e))?;
            if !ids.is_empty() {
                docker::remove(&call, kind, &ids).await.map_err(|e| tool_error("leftovers", e))?;
            }
        }
        Ok(())
    }

    /// The services the project's files start at `up` (`config --services`: a service under a
    /// profile isn't one), and those of them with no container: gone (removed outside the run,
    /// or never made). Files Compose can't read leave none gone: the status stands on `ps`.
    async fn gone(&self, cx: &ToolCx<'_>, containers: &[compose::Container]) -> Vec<String> {
        let declared = match compose::services(&cx.read(READ_LIMIT), &Self::project(cx)).await {
            Ok(services) => services,
            Err(e) => {
                tracing::warn!(run = %cx.run, error = %e, "the project's services couldn't be read; the status is the containers'");
                return Vec::new();
            }
        };
        declared.into_iter().filter(|s| !containers.iter().any(|c| c.service() == s)).collect()
    }

    /// After an update: the containers of services the files still declare but no longer start
    /// (moved under a profile, as an access machine's route keeper is when the machine that stood
    /// in its place goes back to its own) and the run hasn't attached, removed. `--remove-orphans`
    /// leaves them, a declared service being no orphan: stale, a namespace they shared gone, they
    /// read as failed and the run never ready again.
    async fn retire_unstarted(&self, cx: &ToolCx<'_>) -> Result<()> {
        let started = compose::services(&cx.read(READ_LIMIT), &Self::project(cx))
            .await
            .map_err(|e| engine_error("services", e))?;
        let attached = pins::attached(cx)?;
        let mut stale: Vec<String> = Vec::new();
        for c in self.containers(cx).await? {
            let service = c.service();
            if !started.iter().any(|s| s == service) && !attached.iter().any(|s| s == service) && !stale.iter().any(|s| s == service) {
                stale.push(service.to_owned());
            }
        }
        if stale.is_empty() {
            return Ok(());
        }
        tracing::info!(run = %cx.run, services = ?stale, "removing the containers of services the files no longer start");
        compose::rm_stop_force(&cx.call(CALL_LIMIT), &Self::project(cx), &stale)
            .await
            .map_err(|e| engine_error("update", e))
    }

    /// Every container of the project, one-offs (`run`) left out.
    async fn containers(&self, cx: &ToolCx<'_>) -> Result<Vec<compose::Container>> {
        let all = compose::ps(&cx.read(READ_LIMIT), &Self::project(cx))
            .await
            .map_err(|e| engine_error("status", e))?;
        Ok(all.into_iter().filter(|c| !c.one_off()).collect())
    }
}

/// What each service's running `containers` use, from the engine's `stats`: a service's
/// containers summed, a figure none when the engine has it for none of them.
fn usage_by_service(containers: &[compose::Container], stats: &[docker::ContainerStats]) -> Vec<crate::MachineUsage> {
    let mut by_service: std::collections::BTreeMap<&str, (Option<f64>, Option<u64>)> = std::collections::BTreeMap::new();
    for c in containers {
        let seen = stats.iter().find(|s| s.is(c.id()));
        let (cpu, memory) = by_service.entry(c.service()).or_default();
        if let Some(p) = seen.and_then(|s| s.cpu_percent) {
            *cpu = Some(cpu.unwrap_or(0.0) + p);
        }
        if let Some(b) = seen.and_then(|s| s.memory_bytes) {
            *memory = Some(memory.unwrap_or(0).saturating_add(b));
        }
    }
    by_service
        .into_iter()
        .map(|(name, (cpu, memory))| crate::MachineUsage::new(name, cpu, memory))
        .collect()
}

/// An `up`'s failure as the runner says it: a taken host port is `port_in_use`, with the port and
/// protocol Docker names (0 and TCP when it names none), an image's pull the registry refused
/// `download_failed` (step `pull-<step>`), anything else as any tool's.
fn up_error(step: &str, e: CliError<Failure>) -> Error {
    match e {
        CliError::Failed {
            failure: Failure::PortTaken(taken),
            ..
        } => {
            let (port, protocol) = taken.unwrap_or((0, crate::project::platform::Protocol::Tcp));
            Error::PortInUse {
                port: crate::port::HostPort::new(port),
                protocol: crate::encoding::wire_name(&protocol),
                holder: None,
            }
        }
        CliError::Failed {
            failure: Failure::PullRefused(reason),
            ..
        } => Error::DownloadFailed {
            what: crate::error::DownloadKind::Images,
            step: format!("pull-{step}"),
            reason,
        },
        other => engine_error(step, other),
    }
}

/// The run's state as `machines` show it: absent when nothing is there; parked when nothing runs;
/// ready when every service that runs is up and healthy and every one that ended ended well (an
/// init job); failed otherwise (one unhealthy, one that died, one missing among running ones).
pub(super) fn run_state(machines: &[SeenMachine]) -> RunState {
    if machines.is_empty() || machines.iter().all(|m| m.state == MachineState::Absent) {
        return RunState::Absent;
    }
    if !machines.iter().any(|m| m.state.runs()) {
        return RunState::Parked;
    }
    let fine = |m: &SeenMachine| matches!(m.state, MachineState::Running | MachineState::Exited(0));
    if machines.iter().all(fine) { RunState::Ready } else { RunState::Failed }
}

/// The services a `retire` or `attach` names, refused when `machine` isn't one of the project's.
fn one_service(machine: &str) -> Vec<String> {
    vec![machine.to_owned()]
}

impl FormatDriver for Compose {
    async fn probe(&self, cx: &ToolCx<'_>) -> Result<()> {
        let found = compose::version(&cx.read(READ_LIMIT)).await.map_err(|e| engine_error("tools", e))?;
        super::unknown_version(cx, found.as_ref(), "docker compose", compose::MIN_VERSION);
        Ok(())
    }

    async fn up(&self, cx: &ToolCx<'_>) -> Result<()> {
        // A first start (nothing of the run recorded as made) refused for a taken host port is
        // undone: Compose makes the project's network and containers, and starts some, before
        // Docker refuses the bind; left there, they hold the run's names and namespaces, and the
        // next start, the port free, fails on them. A run with machines keeps them.
        // A first start claims the host ports the project fixes before anything is made: one
        // another run holds, or something on this computer binds, is refused with nothing to undo.
        let first = (cx.made)()?.is_empty();
        let started = fetched::recording(cx, &[], async {
            self.prepare(cx, first).await?;
            Ok::<_, crate::Error>(self.launch(cx).await)
        })
        .await?;
        if let Err(e) = &started
            && first
            && e.code() == crate::ErrorCode::PortInUse
        {
            tracing::info!(run = %cx.run, "the start was refused for a taken host port: removing what it made");
            if let Err(undo) = self.down(cx).await {
                let said = format!("what the refused start made wasn't all removed (the next start clears it): {undo}");
                (cx.warn)(crate::events::WarningCode::CleanupFailed, &said);
            }
        }
        started
    }

    async fn down(&self, cx: &ToolCx<'_>) -> Result<()> {
        compose::down(&cx.call(CALL_LIMIT), &Self::project(cx), true, true)
            .await
            .map_err(|e| engine_error("down", e))?;
        // What `down` doesn't take (a container a killed client left unnamed, a volume another
        // file's service made), by the project's label.
        self.remove_labelled(cx).await?;
        pins::forget(cx)
    }

    async fn park(&self, cx: &ToolCx<'_>, mode: Park) -> Result<()> {
        let verb = match mode {
            Park::Pause => Verb::Pause,
            Park::Shutdown => Verb::Stop,
        };
        compose::act(&cx.call(CALL_LIMIT), &Self::project(cx), verb, &[])
            .await
            .map_err(|e| engine_error("park", e))
    }

    async fn resume(&self, cx: &ToolCx<'_>) -> Result<()> {
        let containers = self.containers(cx).await?;
        let gone = self.gone(cx, &containers).await;
        if !gone.is_empty() && !containers.is_empty() {
            // A service removed outside the run (its container gone) can't come back as it was,
            // and what joins its namespace can't start: the run is started again with `up`.
            return Err(Error::PartlyGone {
                run: cx.run.to_string(),
                machines: gone,
            });
        }
        let paused: Vec<String> = containers.iter().filter(|c| c.is_paused()).map(|c| c.service().to_owned()).collect();
        if !paused.is_empty() {
            compose::act(&cx.call(CALL_LIMIT), &Self::project(cx), Verb::Unpause, &paused)
                .await
                .map_err(|e| engine_error("resume", e))?;
        }
        let stranded = namespaces::stranded(cx, &Self::project(cx), &containers).await;
        if !stranded.is_empty() {
            tracing::info!(run = %cx.run, services = ?stranded, "stopping the services left in the network namespace of one that stopped, so the start joins them to its new one");
            compose::act(&cx.call(CALL_LIMIT), &Self::project(cx), Verb::Stop, &stranded)
                .await
                .map_err(|e| engine_error("resume", e))?;
        }
        let how = Up {
            wait: true,
            no_recreate: true,
            ..Up::default()
        };
        self.up_recovering(cx, "resume", &how).await?;
        // The services under a profile the run attached: a plain `up` leaves them stopped.
        let attached = pins::attached(cx)?;
        if attached.is_empty() {
            return Ok(());
        }
        let theirs = Up { services: &attached, ..how };
        self.up_recovering(cx, "resume", &theirs).await
    }

    async fn update(&self, cx: &ToolCx<'_>) -> Result<()> {
        self.start(cx).await?;
        self.retire_unstarted(cx).await
    }

    async fn machines(&self, cx: &ToolCx<'_>) -> Result<Vec<String>> {
        compose::services(&cx.read(READ_LIMIT), &Self::project(cx))
            .await
            .map_err(|e| engine_error("services", e))
    }

    async fn reapply(&self, cx: &ToolCx<'_>, _change: &Change) -> Result<()> {
        // Compose diffs the files against the project's containers itself: one `up` makes what was
        // added, remakes what changed and (with --remove-orphans) removes what went.
        self.start(cx).await
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the trait's operations are async; Compose refuses this one before calling anything"
    )]
    async fn provision(&self, _cx: &ToolCx<'_>, _machine: Option<&str>, _with: Option<&str>) -> Result<()> {
        Err(Error::Declined {
            what: "provisioning a Compose project again (its services set themselves up when they start: update it instead)".to_owned(),
        })
    }

    async fn attach(&self, cx: &ToolCx<'_>, machine: &str) -> Result<()> {
        let services = one_service(machine);
        let how = Up {
            services: &services,
            wait: true,
            ..Up::default()
        };
        pins::attach(cx, machine).await?;
        fetched::recording(cx, &services, self.up_recovering(cx, "attach", &how)).await
    }

    async fn retire(&self, cx: &ToolCx<'_>, machine: &str) -> Result<()> {
        compose::rm_stop_force(&cx.call(CALL_LIMIT), &Self::project(cx), &one_service(machine))
            .await
            .map_err(|e| engine_error("retire", e))?;
        pins::retire(cx, machine)
    }

    async fn exec(&self, cx: &ToolCx<'_>, machine: &str, argv: &[String]) -> Result<Ran> {
        let call = cx.call(super::CHECK_LIMIT);
        let running = self.containers(cx).await?.iter().any(|c| c.service() == machine && c.machine_state().runs());
        let out = if running {
            let what = Exec {
                service: machine,
                user: None,
                argv,
                tty: false,
            };
            compose::exec(&call, &Self::project(cx), &what).await
        } else {
            compose::run_rm(&call, &Self::project(cx), machine, argv).await
        };
        ran(out)
    }

    fn console(&self, cx: &ToolCx<'_>, machine: &str, user: Option<&str>) -> impl Future<Output = Result<ShellCommand>> {
        let shell = user.map_or_else(|| shells::CONTAINER_SHELL.to_owned(), |_| shells::container_shell_as());
        let argv = vec!["sh".to_owned(), "-c".to_owned(), shell];
        let what = Exec {
            service: machine,
            user,
            argv: &argv,
            tty: true,
        };
        let spec = compose::exec_spec(&cx.call(READ_LIMIT), &Self::project(cx), &what);
        std::future::ready(Ok(ShellCommand::of_spec(spec, &cx.project.dir)))
    }

    async fn observe(&self, cx: &ToolCx<'_>) -> Result<(Seen, RunState)> {
        let containers = self.containers(cx).await?;
        let gone = self.gone(cx, &containers).await;
        let mut machines: Vec<SeenMachine> = containers
            .iter()
            .filter(|c| !c.one_off())
            .map(|c| SeenMachine {
                name: c.service().to_owned(),
                state: c.machine_state(),
                published: c
                    .published()
                    .into_iter()
                    .map(|p| SeenPort {
                        guest: p.guest,
                        host: p.host,
                        protocol: p.protocol,
                    })
                    .collect(),
                kind: None,
                address: None,
                windows: false,
            })
            .collect();
        // A service the project starts with no container reads absent: gone, never just left out.
        machines.extend(gone.into_iter().map(|name| SeenMachine {
            name,
            state: MachineState::Absent,
            published: Vec::new(),
            kind: None,
            address: None,
            windows: false,
        }));
        let state = run_state(&machines);
        Ok((Seen { machines, ..Seen::default() }, state))
    }

    async fn usage(&self, cx: &ToolCx<'_>) -> Result<Vec<crate::MachineUsage>> {
        let running: Vec<compose::Container> = self.containers(cx).await?.into_iter().filter(compose::Container::runs).collect();
        if running.is_empty() {
            return Ok(Vec::new());
        }
        let stats = docker::stats(&cx.read(READ_LIMIT)).await.map_err(|e| tool_error("usage", e))?;
        Ok(usage_by_service(&running, &stats))
    }

    async fn found(&self, cx: &ToolCx<'_>) -> Result<Vec<Found>> {
        let call = cx.read(READ_LIMIT);
        let mut found = Vec::new();
        for (object, kind, lifetime) in [
            (Object::Container, ResourceKind::Container, Lifetime::Machines),
            (Object::Network, ResourceKind::Network, Lifetime::Run),
            (Object::Volume, ResourceKind::Volume, Lifetime::Run),
        ] {
            for id in docker::labelled(&call, object, &Self::label(cx))
                .await
                .map_err(|e| tool_error("resources", e))?
            {
                found.push(Found {
                    key: format!("{}:{id}", object_word(object)),
                    kind,
                    native_id: id,
                    lifetime,
                });
            }
        }
        Ok(found)
    }

    async fn destroy_by_id(&self, cx: &ToolCx<'_>, _key: &str, kind: ResourceKind, native_id: &str) -> Result<()> {
        let object = match kind {
            ResourceKind::Container => Object::Container,
            ResourceKind::Network => Object::Network,
            ResourceKind::Volume => Object::Volume,
            other => {
                return Err(Error::Unsupported {
                    what: format!("removing a {other:?} resource through Compose"),
                });
            }
        };
        // An object already gone is fine: the adapter reads Docker's "no such" as done.
        docker::remove(&cx.call(CALL_LIMIT), object, &[native_id.to_owned()])
            .await
            .map_err(|e| tool_error("remove", e))
    }
}

/// The word a [`Found`] key starts with for `object`.
fn object_word(object: Object) -> &'static str {
    match object {
        Object::Container => "container",
        Object::Network => "network",
        Object::Volume => "volume",
    }
}

/// A command's outcome as [`Ran`]: a command that ran and failed is its exit status and what it
/// printed, not an error.
fn ran(out: std::result::Result<crate::process::Captured, CliError<Failure>>) -> Result<Ran> {
    let captured = out.map_err(|e| engine_error("exec", e))?;
    Ok(Ran {
        exit: captured.exit.code.unwrap_or(-1),
        stdout: captured.stdout.join("\n"),
        stderr: captured.stderr_tail.join("\n"),
    })
}

#[cfg(test)]
mod tests;
