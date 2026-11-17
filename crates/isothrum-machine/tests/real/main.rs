//! The crate against real tools and real servers, through its public API only, in two tiers:
//!
//! - `machine`: this computer's own tools: the providers' detection and readiness, the engine and
//!   hypervisor self-tests, Docker's start, the tool path, the network, the OVF Tool, the
//!   terminal, the clouds' token checks, and the tools' installs where `ISOTHRUM_E2E_INSTALL`
//!   asks for them (a clean machine, its owner there);
//! - `server`: the test servers the `ISOTHRUM_TEST_*` variables name: an SSH server, a Proxmox VE
//!   node, an ESXi host.
//!
//! Every test is ignored: `cargo xtask e2e --tier machine|server` runs them, each in a process of
//! its own, and records each as a JSON line (`--results <file>`). A test whose tool or server
//! isn't there skips, saying why (`skip!`, a line starting [`SKIP`]): it never fails for an
//! absence. Each leaves the machine and the server as it found them: what it made it removes, and
//! it starts or stops nothing it didn't start itself.

use std::sync::Arc;

use isothrum_machine::Context;
use isothrum_runner::{ProcessSpec, SystemProcesses, capture};

/// What a test prints to say it skipped; the rest of the line says why. `cargo xtask e2e` reads it.
const SKIP: &str = "isothrum-e2e: skip: ";

/// Says the test skipped, and why (`format!`'s arguments), then returns from it.
macro_rules! skip {
    ($($why:tt)*) => {{
        println!("{}{}", $crate::SKIP, format!($($why)*));
        return;
    }};
}

// Under `cfg(test)`, which the test build sets, so their helpers may unwrap and print as tests do.
#[cfg(test)]
mod machine;
#[cfg(test)]
mod server;

/// The crate on this computer, every tool real.
async fn here() -> Context {
    Context::here(Arc::new(SystemProcesses::default())).await
}

/// Setting `name`'s value: none when it is unset or empty.
fn setting(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The values of settings `names`, in order; the names of those not set, when any isn't.
fn settings<const N: usize>(names: [&str; N]) -> Result<[String; N], String> {
    let missing: Vec<&str> = names.iter().copied().filter(|n| setting(n).is_none()).collect();
    if missing.is_empty() {
        Ok(names.map(|n| setting(n).unwrap_or_default()))
    } else {
        Err(format!("not set: {}", missing.join(", ")))
    }
}

/// What `program args` prints when it runs and exits 0 on this computer; none when it doesn't.
async fn answer(cx: &Context, program: &str, args: &[&str]) -> Option<String> {
    let ran = capture(cx.runner.as_ref(), &ProcessSpec::new(program, args.iter().copied())).await.ok()?;
    ran.output().ok()
}
