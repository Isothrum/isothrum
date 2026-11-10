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

use crate::{Res, process, repo};

/// A cloud.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum CloudName {
    /// Amazon Web Services.
    Aws,
    /// Microsoft Azure.
    Azure,
    /// Google Cloud.
    Gcp,
    /// DigitalOcean.
    Digitalocean,
    /// Akamai Linode.
    Linode,
    /// Oracle Cloud Infrastructure.
    Oci,
}

impl CloudName {
    /// As Isoloom's folders and the runner's command line name it (`digitalocean`).
    pub(crate) fn id(self) -> String {
        clap::ValueEnum::to_possible_value(&self).map(|v| v.get_name().to_owned()).unwrap_or_default()
    }

    /// Its smallest machine size, as the modules' size variables take it.
    pub(crate) fn smallest(self) -> &'static str {
        match self {
            CloudName::Aws => "t3.micro",
            CloudName::Azure => "Standard_B1s",
            CloudName::Gcp => "e2-micro",
            CloudName::Digitalocean => "s-1vcpu-1gb",
            CloudName::Linode => "g6-nanode-1",
            CloudName::Oci => "VM.Standard.E2.1.Micro",
        }
    }
}

/// The cloud target run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum CloudTarget {
    /// One cloud VM per machine.
    CloudVm,
    /// Docker Compose on one cloud VM.
    CloudDocker,
}

impl CloudTarget {
    /// As Isoloom names it.
    pub(crate) fn id(self) -> &'static str {
        match self {
            CloudTarget::CloudVm => "cloud-vm",
            CloudTarget::CloudDocker => "cloud-docker",
        }
    }
}

/// The arguments of `real-run cloud`.
#[derive(Debug, clap::Args)]
pub(crate) struct CloudArgs {
    /// The cloud whose account the environment's credentials open.
    #[arg(value_enum)]
    pub(crate) cloud: CloudName,
    /// The target.
    #[arg(long, value_enum, default_value_t = CloudTarget::CloudVm)]
    pub(crate) target: CloudTarget,
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
        .map_err(|missing| format!("no {} credentials in the environment: {}", a.cloud.id(), missing.join("; ")))?;
    let root = repo::root();
    let out = repo::target_dir(&root).join("real-run").join(format!("{}-{}", a.target.id(), a.cloud.id()));
    let run_dir = out.join(example);
    repo::remove(&run_dir)?;
    repo::copy_dir(&isoloom.join("examples").join(example), &run_dir).map_err(|e| format!("copying the example: {e}"))?;
    let isoloom_bin = super::build_isoloom(&root, isoloom)?;
    process::run(Command::new(isoloom_bin).args(["generate", "--target", a.target.id()]).current_dir(&run_dir))?;
    let manifest = run_dir.join(".isoloom").join(a.target.id()).join(a.cloud.id()).join("run.json");
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
            a.target.id(),
            a.cloud.id(),
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
