//! The Vagrant driver: a Vagrantfile's machines, on the provider the run asks for, through
//! `vagrant` with its dotfile folder in the run's state folder ([`dotfile`]), so a replaced
//! project folder never loses track of its VMs. The run's inputs and key reach the Vagrantfile in
//! the tool's environment ([`ToolCx::env`]), which it reads with `ENV.fetch`: the file is never
//! rewritten.
//!
//! | Operation | Vagrant |
//! |---|---|
//! | up | the preamble, then `up --provider <p>`: ready once it exits 0, after the last provisioner |
//! | down | `destroy -f`, then the dotfile reset (what is left by id goes to [`FormatDriver::destroy_by_id`]) |
//! | park | `suspend` (Pause), `halt` (Shutdown) |
//! | resume | refused naming the machines destroyed outside the run ([`gone`]); else the preamble, then `up --provider <p> --no-provision` |
//! | update, provision | `up --provider <p> --provision [machine]` |
//! | attach, retire | `up --provider <p> <machine>`, `destroy -f <machine>` |
//! | exec, console | `ssh <machine> -c <command>`; a console is `ssh <machine>` with a terminal |
//! | observe | `status --machine-readable`, then `port <machine>` of each running one; a machine `not_created` whose VM the store records as made is gone ([`gone`]) |
//! | found | the dotfile's `machines/<machine>/<provider>/id` |
//! | machines | the machines `status` lists, whether made or not |
//! | reapply | `up <added>`, `destroy -f <removed>`, `provision <changed>` |
//! | graphical, native console | the hypervisor, by the VM's recorded id ([`screen`]) |
//!
//! The preamble: a record of the run on another provider than the one asked is destroyed and the
//! dotfile reset (`up` would refuse "a machine with a different provider"); a VirtualBox VM whose
//! saved state was cut off (`aborted-saved`) has it discarded. Recovery: an `up` refused because
//! an earlier VM holds the name (`VERR_ALREADY_EXISTS`, a folder that can't be renamed) or a record
//! is on another provider removes what the run made, by the dotfile's ids and, on VirtualBox, the
//! VMs and folders whose name carries the run's key as a word; then it starts once more. A call
//! another Vagrant process's lock refused is made again ([`lock`]).

mod booting;
mod busy;
mod clear;
mod dotfile;
mod failures;
mod forwards;
mod gone;
mod lock;
use gone::run_state;
mod names;
mod providers;
mod screen;
mod utm;
use names::{checked_user, key_of, of_key, one, ours, own_address, verb_word};

use std::time::Duration;

use self::dotfile::{Dotfile, Record};
use self::failures::vagrant_error;
use self::lock::{PATIENCE, Rerun, STATUS_PATIENCE, retrying};
use super::{Change, FormatDriver, Found, Ran, Seen, SeenMachine, SeenPort, ToolCx, tool_error};
use crate::Result;
use crate::cli::vagrant::{self, Failure, Provision, Verb};
use crate::cli::{Call, CliError, vboxmanage};
use crate::console::ShellCommand;
use crate::error::Error;
use crate::events::WarningCode;
use crate::project::platform::Provider;
use crate::state::{Park, RunState};
use crate::status::MachineState;
use crate::store::{Lifetime, ResourceKind};

/// How long an `up` may take: boxes fetched, every VM booted and provisioned.
const UP_LIMIT: Duration = Duration::from_secs(3600);

/// How long a lifecycle call that makes nothing may take (halt, suspend, destroy).
const CALL_LIMIT: Duration = Duration::from_secs(900);

/// How long the clean-up destroy of a record on another provider may take before the record is
/// dropped anyway: a wedged hypervisor must not hang a start.
const RECONCILE_LIMIT: Duration = Duration::from_secs(180);

/// How long a read may take (`status`, `port`, a version, the plugins, a VM's state).
const READ_LIMIT: Duration = Duration::from_secs(120);

/// The Vagrant driver.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Vagrant;

/// A call of `vagrant` (or of the hypervisor's tool) for `cx`, with the dotfile folder set.
fn call<'a>(cx: &'a ToolCx<'_>, limit: Duration) -> Call<'a> {
    let mut call = cx.call(limit);
    call.env.retain(|(k, _)| k != dotfile::VAR);
    call.env.push((dotfile::VAR.to_owned(), dot(cx).path.display().to_string()));
    call
}

/// A read of Vagrant (its status, a machine's ports, its version): [`call`] whose lines aren't
/// said in the operation's log.
fn read<'a>(cx: &'a ToolCx<'_>, limit: Duration) -> Call<'a> {
    Call {
        sink: &super::discard,
        ..call(cx, limit)
    }
}

/// The run's dotfile folder.
fn dot(cx: &ToolCx<'_>) -> Dotfile {
    Dotfile::in_state(cx.state_dir)
}

/// The provider asked for, refused when there is none.
fn asked(cx: &ToolCx<'_>) -> Result<Provider> {
    cx.provider.ok_or_else(|| Error::NoProvider {
        providers: providers::listed_for(providers::this_host()),
    })
}

impl Vagrant {
    /// `up` of `machines` (all when empty) on the provider asked for, made again while another
    /// process's lock refuses it and a second run is safe.
    /// On UTM, one an Apple event cut short in passing is made once more ([`utm::once_more`]); one
    /// a machine still booting failed is made again once it may answer ([`booting::until_it_answers`]).
    async fn up_call(&self, cx: &ToolCx<'_>, step: &str, provision: Provision, machines: &[String]) -> std::result::Result<(), CliError<Failure>> {
        let provider = cx.provider.map(Provider::id);
        let rerun = if provision == Provision::Yes { Rerun::Repeats } else { Rerun::Converges };
        let call = &call(cx, UP_LIMIT);
        booting::until_it_answers(cx, step, move || {
            utm::once_more(cx, step, move || {
                retrying(cx.warn, step, rerun, PATIENCE, move || vagrant::up(call, provider, provision, machines))
            })
        })
        .await
    }

    /// `verb` on `machines` (all when empty), made again as [`Rerun`] allows.
    async fn act(&self, cx: &ToolCx<'_>, verb: Verb, machines: &[String], limit: Duration) -> std::result::Result<(), CliError<Failure>> {
        let rerun = match verb {
            Verb::Halt | Verb::Suspend | Verb::Destroy => Rerun::Converges,
            Verb::Provision => Rerun::Repeats,
        };
        let call = call(cx, limit);
        retrying(cx.warn, verb_word(verb), rerun, PATIENCE, || vagrant::act(&call, verb, machines)).await
    }

    /// Each machine `status` lists, with its state label.
    async fn status(&self, cx: &ToolCx<'_>) -> Result<std::collections::BTreeMap<String, String>> {
        self.read_states(cx).await.map_err(|e| vagrant_error(cx, "status", e))
    }

    /// [`Vagrant::status`], its failure as Vagrant's (a lock another process holds).
    async fn read_states(&self, cx: &ToolCx<'_>) -> std::result::Result<std::collections::BTreeMap<String, String>, CliError<Failure>> {
        let call = read(cx, READ_LIMIT);
        let read = retrying(cx.warn, "status", Rerun::Read, STATUS_PATIENCE, || vagrant::status(&call)).await?;
        Ok(read.states)
    }

    /// The preamble of `up` and `resume`: a record on another provider cleared, a cut-off saved
    /// state discarded.
    async fn preamble(&self, cx: &ToolCx<'_>, provider: Provider) -> Result<()> {
        self.reconcile_provider(cx, provider).await?;
        self.discard_aborted_saved(cx, provider).await
    }

    /// A run recorded under another provider than `provider` (it last ran on VirtualBox, now on
    /// libvirt) is destroyed, given [`RECONCILE_LIMIT`] and best effort, and its record dropped.
    async fn reconcile_provider(&self, cx: &ToolCx<'_>, provider: Provider) -> Result<()> {
        let dot = dot(cx);
        if !dot.recorded()?.iter().any(|r| r.provider != provider.id()) {
            return Ok(());
        }
        (cx.warn)(WarningCode::OtherProvider, "this run last ran on another provider: clearing that run first");
        if let Err(e) = self.act(cx, Verb::Destroy, &[], RECONCILE_LIMIT).await {
            (cx.warn)(WarningCode::CleanupFailed, &format!("the run on the other provider couldn't be destroyed: {e}"));
        }
        dot.reset()
    }

    /// A VirtualBox VM of the run whose saved state was cut off (a suspend killed midway, a host
    /// crash) has it discarded, so it boots fresh from its disk: VirtualBox refuses every change to
    /// such a VM. A VM whose state can't be read is left as it is (`up` says what is wrong).
    async fn discard_aborted_saved(&self, cx: &ToolCx<'_>, provider: Provider) -> Result<()> {
        if provider != Provider::Virtualbox {
            return Ok(());
        }
        let call = call(cx, READ_LIMIT);
        for r in dot(cx).recorded()?.into_iter().filter(|r| r.provider == provider.id()) {
            match vboxmanage::vm_state(&call, &r.id).await {
                Ok(vboxmanage::VmState::AbortedSaved) => {
                    (cx.warn)(
                        WarningCode::SavedStateDiscarded,
                        &format!("{}: its saved state was cut off; it boots fresh", r.machine),
                    );
                    vboxmanage::discard_state(&call, &r.id).await.map_err(|e| tool_error("discard-saved", e))?;
                }
                Ok(vboxmanage::VmState::Other(_)) => {}
                Err(e) => tracing::debug!(vm = %r.machine, error = %e, "the VM's state couldn't be read: its saved state, if any, is left as it is"),
            }
        }
        Ok(())
    }

    /// `up` as asked, with one recovery: a name an earlier VM holds, or a record on another
    /// provider, clears what the run made ([`Vagrant::clear`]) and starts once more.
    async fn up_recovering(&self, cx: &ToolCx<'_>, step: &str, provision: Provision, machines: &[String]) -> Result<()> {
        match self.up_call(cx, step, provision, machines).await {
            Ok(()) => Ok(()),
            Err(CliError::Failed {
                failure: Failure::AlreadyExists | Failure::OtherProvider,
                ..
            }) => {
                tracing::info!(run = %cx.run, "an earlier start's VM holds a name of the run: removing what the run made, then starting again");
                self.clear(cx).await?;
                self.up_call(cx, step, provision, machines).await.map_err(|e| vagrant_error(cx, step, e))
            }
            Err(e) => Err(vagrant_error(cx, step, e)),
        }
    }
}

impl FormatDriver for Vagrant {
    /// Not after a start that didn't end well: a VM running says nothing of the provisioning that
    /// start never finished (one killed while Vagrant provisioned it), and a machine it never
    /// reached reads `not_created`, which doesn't count ([`gone::run_state`]); read ready, the run
    /// would refuse the next `up` as already running. A run failed otherwise (a VM that died,
    /// booted again from its hypervisor's app) is ready again once its VMs all run.
    fn ready_by_observation(&self, failed_start: bool) -> bool {
        !failed_start
    }

    /// Hyper-V takes its management commands from an administrator only.
    fn elevates(&self, provider: Option<Provider>) -> bool {
        provider == Some(Provider::Hyperv)
    }

    async fn probe(&self, cx: &ToolCx<'_>) -> Result<()> {
        let provider = asked(cx)?;
        let host = providers::this_host();
        let refuse = |reason: String| Error::Provider {
            need: None,
            provider: provider.id().to_owned(),
            reason,
        };
        if !providers::hosts(provider).contains(&host) {
            return Err(refuse(format!(
                "doesn't run on this computer's system; these do: {}",
                providers::listed_for(host)
            )));
        }
        let read = read(cx, READ_LIMIT);
        let found = vagrant::supported_version(&read).await.map_err(|e| vagrant_error(cx, "tools", e))?;
        super::unknown_version(cx, found.as_ref(), "vagrant", vagrant::MIN_VERSION);
        if let Some(plugin) = provider.plugin() {
            let installed = vagrant::plugins(&read).await.map_err(|e| vagrant_error(cx, "tools", e))?;
            if !installed.iter().any(|(name, _)| name == plugin) {
                return Err(refuse(format!("needs the Vagrant plugin `{plugin}`: `vagrant plugin install {plugin}`")));
            }
        }
        Ok(())
    }

    async fn up(&self, cx: &ToolCx<'_>) -> Result<()> {
        let provider = asked(cx)?;
        crate::store::create_dir(cx.state_dir)?;
        self.preamble(cx, provider).await?;
        self.up_recovering(cx, "up", Provision::Default, &[]).await
    }

    async fn down(&self, cx: &ToolCx<'_>) -> Result<()> {
        self.act(cx, Verb::Destroy, &[], CALL_LIMIT).await.map_err(|e| vagrant_error(cx, "down", e))?;
        // What `destroy` took is gone from the dotfile; a VM it left keeps its resource row, which
        // the core removes by id (`destroy_by_id`), with its record written back if need be.
        dot(cx).reset()
    }

    async fn park(&self, cx: &ToolCx<'_>, mode: Park) -> Result<()> {
        let verb = match mode {
            Park::Pause => Verb::Suspend,
            Park::Shutdown => Verb::Halt,
        };
        match self.act(cx, verb, &[], CALL_LIMIT).await {
            // ESXi refuses to halt a VM already off: halted is what was asked.
            Ok(())
            | Err(CliError::Failed {
                failure: Failure::AlreadyOff, ..
            }) => Ok(()),
            Err(e) => Err(vagrant_error(cx, "park", e)),
        }
    }

    async fn resume(&self, cx: &ToolCx<'_>) -> Result<()> {
        let provider = asked(cx)?;
        self.refuse_resume_if_gone(cx).await?;
        self.preamble(cx, provider).await?;
        self.up_recovering(cx, "resume", Provision::No, &[]).await
    }

    async fn update(&self, cx: &ToolCx<'_>) -> Result<()> {
        asked(cx)?;
        self.up_call(cx, "update", Provision::Yes, &[])
            .await
            .map_err(|e| vagrant_error(cx, "update", e))
    }

    async fn machines(&self, cx: &ToolCx<'_>) -> Result<Vec<String>> {
        Ok(self.status(cx).await?.into_keys().collect())
    }

    async fn reapply(&self, cx: &ToolCx<'_>, change: &Change) -> Result<()> {
        if !change.added.is_empty() {
            asked(cx)?;
            self.up_call(cx, "reapply", Provision::Default, &change.added)
                .await
                .map_err(|e| vagrant_error(cx, "reapply", e))?;
        }
        if !change.removed.is_empty() {
            // Vagrant acts only on machines the Vagrantfile defines: one it no longer does is
            // removed by its recorded id.
            let defined = self.machines(cx).await?;
            let (known, gone): (Vec<String>, Vec<String>) = change.removed.iter().cloned().partition(|m| defined.contains(m));
            if !known.is_empty() {
                self.act(cx, Verb::Destroy, &known, CALL_LIMIT)
                    .await
                    .map_err(|e| vagrant_error(cx, "reapply", e))?;
            }
            for r in dot(cx).recorded()?.into_iter().filter(|r| gone.contains(&r.machine)) {
                self.remove_at_hypervisor(cx, &r).await?;
            }
        }
        if !change.changed.is_empty() {
            self.act(cx, Verb::Provision, &change.changed, UP_LIMIT)
                .await
                .map_err(|e| vagrant_error(cx, "reapply", e))?;
        }
        Ok(())
    }

    async fn provision(&self, cx: &ToolCx<'_>, machine: Option<&str>, with: Option<&str>) -> Result<()> {
        asked(cx)?;
        let machines: Vec<String> = machine.map(one).unwrap_or_default();
        let Some(name) = with else {
            return self
                .up_call(cx, "provision", Provision::Yes, &machines)
                .await
                .map_err(|e| vagrant_error(cx, "provision", e));
        };
        // One provisioner on machines that run: nothing else of their set-up runs again.
        let name = vagrant::ProvisionerName::new(name).ok_or_else(|| Error::Input {
            name: "provisioner".to_owned(),
            reason: format!("`{name}` isn't a provisioner's name (letters, digits, `-`, `_`)"),
        })?;
        let call = call(cx, super::CHECK_LIMIT);
        retrying(cx.warn, "provision", Rerun::Repeats, PATIENCE, || {
            vagrant::provision_with(&call, &name, &machines)
        })
        .await
        .map_err(|e| vagrant_error(cx, "provision", e))
    }

    async fn attach(&self, cx: &ToolCx<'_>, machine: &str) -> Result<()> {
        asked(cx)?;
        self.up_call(cx, "attach", Provision::Default, &one(machine))
            .await
            .map_err(|e| vagrant_error(cx, "attach", e))
    }

    async fn retire(&self, cx: &ToolCx<'_>, machine: &str) -> Result<()> {
        self.act(cx, Verb::Destroy, &one(machine), CALL_LIMIT)
            .await
            .map_err(|e| vagrant_error(cx, "retire", e))
    }

    async fn exec(&self, cx: &ToolCx<'_>, machine: &str, argv: &[String]) -> Result<Ran> {
        let script = argv.iter().map(|w| crate::console::sh_word(w)).collect::<Vec<_>>().join(" ");
        let out = vagrant::ssh_run(&call(cx, super::CHECK_LIMIT), machine, &script)
            .await
            .map_err(|e| vagrant_error(cx, "exec", e))?;
        Ok(Ran {
            exit: out.exit.code.unwrap_or(-1),
            stdout: out.stdout.join("\n"),
            stderr: out.stderr_tail.join("\n"),
        })
    }

    fn console(&self, cx: &ToolCx<'_>, machine: &str, user: Option<&str>) -> impl Future<Output = Result<ShellCommand>> {
        let script = user.map(checked_user).transpose().map(|u| u.map(crate::console::shells::login_shell_as));
        std::future::ready(script.map(|script| {
            let spec = vagrant::console_spec(&call(cx, READ_LIMIT), machine, script.as_deref());
            ShellCommand::of_spec(spec, &cx.project.dir)
        }))
    }

    async fn observe(&self, cx: &ToolCx<'_>) -> Result<(Seen, RunState)> {
        let read = read(cx, READ_LIMIT);
        let records = dot(cx).recorded()?;
        let mut machines = Vec::new();
        let states = match self.read_states(cx).await {
            Ok(states) => states,
            // Another process acts on these machines (a start, a provision from a terminal) past
            // the read's patience: they are busy, the rest as last read.
            Err(CliError::Failed {
                failure: Failure::Locked { machines: named, .. },
                ..
            }) if !named.is_empty() => return Ok((busy::busy(named), RunState::Ready)),
            Err(e) => return Err(vagrant_error(cx, "status", e)),
        };
        // A machine destroyed outside the run reads absent, as one never made, but fails the run.
        let gone = gone::gone(cx, &states)?;
        for (name, label) in states {
            let state = crate::tool_output::vagrant::machine_state(&label);
            // The provider it was made on, as the dotfile records it (the one asked for, unless a
            // record was left on another).
            let asked = cx.provider.map(Provider::id);
            let mine = records.iter().filter(|r| r.machine == name);
            let kind = mine
                .clone()
                .find(|r| Some(r.provider.as_str()) == asked)
                .or_else(|| mine.clone().next())
                .map(|r| r.provider.clone());
            let mut published = Vec::new();
            if state == MachineState::Running {
                match retrying(cx.warn, "port", Rerun::Read, STATUS_PATIENCE, || vagrant::port(&read, &name)).await {
                    // `port` doesn't say a forward's protocol: it is left unknown, never guessed.
                    Ok(forwards) => published.extend(forwards.into_iter().map(|f| SeenPort {
                        guest: f.guest,
                        host: f.host,
                        protocol: None,
                    })),
                    // vagrant-qemu can't list them: its QEMU says them. libvirt can't either, and
                    // ESXi forwards nothing: no port is proven.
                    Err(CliError::Failed {
                        failure: Failure::PortsUnsupported,
                        ..
                    }) => {
                        if kind.as_deref() == Some(Provider::Qemu.id()) {
                            published.extend(forwards::of_qemu(cx, &name).await);
                        }
                    }
                    Err(e) => return Err(vagrant_error(cx, "status", e)),
                }
            }
            let address = if state == MachineState::Running && kind.as_deref().is_some_and(own_address) {
                self.address(cx, &name).await
            } else {
                None
            };
            machines.push(SeenMachine {
                name,
                state,
                published,
                kind,
                address,
                windows: false,
            });
        }
        let state = run_state(&machines, &gone);
        Ok((Seen { machines, ..Seen::default() }, state))
    }

    /// Read from the dotfile: no call.
    fn found(&self, cx: &ToolCx<'_>) -> impl Future<Output = Result<Vec<Found>>> {
        let found = dot(cx).recorded().map(|records| {
            records
                .into_iter()
                .map(|r| Found {
                    key: key_of(&r, cx.provider),
                    kind: ResourceKind::Vm,
                    native_id: r.id,
                    lifetime: Lifetime::Machines,
                })
                .collect()
        });
        std::future::ready(found)
    }

    async fn destroy_by_id(&self, cx: &ToolCx<'_>, key: &str, kind: ResourceKind, native_id: &str) -> Result<()> {
        let refused = || Error::Unsupported {
            what: format!("removing the {kind:?} resource `{key}` through Vagrant"),
        };
        if kind != ResourceKind::Vm {
            return Err(refused());
        }
        let (machine, provider) = of_key(key, cx.provider).ok_or_else(refused)?;
        let dot = dot(cx);
        let recorded = dot.recorded()?;
        let record = if let Some(r) = recorded.into_iter().find(|r| r.id == native_id) {
            r
        } else {
            let Some(provider) = provider else {
                return Err(Error::NoProvider {
                    providers: providers::listed_for(providers::this_host()),
                });
            };
            let lost = Record {
                machine,
                provider,
                id: native_id.to_owned(),
            };
            // Vagrant acts on a VM by the id its dotfile records: written back, `destroy` takes
            // the VM whose record a reset or a crash lost.
            dot.write_back(&lost)?;
            lost
        };
        match self.act(cx, Verb::Destroy, &one(&record.machine), CALL_LIMIT).await {
            Ok(()) => dot.forget(&record.machine, &record.provider),
            // A machine the Vagrantfile no longer defines: its VM goes at the hypervisor.
            Err(CliError::Failed {
                failure: Failure::UnknownMachine,
                ..
            }) => self.remove_at_hypervisor(cx, &record).await,
            Err(e) => Err(vagrant_error(cx, "remove", e)),
        }
    }

    async fn graphical(&self, cx: &ToolCx<'_>, _seen: &Seen, machine: &SeenMachine) -> Result<Option<crate::graphical::ConsoleOffer>> {
        Ok(screen::offer(cx, machine).await)
    }

    async fn reach_graphical(&self, cx: &ToolCx<'_>, machine: &str) -> Result<crate::graphical::Console> {
        screen::reach(cx, machine).await
    }

    fn native_app(&self, cx: &ToolCx<'_>, seen: &Seen) -> Option<crate::graphical::NativeApp> {
        screen::app(cx, seen)
    }

    async fn native_console(&self, cx: &ToolCx<'_>, machine: &str) -> Result<crate::graphical::NativeConsole> {
        screen::native(cx, machine).await
    }
}

#[cfg(test)]
mod tests;
