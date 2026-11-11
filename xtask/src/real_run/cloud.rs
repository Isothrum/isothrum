//! `real-run cloud <cloud>`: an Isoloom example (solo-web) on a real cloud account through the
//! runner's command line: up, status, check, down, then `reap`, on a budget (the cloud's
//! smallest size where the module takes one, a short life the cloud itself enforces, the
//! firewall open to this machine only, a wall-clock budget after which nothing more starts) and
//! with the destroy run whatever happened. A phase already running is never killed: a Terraform
//! cut short loses what it created from its state, which no destroy then removes; each step has
//! its own time limit in the manifest. It refuses to start without the account's credentials in
//! the environment and never runs in the tests: [`account()`], [`phases()`] and [`execute`] are the
//! plan, tested on fakes; [`run`] is the shell around them.

mod account;
mod phases;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

pub(crate) use account::{Account, account};
pub(crate) use phases::{Ran, Runs, execute, phases};

use clap::builder::{PossibleValuesParser, TypedValueParser};
use isothrum_runner::isoloom_manifest::contract::OUTPUT_DIR;
use isothrum_runner::isoloom_manifest::{Cloud, Target};
use isothrum_runner::{MANIFEST_FILE, wire_name};

use crate::{Res, process, repo};

/// The targets a real run on a cloud runs, in the order the help lists them.
const TARGETS: [Target; 2] = [Target::CloudVm, Target::CloudDocker];

/// Every cloud Isoloom's manifests name, in the order the help lists them (a test holds it to
/// Isoloom's: a cloud added there fails it until it is here, and [`smallest`] until it has a size).
const CLOUDS: [Cloud; 6] = [Cloud::Aws, Cloud::Azure, Cloud::Gcp, Cloud::Digitalocean, Cloud::Linode, Cloud::Oci];

/// A cloud, by the name Isoloom's manifests give it (`digitalocean`), as the runner's command
/// line takes it.
fn cloud_parser() -> impl TypedValueParser<Value = Cloud> {
    PossibleValuesParser::new(CLOUDS.map(|c| wire_name(&c))).try_map(|name| serde_json::from_value::<Cloud>(serde_json::Value::String(name)))
}

/// A cloud target, by Isoloom's name (`cloud-vm`).
fn target_parser() -> impl TypedValueParser<Value = Target> {
    PossibleValuesParser::new(TARGETS.map(|t| wire_name(&t))).try_map(|name| serde_json::from_value::<Target>(serde_json::Value::String(name)))
}

/// `cloud`'s smallest machine size, as the modules' size variables take it.
pub(crate) fn smallest(cloud: Cloud) -> &'static str {
    match cloud {
        Cloud::Aws => "t3.micro",
        Cloud::Azure => "Standard_B1s",
        Cloud::Gcp => "e2-micro",
        Cloud::Digitalocean => "s-1vcpu-1gb",
        Cloud::Linode => "g6-nanode-1",
        Cloud::Oci => "VM.Standard.E2.1.Micro",
    }
}

/// The arguments of `real-run cloud`.
#[derive(Debug, clap::Args)]
pub(crate) struct CloudArgs {
    /// The cloud whose account the environment's credentials open.
    #[arg(value_parser = cloud_parser())]
    pub(crate) cloud: Cloud,
    /// The target.
    #[arg(long, value_parser = target_parser(), default_value = "cloud-vm")]
    pub(crate) target: Target,
    /// How long up, status and check may take together, in minutes: after it nothing more
    /// starts but the destroy.
    #[arg(long, default_value_t = 45)]
    pub(crate) budget_minutes: u64,
    /// How long the run may live before the cloud's own guard (and the reaper) ends it, in minutes.
    #[arg(long, default_value_t = 60)]
    pub(crate) lifetime_minutes: u64,
    /// This host, as Isoloom's `cargo xtask tiers record` names it, for the command printed
    /// after a pass (default: `<host>`, to fill in).
    #[arg(long)]
    pub(crate) host: Option<String>,
}

/// The example run on a cloud when `--example` names none.
pub(crate) const EXAMPLE: &str = "solo-web";

/// The run's name, its own so it never meets a run started by hand.
pub(crate) const NAME: &str = "isothrum-real-run";

/// Runs `a` on Isoloom's `example` with the Isoloom checkout `isoloom`: refused without
/// credentials; else the example generated, then each phase, the destroy whatever happened.
pub(crate) fn run(a: &CloudArgs, example: &str, isoloom: &Path) -> Res<()> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let home_dir = std::env::home_dir();
    let exists = |p: &Path| p.exists();
    let account = account(a.cloud, &var, &exists, home_dir.as_deref())
        .map_err(|missing| format!("no {} credentials in the environment: {}", wire_name(&a.cloud), missing.join("; ")))?;
    let root = repo::root();
    let out = repo::target_dir(&root)
        .join("real-run")
        .join(format!("{}-{}", wire_name(&a.target), wire_name(&a.cloud)));
    let run_dir = super::generate_example(&root, isoloom, example, &wire_name(&a.target), &out)?;
    let manifest = run_dir
        .join(OUTPUT_DIR)
        .join(wire_name(&a.target))
        .join(wire_name(&a.cloud))
        .join(MANIFEST_FILE);
    let declared = std::fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let cidr = match var("ALLOWED_CIDR") {
        Some(c) => c,
        None => format!("{}/32", account::public_ip()?),
    };
    let bin = build_isothrum(&root)?;
    let plan = phases(a, &account, &declared, &cidr, (&run_dir, &out.join("home")))?;
    let started = Instant::now();
    let outcome = execute(&plan, Duration::from_secs(a.budget_minutes * 60), &mut Real { bin }, &|| started.elapsed());
    for (phase, ran) in &outcome {
        println!("real-run: {:<6} {ran:?}", phase.step.name());
    }
    if outcome.iter().all(|(_, r)| *r == Ran::Ok) {
        println!(
            "real-run: passed. Record it in Isoloom:\n  cargo xtask tiers record {}/{} {} pass --note \"{}: up, status, check, down and reap through the Isothrum runner\"",
            wire_name(&a.target),
            wire_name(&a.cloud),
            a.host.as_deref().unwrap_or("<host>"),
            example
        );
        Ok(())
    } else {
        Err("the real run failed (the run was destroyed; see the phases above)".into())
    }
}

/// Builds the runner's command line; its path.
fn build_isothrum(root: &Path) -> Res<PathBuf> {
    process::run(process::cargo(root).args(["build", "-p", "isothrum-cli"]))?;
    Ok(repo::target_dir(root)
        .join("debug")
        .join(if cfg!(windows) { "isothrum.exe" } else { "isothrum" }))
}

/// The runner's command line, run for real.
struct Real {
    bin: PathBuf,
}

impl Runs for Real {
    fn run(&mut self, args: &[String]) -> Ran {
        ran(Command::new(&self.bin).args(args).status())
    }
}

/// How a phase went, from how its command ended: a failure says why (its exit status, or why it
/// couldn't start).
fn ran(ended: std::io::Result<ExitStatus>) -> Ran {
    match ended {
        Ok(status) if status.success() => Ran::Ok,
        Ok(status) => Ran::Failed(status.to_string()),
        Err(e) => Ran::Failed(format!("couldn't start: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phase_that_could_not_start_says_why() {
        assert_eq!(
            ran(Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"))),
            Ran::Failed("couldn't start: no such file".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_phase_that_failed_says_its_exit_status() {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(ran(Ok(ExitStatus::from_raw(0))), Ran::Ok);
        assert_eq!(ran(Ok(ExitStatus::from_raw(3 << 8))), Ran::Failed("exit status: 3".into()));
    }
}
