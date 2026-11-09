//! The `vagrant` actions about Vagrant's own record of the run: clearing
//! a record of another provider, a VirtualBox VM's cut-off saved state, the state folder, booting
//! the controller (waiting for one still halting) and halting it (waiting for one that powers
//! itself off).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use isoloom_manifest::{Provider, SnakeName, Step, Template};

use super::{READ_TIMEOUT, Vagrant};
use crate::Result;
use crate::error::Error;
use crate::events::{Stream, WarningCode};
use crate::executor::{Cx, Echo, call, call_ok};
use crate::process::{Captured, ProcessSpec};

/// How long the clean-up destroy of a record on another provider may take before the record is
/// dropped anyway (a wedged hypervisor must not hang a start).
const RECONCILE_DESTROY: Duration = Duration::from_secs(180);

/// The controller's define name.
const CONTROLLER: &str = "isoloom-controller";

/// What the controller holds while it powers itself off after its play.
const HALTING: &str = "/run/isoloom-halting";

/// How many times, two seconds apart, the controller may still be halting before it is booted.
const HALT_POLLS: u32 = 60;

/// How long the controller's boot may take, when its step sets no limit.
const BOOT_TIMEOUT: Duration = Duration::from_secs(1800);

/// How long the controller's halt may take, when its step sets no limit.
const HALT_TIMEOUT: Duration = Duration::from_secs(300);

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
/// VirtualBox, now on libvirt) is destroyed, within [`RECONCILE_DESTROY`] and best effort, and
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
    if let Err(e) = call(cx, id, &destroy, RECONCILE_DESTROY, Echo::Shown).await {
        cx.events.warning(WarningCode::StepTolerated, &e.to_string());
    }
    remove_dir(&dotfile)
}

/// `vagrant.reset_state`: Vagrant's state folder of the run, removed (a lock a killed process
/// left, a record of VMs that are gone).
pub(super) fn reset_state(v: &Vagrant, cx: &Cx<'_>) -> Result<()> {
    remove_dir(&v.dotfile(cx)?)
}

/// Removes `dir` and what it holds; a missing one is nothing to do. A symbolic link is removed
/// itself, never followed.
fn remove_dir(dir: &Path) -> Result<()> {
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(dir, "removed")(e)),
        Ok(m) if m.file_type().is_symlink() || m.is_file() => std::fs::remove_file(dir).map_err(Error::io(dir, "removed")),
        Ok(_) => std::fs::remove_dir_all(dir).map_err(Error::io(dir, "removed")),
    }
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
        let info = call(cx, id, &vboxmanage(&["showvminfo", &name, "--machinereadable"]), READ_TIMEOUT, Echo::Quiet).await?;
        if !info.exit.is_some_and(crate::process::Exit::success) || !aborted_saved(&info.stdout) {
            continue;
        }
        cx.events.warning(
            WarningCode::SavedStateDiscarded,
            &format!("{name}: its saved state was cut off; it boots fresh"),
        );
        call_ok(cx, id, &vboxmanage(&["discardstate", &name]), READ_TIMEOUT, Echo::Quiet).await?;
    }
    Ok(())
}

/// Whether `showvminfo --machinereadable` says the VM's saved state was cut off.
pub(super) fn aborted_saved(lines: &[String]) -> bool {
    lines.iter().any(|l| l.trim() == "VMState=\"aborted-saved\"")
}

/// `vagrant.wake_controller`: the controller (`with.machine`, else Isoloom's) booted for the
/// checks. One still powering itself off after its play is waited for first, up to
/// [`HALT_POLLS`] times two seconds, booting it then would race its own poweroff: it says so
/// (it holds [`HALTING`]), or no longer answers over SSH on its way down. Only one that answers
/// it isn't halting is left running as it is.
pub(super) async fn wake_controller(v: &Vagrant, cx: &Cx<'_>, step: &Step, with: &BTreeMap<SnakeName, Template>) -> Result<()> {
    let id = step.id.as_str();
    let machine = controller(cx, with)?;
    for _ in 0..HALT_POLLS {
        if !running(v, cx, id, &machine).await? {
            break;
        }
        let probe = v.vagrant(cx, id, &["ssh", &machine, "-c", &halting_probe()])?;
        let said = call(cx, id, &probe, READ_TIMEOUT, Echo::Quiet).await?;
        if said.exit.is_some_and(crate::process::Exit::success) && said.stdout.iter().any(|l| l.trim() == STAYS_UP) {
            // Running, and not on its way down: nothing to wait for.
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let limit = step.timeout_s.map_or(BOOT_TIMEOUT, |s| Duration::from_secs(u64::from(s.get())));
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
/// seconds, and counts as done once the controller is off. The halt's lines reach the log once
/// it is over, so ESXi's refusal never shows as an error.
pub(super) async fn park_controller(v: &Vagrant, cx: &Cx<'_>, step: &Step, with: &BTreeMap<SnakeName, Template>) -> Result<()> {
    let id = step.id.as_str();
    let machine = controller(cx, with)?;
    let limit = step.timeout_s.map_or(HALT_TIMEOUT, |s| Duration::from_secs(u64::from(s.get())));
    let halt = call(cx, id, &v.vagrant(cx, id, &["halt", &machine])?, limit, Echo::Quiet).await?;
    let halted = halt.exit.is_some_and(crate::process::Exit::success);
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

/// The controller an action names (`with.machine`), else Isoloom's.
fn controller(cx: &Cx<'_>, with: &BTreeMap<SnakeName, Template>) -> Result<String> {
    match with.iter().find(|(k, _)| k.as_str() == "machine") {
        Some((_, t)) => cx.vars.render(t, "with.machine"),
        None => Ok(CONTROLLER.to_owned()),
    }
}

/// Whether Vagrant reads `machine` as running.
async fn running(v: &Vagrant, cx: &Cx<'_>, id: &str, machine: &str) -> Result<bool> {
    let spec = v.vagrant(cx, id, &["status", machine, "--machine-readable"])?;
    let status = call_ok(cx, id, &spec, READ_TIMEOUT, Echo::Quiet).await?;
    Ok(super::status::parse(&status).states.get(machine).map(String::as_str) == Some("running"))
}
