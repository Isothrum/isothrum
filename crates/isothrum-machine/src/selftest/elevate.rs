//! The VM self-test's Hyper-V elevation hook: an injection point, not an implementation.
//!
//! Hyper-V takes management commands from an administrator only (or a member of "Hyper-V
//! Administrators"), and Vagrant's Hyper-V provider refuses to run otherwise. An embedder may
//! boot the test VM and probe it as one elevated batch (one UAC prompt for both `vagrant up` and
//! `vagrant ssh`) through its own `unsafe` Win32 code. That `unsafe` is exactly what this crate
//! forbids, so isothrum-machine never elevates itself: the VM self-test takes an
//! optional [`ElevatedBoot`] a caller supplies. `None` (the default) runs the
//! two commands as plain [`ProcessRunner`](isothrum_runner::ProcessRunner) calls — today's path,
//! one prompt each on Hyper-V. `Some` routes the whole boot-and-probe through the caller's hook, so
//! both commands share one prompt.

use std::path::Path;

use isothrum_runner::BoxFuture;

use crate::error::Result;

/// A caller-supplied way to boot the test VM and probe it as one elevated batch.
///
/// An embedder implements this over an elevated batch of its own (`up` as step 0, `ssh` as step
/// 1, both under one UAC prompt), mapping that batch's two outcomes onto [`BootProbe`]. An
/// embedder that passes none keeps its VM self-test on the unelevated two-call path.
pub trait ElevatedBoot: Send + Sync {
    /// Boots the test VM (`vagrant` with `up`), then runs one command inside it (`vagrant` with
    /// `ssh`), as one elevated batch in `dir`. Each boot output line is handed to `on_line` as it
    /// arrives (the probe's output is returned in [`BootProbe`], not streamed).
    ///
    /// The returned [`BootProbe`] carries each command's own outcome. The outer `Result` is `Err`
    /// only when the batch could not run at all (the elevation prompt was declined, or the
    /// elevated session ended) — a failed boot or probe is inside [`BootProbe`], not here.
    fn boot_and_probe<'a>(
        &'a self,
        dir: &'a Path,
        up: &'a [&'a str],
        ssh: &'a [&'a str],
        on_line: &'a mut (dyn FnMut(&str) + Send),
    ) -> BoxFuture<'a, Result<BootProbe>>;
}

/// The outcome of an elevated [`ElevatedBoot::boot_and_probe`] batch: whether the VM booted, and
/// the probe command's output when it ran.
pub struct BootProbe {
    /// The boot's outcome (`vagrant up`): `Ok` when the VM came up.
    pub boot: Result<()>,
    /// The probe's outcome (`vagrant ssh -c …`): its output, or `None` when the boot failed so the
    /// probe was skipped.
    pub probe: Option<Result<String>>,
}

impl BootProbe {
    /// A batch whose boot ended `boot` and whose probe ended `probe`.
    #[must_use]
    pub fn new(boot: Result<()>, probe: Option<Result<String>>) -> BootProbe {
        BootProbe { boot, probe }
    }
}
