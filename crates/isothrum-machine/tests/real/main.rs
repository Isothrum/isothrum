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
use isothrum_machine::host::tool_path::{self, Env};
use isothrum_runner::{BoxFuture, Exit, OnLine, ProcessError, ProcessRunner, ProcessSpec, SystemProcesses, capture};

/// What a test prints to say it skipped; the rest of the line says why. `cargo xtask e2e` reads it.
const SKIP: &str = "isothrum-e2e: skip: ";

/// What a test prints, once it passed, to say the tests after it need a new login (groups it
/// joined this account to apply from it), then why. `cargo xtask e2e` reads it: the installs end,
/// then it stops and asks for the same command again in a new session.
const RELOGIN: &str = "isothrum-e2e: relogin: ";

/// What a test prints to say it waits for a person at the computer, who alone can take its next
/// step (accept a vendor's agreement in its own window: nothing accepts it for them), then for
/// what. `cargo xtask e2e` records it as waiting, which fails nothing.
const WAITING: &str = "isothrum-e2e: waiting: ";

/// What a test prints to say the computer must restart before it goes on; the rest of the line
/// says why. `cargo xtask e2e` stops there and, run again once the computer has restarted, runs
/// the test again first, [`resumed`].
const RESTART: &str = "isothrum-e2e: restart: ";

/// Says the computer must restart before the test goes on, and why (`format!`'s arguments), then
/// returns from it.
macro_rules! restart {
    ($($why:tt)*) => {{
        println!("{}{}", $crate::RESTART, format!($($why)*));
        return;
    }};
}

/// Says the test waits for a person at the computer, and for what (`format!`'s arguments), then
/// returns from it.
macro_rules! waiting {
    ($($why:tt)*) => {{
        println!("{}{}", $crate::WAITING, format!($($why)*));
        return;
    }};
}

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

/// The crate on this computer, every tool real, found where the launcher finds it ([`reread`]).
async fn here() -> Context {
    reread(&Context::here(Arc::new(SystemProcesses::default())).await).await
}

/// The crate on this computer as this process started: its tools on its own `PATH`, as a
/// launcher's are when it started before an install (a test's `PATH` is its runner's). What the
/// crate does with this context it must do without [`reread`]'s help.
async fn as_started() -> Context {
    Context::here(Arc::new(SystemProcesses::default())).await
}

/// This computer's tools, each started with `path` as its `PATH`.
struct OnPath {
    system: SystemProcesses,
    path: String,
}

impl ProcessRunner for OnPath {
    fn run<'a>(&'a self, spec: &'a ProcessSpec, on_line: OnLine<'a>) -> BoxFuture<'a, Result<Exit, ProcessError>> {
        let mut spec = spec.clone();
        spec.env.push(("PATH".to_owned(), self.path.clone()));
        Box::pin(async move { self.system.run(&spec, on_line).await })
    }
}

/// `cx` as the launcher reads this computer before each check: on Windows, its tools found on the
/// `PATH` the installers have written to the registry, read again now and merged into this
/// process's ([`tool_path::refreshed`]). A running process never reads it again by itself, and a
/// test's is its runner's, stale once an earlier test (or anything since the runner started)
/// installed a tool. Elsewhere, `cx`'s own tools.
async fn reread(cx: &Context) -> Context {
    let refreshed = tool_path::refreshed(cx, &Env::here()).await;
    let path = refreshed.and_then(|dirs| std::env::join_paths(dirs).ok()).and_then(|p| p.into_string().ok());
    let runner: Arc<dyn ProcessRunner> = match path {
        Some(path) => Arc::new(OnPath {
            system: SystemProcesses::default(),
            path,
        }),
        None => Arc::clone(&cx.runner),
    };
    Context::new(runner, cx.host.clone())
}

/// Whether the test runs again after the restart it asked for ([`restart!`]).
fn resumed() -> bool {
    setting("ISOTHRUM_E2E_RESUMED").is_some()
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
