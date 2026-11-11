//! The command line's grammar.

use std::path::PathBuf;

use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use isothrum_runner::isoloom_manifest::{Cloud, Provider};
use isothrum_runner::{every_name, from_wire_name};

/// Runs what Isoloom generates: the run manifest next to a target's files
/// (`<run>/.isoloom/<target>/run.json`, or `<run>/.isoloom/<target>/<cloud>/run.json`).
#[derive(Debug, Parser)]
#[command(name = "isothrum", version, about)]
pub(crate) struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub(crate) command: Command,
    /// The runner's home, where runs are recorded (default: `$ISOTHRUM_HOME`, else `~/.isothrum`).
    #[arg(long, global = true, value_name = "DIR")]
    pub(crate) home: Option<PathBuf>,
    /// Machine-readable output on standard output: an operation's events, and the ones `events`
    /// replays, one JSON object per line; `status`, `console`, `screen` and `key` print one JSON
    /// document; any failure is an `error` event. `completions` prints its script either way.
    #[arg(long, global = true)]
    pub(crate) json: bool,
}

/// What the command line asks for.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// A command on the runner: every one but `completions`.
    #[command(flatten)]
    Runner(Box<RunnerCommand>),
    /// Print the shell completion script for `shell` (e.g. `isothrum completions bash >
    /// /etc/bash_completion.d/isothrum`).
    Completions {
        /// The shell.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

/// The commands on the runner (flattened into [`Command`]: the same grammar).
#[derive(Debug, Subcommand)]
pub(crate) enum RunnerCommand {
    /// The commands that change the run's life: start, stop, park, resume, update, retire.
    #[command(flatten)]
    Lifecycle(Lifecycle),
    /// Show the run: its state, machines, outputs and message.
    Status(RunArgs),
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
    /// Hot-add the run's on-demand access machine into the running run.
    Attach {
        /// The run, and the params and secrets its operations take.
        #[command(flatten)]
        op: OpArgs,
        /// Which on-demand machine, when the run has more than one (default: its only one).
        #[arg(long)]
        machine: Option<String>,
        /// The run's network to attach it to (default: its first).
        #[arg(long)]
        network: Option<String>,
        /// What to run it from (default: the run's): a container image for a Compose access
        /// service, the Vagrant box an access VM boots.
        #[arg(long)]
        image: Option<String>,
    },
    /// Hot-remove the on-demand access machine from the running run.
    Detach {
        /// The run, and what changing it needs, as for `attach`.
        #[command(flatten)]
        op: OpArgs,
        /// Which on-demand machine, when the run has more than one (default: its only one).
        #[arg(long)]
        machine: Option<String>,
    },
    /// Print the command that opens an interactive console into any machine of the run.
    Console {
        /// The machine, by its manifest name.
        machine: String,
        /// The account to open it as, in its home, rather than the one the way in logs in as (a
        /// container's own, Vagrant's `vagrant`, a module's SSH user): the machine must have it,
        /// and on a VM that login must be allowed to `sudo`. Not on a Windows machine.
        #[arg(long)]
        user: Option<String>,
        #[command(flatten)]
        run: RunArgs,
    },
    /// Reach a machine's graphical console (its screen) and print where it answers: an address of
    /// this computer's loopback for a VNC or RDP viewer, or Proxmox's VNC websocket. One reached
    /// through an SSH tunnel is held open until the command is stopped (Ctrl-C).
    Screen {
        /// The machine, by its manifest name.
        machine: String,
        /// Print its password too (the VNC password, or the account's for RDP).
        #[arg(long)]
        show_password: bool,
        #[command(flatten)]
        run: RunArgs,
    },
    /// Print the run's recorded events, oldest first.
    Events(RunArgs),
    /// Destroy a cloud's runs past their expiry (each with the params it was applied with), then
    /// list what the cloud still holds past its expiry that no run records. Exit 1 when a destroy
    /// failed, something is left or the cloud couldn't be searched in full.
    Reap(ReapArgs),
    /// Print the runner's SSH public key (made when it has none): authorize it where a run's SSH
    /// goes through (a Proxmox node, with `--ssh-jump`). With `--json`, `{"public_key": …}`.
    Key,
}

/// The commands that change the run's life (flattened into [`RunnerCommand`]: the same grammar).
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
    /// stopped and removed (also one only moved out of the run's start).
    Retire {
        /// The machine, by its manifest name.
        machine: String,
        /// The run, and the params and secrets its operations take.
        #[command(flatten)]
        op: OpArgs,
    },
}

/// A value of Isoloom's enum `T` by the name its manifests give it (`digitalocean`,
/// `vmware_esxi`), every one Isoloom names offered ([`every_name`]).
fn wire_parser<T: serde::de::DeserializeOwned + schemars::JsonSchema + Clone + Send + Sync + 'static>() -> impl TypedValueParser<Value = T> {
    PossibleValuesParser::new(every_name::<T>()).try_map(|name| from_wire_name::<T>(&name).ok_or("not a name Isoloom gives"))
}

/// What `reap` sweeps.
#[derive(Debug, Args)]
pub(crate) struct ReapArgs {
    /// The cloud.
    #[arg(value_parser = wire_parser::<Cloud>())]
    pub(crate) cloud: Cloud,
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
pub(crate) struct RunArgs {
    /// The run's folder (the manifest Isoloom generated under its `.isoloom/`, or an instance's
    /// `.isoloom-<n>/`, is read), or a run.json.
    #[arg(default_value = ".")]
    pub(crate) path: PathBuf,
    /// The target whose manifest to run (`docker`, `cloud-vm/aws`, an instance's
    /// `.isoloom-2/docker`), when the run was generated for several.
    #[arg(long)]
    pub(crate) target: Option<String>,
    /// The name to run it under, so one manifest can run several times side by side (default:
    /// the environment's name in the manifest).
    #[arg(long)]
    pub(crate) name: Option<String>,
}

/// Which run, and the values an operation runs with.
#[derive(Debug, Args)]
pub(crate) struct OpArgs {
    #[command(flatten)]
    pub(crate) run: RunArgs,
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
    /// The Vagrant provider a VM run runs on, one of the manifest's; a run keeps the one it was
    /// started with.
    #[arg(long, value_name = "PROVIDER", value_parser = wire_parser::<Provider>())]
    pub(crate) provider: Option<Provider>,
}

/// The completion script of `shell` for the whole grammar.
pub(crate) fn completions(shell: clap_complete::Shell) -> String {
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut Cli::command(), "isothrum", &mut script);
    String::from_utf8_lossy(&script).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runner command `command` is.
    fn on_runner(command: Command) -> RunnerCommand {
        match command {
            Command::Runner(command) => *command,
            Command::Completions { .. } => panic!("not a runner command"),
        }
    }

    #[test]
    fn the_grammar_is_consistent() {
        Cli::command().debug_assert();
    }

    /// `reap` takes every cloud Isoloom names (each variant its JSON Schema lists), by that name,
    /// and only those.
    #[test]
    fn a_cloud_is_named_as_isoloom_writes_it() {
        for name in every_name::<Cloud>() {
            let asked = Cli::parse_from(["isothrum", "reap", &name]);
            assert!(
                matches!(on_runner(asked.command), RunnerCommand::Reap(ReapArgs { cloud, .. }) if isothrum_runner::wire_name(&cloud) == name),
                "{name}"
            );
        }
        let unknown = Cli::try_parse_from(["isothrum", "reap", "Aws"]).unwrap_err().to_string();
        assert!(unknown.contains("[possible values: aws, azure, gcp, digitalocean, linode, oci]"), "{unknown}");
    }

    /// Regression: `--provider` took any text, and a typo was only refused deep in the Vagrant
    /// driver. It takes a provider Isoloom names, by that name.
    #[test]
    fn a_provider_is_one_isoloom_names() {
        let up = |provider: &str| Cli::try_parse_from(["isothrum", "up", "--provider", provider]);
        for name in every_name::<Provider>() {
            assert!(up(&name).is_ok(), "{name}");
        }
        let typo = up("virtualbx").unwrap_err().to_string();
        assert!(typo.contains("invalid value 'virtualbx' for '--provider <PROVIDER>'"), "{typo}");
        assert!(typo.contains("virtualbox"), "{typo}");
    }

    #[test]
    fn every_shell_completes_every_command() {
        let asked = Cli::parse_from(["isothrum", "completions", "zsh"]);
        assert!(matches!(
            asked.command,
            Command::Completions {
                shell: clap_complete::Shell::Zsh
            }
        ));
        for shell in [
            clap_complete::Shell::Bash,
            clap_complete::Shell::Zsh,
            clap_complete::Shell::Fish,
            clap_complete::Shell::PowerShell,
        ] {
            let script = completions(shell);
            for command in ["up", "down", "status", "reap", "completions"] {
                assert!(script.contains(command), "{shell}: {command}");
            }
        }
    }

    #[test]
    fn options_parse_after_the_run_folder() {
        let cli = Cli::parse_from([
            "isothrum",
            "up",
            "runs/web",
            "--param",
            "ISOLOOM_PUBLISH_FIXED=1",
            "--secret-from-env",
            "DEMO_SECRET",
            "--provider",
            "virtualbox",
            "--json",
        ]);
        let RunnerCommand::Lifecycle(Lifecycle::Up { op, replace, .. }) = on_runner(cli.command) else {
            panic!("not `up`")
        };
        assert_eq!(op.provider, Some(Provider::Virtualbox));
        assert_eq!(
            (op.run.path, op.params, op.secrets, replace, cli.json),
            (
                "runs/web".into(),
                vec!["ISOLOOM_PUBLISH_FIXED=1".to_owned()],
                vec!["DEMO_SECRET".to_owned()],
                false,
                true
            )
        );
        let retire = Cli::parse_from(["isothrum", "retire", "web", "runs/web", "--secret-from-env", "DEMO_SECRET"]);
        let RunnerCommand::Lifecycle(Lifecycle::Retire {
            machine: retired,
            op: retiring,
        }) = on_runner(retire.command)
        else {
            panic!("not `retire`")
        };
        assert_eq!((retired.as_str(), retiring.run.path), ("web", "runs/web".into()));
        assert_eq!(retiring.secrets, ["DEMO_SECRET"]);
    }

    #[test]
    fn a_screen_takes_its_machine_and_keeps_its_password_unless_asked() {
        let screen = Cli::parse_from(["isothrum", "screen", "web"]);
        assert!(matches!(on_runner(screen.command), RunnerCommand::Screen { machine: m, show_password: false, .. } if m == "web"));
        let shown = Cli::parse_from(["isothrum", "screen", "--show-password", "web"]);
        assert!(matches!(on_runner(shown.command), RunnerCommand::Screen { show_password: true, .. }));
    }

    #[test]
    fn a_console_takes_its_machine_and_the_run_defaults_to_here() {
        let console = Cli::parse_from(["isothrum", "console", "web"]);
        assert!(matches!(on_runner(console.command), RunnerCommand::Console { machine: m, run, user: None } if m == "web" && run.path.as_os_str() == "."));
        let as_user = Cli::parse_from(["isothrum", "console", "--user", "user", "web"]);
        assert!(matches!(on_runner(as_user.command), RunnerCommand::Console { user: Some(u), .. } if u == "user"));
    }

    #[test]
    fn only_up_takes_the_ssh_jump() {
        let up = Cli::parse_from(["isothrum", "up", "--ssh-jump", "root@pve.lan"]);
        assert!(matches!(on_runner(up.command), RunnerCommand::Lifecycle(Lifecycle::Up { ssh_jump: Some(j), .. }) if j == "root@pve.lan"));
        for later in ["down", "park", "resume", "update", "provision", "attach", "detach"] {
            let given = Cli::try_parse_from(["isothrum", later, "--ssh-jump", "root@pve.lan"]);
            assert!(given.is_err(), "only `up` takes the jump the run records: {later}");
        }
    }

    #[test]
    fn park_takes_its_mode_and_the_home_comes_after_the_command() {
        let park = Cli::parse_from(["isothrum", "park", "--mode", "pause", "--home", "/tmp/h"]);
        assert_eq!(park.home, Some("/tmp/h".into()));
        assert!(matches!(
            on_runner(park.command),
            RunnerCommand::Lifecycle(Lifecycle::Park { mode: Mode::Pause, .. })
        ));
    }
}
