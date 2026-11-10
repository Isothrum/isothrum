//! The `vagrant` actions about Vagrant's own record of the run: clearing
//! a record of another provider, a VirtualBox VM's cut-off saved state, the state folder, booting
//! the controller (waiting for one still halting) and halting it (waiting for one that powers
//! itself off).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use isoloom_manifest::{Provider, SnakeName, Step, Template};

use super::{READ_LIMIT, Vagrant, lock};
use crate::Result;
use crate::error::Error;
use crate::events::{Stream, WarningCode};
use crate::executor::{Cx, Echo, call, call_failed, call_ok, plan};
use crate::process::{Captured, ProcessSpec};

/// How long the clean-up destroy of a record on another provider may go without progress (as
/// every tool call's limit counts) before the record is dropped anyway (a wedged hypervisor must
/// not hang a start).
const RECONCILE_DESTROY_LIMIT: Duration = Duration::from_secs(180);

/// The controller's define name.
const CONTROLLER: &str = "isoloom-controller";

/// What the controller holds while it powers itself off after its play.
const HALTING: &str = "/run/isoloom-halting";

/// How many times, two seconds apart, a controller whose halt failed is read before the halt
/// counts as failed.
const HALT_POLLS: u32 = 60;

/// How long a controller that reads as running may take to say it stays up, or to go off, before
/// waking it fails.
const WAKE_PATIENCE: Duration = Duration::from_secs(180);

/// How long one ask of the controller whether it is halting may go without progress (as every
/// tool call's limit counts): no answer in time is no answer, asked again.
const PROBE_LIMIT: Duration = Duration::from_secs(20);

/// How long the controller's boot may go without progress, when its step sets no limit.
const BOOT_LIMIT: Duration = Duration::from_secs(1800);

/// How long the controller's halt may go without progress, when its step sets no limit.
const HALT_LIMIT: Duration = Duration::from_secs(300);

/// The providers Vagrant recorded a created machine under in `dotfile`
/// (`machines/<name>/<provider>/id`; a provider folder without its `id` is no machine: a destroy
/// or a mere `vagrant status` leaves one).
pub(super) fn recorded_providers(dotfile: &Path) -> Vec<String> {
    let Ok(machines) = std::fs::read_dir(dotfile.join("machines")) else {
        return Vec::new();
    };
    let mut out: Vec<String> = machines
        .flatten()
        .filter_map(|m| std::fs::read_dir(m.path()).ok())
        .flat_map(std::iter::Iterator::flatten)
        .filter(|p| p.path().join("id").is_file())
        .map(|p| p.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// `vagrant.reconcile_provider`: a run recorded under another provider (the run last ran on
/// VirtualBox, now on libvirt) is destroyed, given [`RECONCILE_DESTROY_LIMIT`] and best effort, and
/// its record dropped, so `vagrant up` doesn't refuse "a machine with a different provider".
pub(super) async fn reconcile_provider(v: &Vagrant, cx: &Cx<'_>, step: &Step) -> Result<()> {
    let dotfile = v.dotfile(cx)?;
    let asked = cx.scope.provider.clone().unwrap_or_default();
    if !recorded_providers(&dotfile).iter().any(|p| *p != asked) {
        return Ok(());
    }
    let id = step.id.as_str();
    cx.events
        .warning(WarningCode::OtherProvider, "this run last ran on another provider: clearing that run first");
    let destroy = v.vagrant(cx, id, &["destroy", "--force"])?;
    if let Err(e) = call(cx, id, &destroy, RECONCILE_DESTROY_LIMIT, Echo::Shown).await {
        cx.events.warning(WarningCode::StepTolerated, &e.to_string());
    }
    remove_dir(&dotfile)
}

/// `vagrant.reset_state`: Vagrant's state folder of the run, removed (a lock a killed process
/// left, a record of VMs that are gone).
pub(super) fn reset_state(v: &Vagrant, cx: &Cx<'_>) -> Result<()> {
    remove_dir(&v.dotfile(cx)?)
}

/// Removes `dir` and what it holds ([`crate::store::remove_path`]).
fn remove_dir(dir: &Path) -> Result<()> {
    crate::store::remove_path(dir).map_err(Error::io(dir, "removed"))
}

/// The run's VMs' names under `provider`, from the manifest: what orphan clean-up and the
/// VirtualBox actions match, exactly.
pub(super) fn vm_names(cx: &Cx<'_>, provider: Provider) -> Vec<String> {
    cx.run
        .manifest()
        .machines
        .iter()
        .filter_map(|m| match &m.native {
            Some(isoloom_manifest::Native::Vagrant { vm_names, .. }) => vm_names.get(&provider).cloned(),
            _ => None,
        })
        .collect()
}

/// `VBoxManage <args>`.
pub(super) fn vboxmanage(args: &[&str]) -> ProcessSpec {
    ProcessSpec::new("VBoxManage", args.iter().copied())
}

/// `vagrant.discard_aborted_saved`: a VirtualBox VM whose saved state was cut off (a suspend
/// killed midway, a host crash) is `aborted-saved`, and VirtualBox then refuses every change to it
/// ("the machine is not mutable"): its saved state is discarded, so it boots fresh from its disk.
/// A VM that doesn't exist is skipped.
pub(super) async fn discard_aborted_saved(cx: &Cx<'_>, step: &Step) -> Result<()> {
    let id = step.id.as_str();
    for name in vm_names(cx, Provider::Virtualbox) {
        let info = call(cx, id, &vboxmanage(&["showvminfo", &name, "--machinereadable"]), READ_LIMIT, Echo::Quiet).await?;
        if !info.succeeded() || !aborted_saved(&info.stdout) {
            continue;
        }
        cx.events.warning(
            WarningCode::SavedStateDiscarded,
            &format!("{name}: its saved state was cut off; it boots fresh"),
        );
        call_ok(cx, id, &vboxmanage(&["discardstate", &name]), READ_LIMIT, Echo::Quiet).await?;
    }
    Ok(())
}

/// Whether `showvminfo --machinereadable` says the VM's saved state was cut off.
pub(super) fn aborted_saved(lines: &[String]) -> bool {
    lines.iter().any(|l| l.trim() == "VMState=\"aborted-saved\"")
}

/// `vagrant.wake_controller`: the controller (`with.machine`, else Isoloom's) booted for the
/// checks. One still powering itself off after its play is waited for first, two seconds
/// between reads, booting it then would race its own poweroff: it says so (it holds
/// [`HALTING`]), or no longer answers over SSH on its way down (each ask given
/// [`PROBE_LIMIT`]). Only one that answers it isn't halting is left running as it is; one
/// still running and not saying so after [`WAKE_PATIENCE`] fails the step.
pub(super) async fn wake_controller(v: &Vagrant, cx: &Cx<'_>, step: &Step, with: &BTreeMap<SnakeName, Template>) -> Result<()> {
    let id = step.id.as_str();
    let machine = controller(cx, step, with)?;
    let deadline = tokio::time::Instant::now() + WAKE_PATIENCE;
    while running(v, cx, id, &machine).await? {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Err(Error::ToolFailed {
                step: id.to_owned(),
                reason: format!(
                    "`{machine}` still reads as running after {} s without saying over SSH that it stays up: \
                     it may be stuck halting, or its SSH may not answer",
                    WAKE_PATIENCE.as_secs()
                ),
            });
        }
        let probe = v.vagrant(cx, id, &["ssh", &machine, "-c", &halting_probe()])?;
        match call(cx, id, &probe, PROBE_LIMIT.min(left), Echo::Quiet).await {
            // Running, and not on its way down: nothing to wait for.
            Ok(said) if said.succeeded() && said.stdout.iter().any(|l| l.trim() == STAYS_UP) => return Ok(()),
            Ok(_) | Err(Error::Timeout { .. }) => {}
            Err(e) => return Err(e),
        }
        tokio::time::sleep(Duration::from_secs(2).min(deadline.saturating_duration_since(tokio::time::Instant::now()))).await;
    }
    let limit = plan::limit_or(step, &cx.scope, BOOT_LIMIT);
    let up = v.vagrant(cx, id, &["up", &machine, "--no-provision"])?;
    call_ok(cx, id, &up, limit, Echo::Shown).await.map(drop)
}

/// What the controller prints when it isn't on its way down (see [`halting_probe`]).
const STAYS_UP: &str = "up";

/// Run on the controller: [`STAYS_UP`] unless it holds [`HALTING`]. An answer, not an exit code:
/// SSH failing on a controller going down must not read as one staying up.
fn halting_probe() -> String {
    format!("if test -e {HALTING}; then echo halting; else echo {STAYS_UP}; fi")
}

/// What vagrant-vmware-esxi says when the VM it is asked to halt is already off: ESXi refuses the
/// guest shutdown (`vim.fault.InvalidPowerState`, the VM `poweredOff`).
const ESXI_ALREADY_OFF: [&str; 2] = ["existingState = \"poweredOff\"", "in the current state (Powered off)"];

/// `vagrant.park_controller`: the controller (`with.machine`, else Isoloom's) halted. After `up`
/// it powers itself off at the end of its play, and a `vagrant halt` racing that fails: on ESXi,
/// which refuses to shut down a VM already off, that is done at once; elsewhere (libvirt can no
/// longer reach it over SSH) a failed halt is waited for, up to [`HALT_POLLS`] times two
/// seconds, and counts as done once the controller is off. A halt another Vagrant process's lock
/// refused is tried again first ([`lock`]). The halt's lines reach the log once it is over, so
/// ESXi's refusal never shows as an error.
pub(super) async fn park_controller(v: &Vagrant, cx: &Cx<'_>, step: &Step, with: &BTreeMap<SnakeName, Template>) -> Result<()> {
    let id = step.id.as_str();
    let machine = controller(cx, step, with)?;
    let limit = plan::limit_or(step, &cx.scope, HALT_LIMIT);
    let halt = lock::call_unlocked(cx, id, &v.vagrant(cx, id, &["halt", &machine])?, limit, Echo::Quiet).await?;
    let halted = halt.succeeded();
    if !halted && already_off(&halt) {
        cx.events.log(id, Stream::Stdout, &format!("{machine} was already off."));
        return Ok(());
    }
    let said = halt
        .stdout
        .iter()
        .map(|l| (Stream::Stdout, l))
        .chain(halt.stderr.iter().map(|l| (Stream::Stderr, l)));
    for (stream, line) in said {
        cx.events.log(id, stream, line);
    }
    if halted {
        return Ok(());
    }
    for _ in 0..HALT_POLLS {
        if !running(v, cx, id, &machine).await? {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(Error::ToolFailed {
        step: id.to_owned(),
        reason: cx
            .events
            .mask(&format!("`vagrant halt {machine}` failed: {}", crate::executor::why_failed(&halt))),
    })
}

/// Whether a failed halt says the VM was already off (ESXi's refusal).
fn already_off(halt: &Captured) -> bool {
    halt.stdout
        .iter()
        .chain(&halt.stderr)
        .any(|line| ESXI_ALREADY_OFF.iter().any(|said| line.contains(said)))
}

/// The controller step `step`'s action names (`with.machine`), else Isoloom's.
fn controller(cx: &Cx<'_>, step: &Step, with: &BTreeMap<SnakeName, Template>) -> Result<String> {
    Ok(cx.vars.with_arg(with, step.id.as_str(), "machine")?.unwrap_or_else(|| CONTROLLER.to_owned()))
}

/// Whether Vagrant reads `machine` as running.
async fn running(v: &Vagrant, cx: &Cx<'_>, id: &str, machine: &str) -> Result<bool> {
    let spec = v.vagrant(cx, id, &["status", machine, "--machine-readable"])?;
    let read = lock::call_unlocked(cx, id, &spec, READ_LIMIT, Echo::Quiet).await?;
    if !read.succeeded() {
        return Err(call_failed(cx, id, &spec, &read));
    }
    Ok(super::status::parse(&read.stdout.join("\n")).states.get(machine).map(String::as_str) == Some("running"))
}
