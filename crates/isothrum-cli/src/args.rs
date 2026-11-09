//! The command line's grammar.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Runs what Isoloom generates: the run manifest next to a target's files
/// (`<run>/.isoloom/<target>/run.json`).
#[derive(Debug, Parser)]
#[command(name = "isothrum", version, about)]
pub(crate) struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub(crate) command: Command,
    /// The runner's home, where runs are recorded (default: `$ISOTHRUM_HOME`, else `~/.isothrum`).
    #[arg(long, global = true, value_name = "DIR")]
    pub(crate) home: Option<PathBuf>,
    /// Print the events as JSON, one per line, on standard output.
    #[arg(long, global = true)]
    pub(crate) json: bool,
}

/// The operations.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// The commands that change the run's life: start, stop, park, resume, update, retire.
    #[command(flatten)]
    Lifecycle(Lifecycle),
    /// Show the run: its state, machines, outputs and message.
    Status(TargetArgs),
    /// Run the run's checks (exit 1 when one fails).
    Check(OpArgs),
    /// Run the set-up again on the running machines (a VM run's provisioners).
    Provision {
        #[command(flatten)]
        op: OpArgs,
        /// Only this machine.
        #[arg(long)]
        machine: Option<String>,
    },
    /// Hot-add the lab's on-demand access machine into the running run.
    Attach {
        /// The run, and the params and secrets its operations take.
        #[command(flatten)]
        op: OpArgs,
        /// Which on-demand machine, when the lab has more than one (default: its only one).
        #[arg(long)]
        machine: Option<String>,
        /// The lab network to attach it to (default: its first).
        #[arg(long)]
        network: Option<String>,
        /// What to run it from (default: the lab's): a container image for a Compose access
        /// service, the Vagrant box an access VM boots.
        #[arg(long)]
        image: Option<String>,
    },
    /// Hot-remove the on-demand access machine from the running run.
    Detach {
        /// The run, and what changing it needs, as for `attach`.
        #[command(flatten)]
        op: OpArgs,
        /// Which on-demand machine, when the lab has more than one (default: its only one).
        #[arg(long)]
        machine: Option<String>,
    },
    /// Print the command that opens an interactive console into any machine of the run.
    Console {
        /// The machine, by its manifest name.
        machine: String,
        #[command(flatten)]
        run: TargetArgs,
    },
    /// Print the run's recorded events, oldest first.
    Events(TargetArgs),
    /// Destroy a cloud's runs past their expiry (each with the params it was applied with), then
    /// list what the cloud still holds past its expiry that no run records. Exit 1 when a destroy
    /// failed or something is left.
    Reap(ReapArgs),
    /// Print the runner's SSH public key (made when it has none): authorize it where a run's SSH
    /// goes through (a Proxmox node, with `--ssh-jump`).
    Key,
}

/// The commands that change the run's life (flattened into [`Command`]: the same grammar).
#[derive(Debug, Subcommand)]
pub(crate) enum Lifecycle {
    /// Start the run (a parked run is resumed; leftovers are removed first).
    Up {
        #[command(flatten)]
        op: OpArgs,
        /// Remove whatever of the run exists, then start afresh.
        #[arg(long)]
        replace: bool,
        /// How long a run that stops itself (a cloud run) may live, in minutes (default: its
        /// manifest's).
        #[arg(long, value_name = "MINUTES")]
        lifetime: Option<u64>,
        /// A login (`user@host`) the run's SSH goes through first: a Proxmox node whose run bridge
        /// this machine can't route to. Recorded when the run starts: every later operation (a
        /// resume through `up` too) goes the same way.
        #[arg(long, value_name = "USER@HOST")]
        ssh_jump: Option<String>,
    },
    /// Remove everything the run created.
    Down(OpArgs),
    /// Stop the run, keeping what it created.
    Park {
        #[command(flatten)]
        op: OpArgs,
        /// `pause` or `shutdown`.
        #[arg(long, value_enum, default_value_t = Mode::Shutdown)]
        mode: Mode,
    },
    /// Start a parked run again.
    Resume(OpArgs),
    /// Bring a running run up to date with its manifest, regenerated since it started (a
    /// machine added or taken out): its start steps again, with the params it was started with.
    Update(OpArgs),
    /// Take a machine out of the running run, ahead of its removal from the manifest and the
    /// `update` after.
    ///
    /// What the update wouldn't remove itself: a Vagrant VM is destroyed, a Compose service
    /// stopped and removed (also one only moved out of the lab's start).
    Retire {
        /// The machine, by its manifest name.
        machine: String,
        /// The run, and the params and secrets its operations take.
        #[command(flatten)]
        op: OpArgs,
    },
}

/// A cloud.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
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
    /// As the command line writes it (`digitalocean`).
    pub(crate) fn id(self) -> String {
        self.to_possible_value().map(|v| v.get_name().to_owned()).unwrap_or_default()
    }
}

impl From<CloudName> for isothrum_runner::isoloom_manifest::Cloud {
    fn from(c: CloudName) -> Self {
        use isothrum_runner::isoloom_manifest::Cloud;
        match c {
            CloudName::Aws => Cloud::Aws,
            CloudName::Azure => Cloud::Azure,
            CloudName::Gcp => Cloud::Gcp,
            CloudName::Digitalocean => Cloud::Digitalocean,
            CloudName::Linode => Cloud::Linode,
            CloudName::Oci => Cloud::Oci,
        }
    }
}

/// What `reap` sweeps.
#[derive(Debug, Args)]
pub(crate) struct ReapArgs {
    /// The cloud.
    #[arg(value_enum)]
    pub(crate) cloud: CloudName,
    /// Say what would be destroyed, destroying nothing.
    #[arg(long)]
    pub(crate) dry_run: bool,
    /// An AWS region to look for leftovers in (repeatable; default: the AWS command line's).
    #[arg(long = "region", value_name = "REGION")]
    pub(crate) regions: Vec<String>,
    /// The Google Cloud project to look for leftovers in (default: gcloud's).
    #[arg(long, value_name = "PROJECT")]
    pub(crate) project: Option<String>,
    /// A connection slot's value (the account's keys), read from an environment variable
    /// (default: the slot's own name). Repeatable. Values never go on the command line.
    #[arg(long = "secret-from-env", value_name = "NAME[=VAR]")]
    pub(crate) secrets: Vec<String>,
}

/// How a run is parked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Mode {
    /// Suspended where the target can.
    Pause,
    /// Shut down.
    Shutdown,
}

/// Which run.
#[derive(Debug, Args)]
pub(crate) struct TargetArgs {
    /// The run's folder (its `.isoloom/<target>/run.json` is read), or a run.json.
    #[arg(default_value = ".")]
    pub(crate) path: PathBuf,
    /// The target whose manifest to run, when the run was generated for several.
    #[arg(long)]
    pub(crate) target: Option<String>,
    /// The run's name (default: the run's name).
    #[arg(long)]
    pub(crate) name: Option<String>,
}

/// Which run, and the values an operation runs with.
#[derive(Debug, Args)]
pub(crate) struct OpArgs {
    #[command(flatten)]
    pub(crate) run: TargetArgs,
    /// A param's value (repeatable).
    #[arg(long = "param", value_name = "NAME=VALUE")]
    pub(crate) params: Vec<String>,
    /// A secret or input slot's value, read from an environment variable (default: the slot's
    /// own name). Repeatable. Values never go on the command line.
    #[arg(long = "secret-from-env", value_name = "NAME[=VAR]")]
    pub(crate) secrets: Vec<String>,
    /// What to call the place the run is on (recorded; a later operation elsewhere is refused).
    #[arg(long, value_name = "LABEL")]
    pub(crate) host_label: Option<String>,
    /// The Vagrant provider a VM run runs on (`virtualbox`, `libvirt`, `hyperv`, …); a run keeps
    /// the one it was started with.
    #[arg(long, value_name = "PROVIDER")]
    pub(crate) provider: Option<String>,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_grammar_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn options_parse_after_the_lab() {
        let cli = Cli::parse_from([
            "isothrum",
            "up",
            "labs/flag",
            "--param",
            "ISOLOOM_PUBLISH_FIXED=1",
            "--secret-from-env",
            "CTF_FLAG_MAIN",
            "--provider",
            "virtualbox",
            "--json",
        ]);
        let Command::Lifecycle(Lifecycle::Up { op, replace, .. }) = cli.command else {
            panic!("{cli:?}")
        };
        assert_eq!(op.provider.as_deref(), Some("virtualbox"));
        assert_eq!(
            (op.run.path, op.params, op.secrets, replace, cli.json),
            (
                "labs/flag".into(),
                vec!["ISOLOOM_PUBLISH_FIXED=1".to_owned()],
                vec!["CTF_FLAG_MAIN".to_owned()],
                false,
                true
            )
        );
        let retire = Cli::parse_from(["isothrum", "retire", "web", "labs/flag", "--secret-from-env", "CTF_FLAG_MAIN"]);
        let Command::Lifecycle(Lifecycle::Retire {
            machine: retired,
            op: retiring,
        }) = retire.command
        else {
            panic!("{retire:?}")
        };
        assert_eq!((retired.as_str(), retiring.run.path), ("web", "labs/flag".into()));
        assert_eq!(retiring.secrets, ["CTF_FLAG_MAIN"]);
        let console = Cli::parse_from(["isothrum", "console", "web"]);
        assert!(matches!(console.command, Command::Console { machine: m, run } if m == "web" && run.path.as_os_str() == "."));
        let up = Cli::parse_from(["isothrum", "up", "--ssh-jump", "root@pve.lan"]);
        assert!(matches!(up.command, Command::Lifecycle(Lifecycle::Up { ssh_jump: Some(j), .. }) if j == "root@pve.lan"));
        for later in ["down", "park", "resume", "update", "provision", "attach", "detach"] {
            let given = Cli::try_parse_from(["isothrum", later, "--ssh-jump", "root@pve.lan"]);
            assert!(given.is_err(), "only `up` takes the jump the run records: {later}");
        }
        let park = Cli::parse_from(["isothrum", "park", "--mode", "pause", "--home", "/tmp/h"]);
        assert!(matches!(park.command, Command::Lifecycle(Lifecycle::Park { mode: Mode::Pause, .. })) && park.home == Some("/tmp/h".into()));
    }
}
