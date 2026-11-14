//! The command line's grammar.

use std::path::PathBuf;

use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use isothrum_runner::{Provider, every_name, from_wire_name};

/// Runs a project folder through its own tool: a Compose project (`docker compose`), a
/// Vagrantfile (`vagrant`) or a Terraform module (`terraform`).
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
    /// replays, one JSON object per line; `status`, `console` and `key` print one JSON document;
    /// any failure is an `error` event. `completions` prints its script either way.
    #[arg(long, global = true)]
    pub(crate) json: bool,
    /// Run the command in this process rather than through the home's runner service (also
    /// `ISOTHRUM_NO_SERVICE=1`): for tests and CI, where nothing should outlive the command.
    #[arg(long, global = true)]
    pub(crate) no_service: bool,
}

/// What the command line asks for.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// A command on the runner: every one but `completions`.
    #[command(flatten)]
    Runner(Box<RunnerCommand>),
    /// Run the home's runner service: it runs every operation of the home (one at a time per run,
    /// runs side by side) and streams their events to its clients. The other commands start it
    /// when it isn't running and talk to it (unless `--no-service`).
    Serve(ServeArgs),
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
    /// Show the run: its state, its machines and the ports they publish, and its outputs.
    Status(RunArgs),
    /// Run a command in a machine of the running run, with no terminal (`exec web -- id -u`):
    /// what it prints, and its exit status as the command's.
    Exec {
        /// The machine (a Compose service, a Vagrant machine).
        machine: String,
        #[command(flatten)]
        op: OpArgs,
        /// The command and its arguments, after `--`.
        #[arg(last = true, required = true, value_name = "ARGV")]
        argv: Vec<String>,
    },
    /// Run the set-up again on the running machines (a Vagrant project's provisioners).
    Provision {
        #[command(flatten)]
        op: OpArgs,
        /// Only this machine.
        #[arg(long)]
        machine: Option<String>,
    },
    /// Start a machine the project leaves down at `up` (a Compose service under a profile, a
    /// Vagrant machine with `autostart: false`).
    Attach {
        /// The machine.
        machine: String,
        #[command(flatten)]
        op: OpArgs,
    },
    /// Print the command that opens an interactive console into a machine of the run.
    Console {
        /// The machine (a Compose service, a Vagrant machine, a module's VM).
        machine: String,
        /// The account to open it as, in its home, rather than the one the way in logs in as (a
        /// container's own, Vagrant's `vagrant`): the machine must have it, and on a VM that
        /// login must be allowed to `sudo`.
        #[arg(long)]
        user: Option<String>,
        #[command(flatten)]
        run: RunArgs,
    },
    /// Reach a machine's graphical console (its screen) and print where it answers: an address of
    /// this computer's loopback for a VNC or RDP viewer, or Proxmox's VNC websocket. With `--app`,
    /// what opens it in its hypervisor's own app instead.
    Screen {
        /// The machine (a Vagrant machine, a module's VM).
        machine: String,
        /// Print its password too (Proxmox's ticket).
        #[arg(long)]
        show_password: bool,
        /// Print what opens it in its hypervisor's own app (a program and its arguments, a macOS
        /// app, a URL), best first.
        #[arg(long)]
        app: bool,
        /// The values and secrets the run's tools get (a Proxmox server and its credentials).
        #[command(flatten)]
        op: OpArgs,
    },
    /// Print the run's recorded events, oldest first.
    Events(RunArgs),
    /// Remove the recorded runs past their lifetime (`up --lifetime`), each from the folder it
    /// was started from. Exit 1 when a removal failed, or one is left (`--dry-run`).
    Reap(ReapArgs),
    /// Print the runner's SSH public key (made when it has none): authorize it where a run's
    /// tools reach a machine as the runner. With `--json`, `{"public_key": …}`.
    Key,
}

/// What `up` takes: the operation's arguments and the start's own.
#[derive(Debug, clap::Args)]
pub(crate) struct UpArgs {
    #[command(flatten)]
    pub(crate) op: OpArgs,
    /// How long the run may live, in minutes, 1 or more: recorded, and `reap` removes it after.
    #[arg(long, value_name = "MINUTES", value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) lifetime: Option<u64>,
}

/// The commands that change the run's life (flattened into [`RunnerCommand`]: the same grammar).
#[derive(Debug, Subcommand)]
pub(crate) enum Lifecycle {
    /// Start the run, and wait until the project says it's ready.
    Up(UpArgs),
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
    /// Bring a running run to what the project's files now say.
    Update(OpArgs),
    /// Take a machine out of the running run (a Vagrant machine destroyed, a Compose service
    /// stopped and removed).
    Retire {
        /// The machine.
        machine: String,
        /// The run, and the values and secrets its operations take.
        #[command(flatten)]
        op: OpArgs,
    },
}

/// A value of the runner's enum `T` by the name it gives it (`vmware_esxi`), every one offered
/// ([`every_name`]).
fn wire_parser<T: serde::de::DeserializeOwned + schemars::JsonSchema + Clone + Send + Sync + 'static>() -> impl TypedValueParser<Value = T> {
    PossibleValuesParser::new(every_name::<T>()).try_map(|name| from_wire_name::<T>(&name).ok_or("not a name the runner gives"))
}

/// How the service runs.
#[derive(Debug, Args)]
pub(crate) struct ServeArgs {
    /// Stop after this many seconds with no client and no operation (0: never).
    #[arg(long, value_name = "SECONDS", default_value_t = 600)]
    pub(crate) idle_timeout: u64,
    /// Run as a background service, as a client starts it: a terminal's hang-up doesn't stop it,
    /// and it logs at `info` unless `ISOTHRUM_LOG` says otherwise.
    #[arg(long)]
    pub(crate) detach: bool,
}

/// What `reap` sweeps.
#[derive(Debug, Args)]
pub(crate) struct ReapArgs {
    /// Say what would be removed, removing nothing.
    #[arg(long)]
    pub(crate) dry_run: bool,
    /// A value every removal's tools get (`NAME=VALUE`, repeatable): an account's region, say.
    #[arg(long = "env", value_name = "NAME=VALUE")]
    pub(crate) env: Vec<String>,
    /// A secret every removal's tools get (the account's keys), read from an environment
    /// variable (default: the secret's own name). Repeatable. Values never go on the command
    /// line.
    #[arg(long = "secret-from-env", value_name = "NAME[=VAR]")]
    pub(crate) secrets: Vec<String>,
}

/// A project's format, as `--format` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum FormatArg {
    /// A Compose project.
    Compose,
    /// A Vagrantfile.
    Vagrant,
    /// A Terraform module.
    Terraform,
}

impl From<FormatArg> for isothrum_runner::Format {
    fn from(f: FormatArg) -> isothrum_runner::Format {
        match f {
            FormatArg::Compose => isothrum_runner::Format::Compose,
            FormatArg::Vagrant => isothrum_runner::Format::Vagrant,
            FormatArg::Terraform => isothrum_runner::Format::Terraform,
        }
    }
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
    /// The project's folder (a file in it names its folder).
    #[arg(default_value = ".")]
    pub(crate) path: PathBuf,
    /// The project's format, when its folder holds the files of several.
    #[arg(long, value_enum)]
    pub(crate) format: Option<FormatArg>,
    /// The name to run it under, so one project can run several times side by side (default:
    /// the folder's name).
    #[arg(long)]
    pub(crate) name: Option<String>,
}

/// Which run, and the values an operation runs with.
#[derive(Debug, Args)]
pub(crate) struct OpArgs {
    #[command(flatten)]
    pub(crate) run: RunArgs,
    /// A value the project's tools get (`NAME=VALUE`, repeatable): Compose's `${NAME}`, a
    /// Vagrantfile's `ENV["NAME"]`, a Terraform variable `NAME`.
    #[arg(long = "env", value_name = "NAME=VALUE")]
    pub(crate) env: Vec<String>,
    /// A secret the project's tools get as a value is, read from an environment variable
    /// (default: the secret's own name). Repeatable. Values never go on the command line.
    #[arg(long = "secret-from-env", value_name = "NAME[=VAR]")]
    pub(crate) secrets: Vec<String>,
    /// What to call the place the run is on (recorded).
    #[arg(long, value_name = "LABEL")]
    pub(crate) host_label: Option<String>,
    /// The Vagrant provider a Vagrant project runs on.
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
            Command::Completions { .. } | Command::Serve(_) => panic!("not a runner command"),
        }
    }

    #[test]
    fn the_grammar_is_consistent() {
        Cli::command().debug_assert();
    }

    /// Regression: `--provider` took any text, and a typo was only refused deep in the Vagrant
    /// driver. It takes a provider the runner names, by that name.
    #[test]
    fn a_provider_is_one_the_runner_names() {
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

    /// Regression (#136): `--lifetime 0` was taken, and the run expired as it started: the next
    /// reap destroyed it. A lifetime is a minute or more; anything else is a usage error.
    #[test]
    fn a_lifetime_is_a_minute_or_more() {
        let up = |minutes: &str| Cli::try_parse_from(["isothrum", "up", "--lifetime", minutes]);
        for refused in ["0", "ten", ""] {
            let err = up(refused).unwrap_err();
            assert_eq!(err.exit_code(), 2, "{refused}: {err}");
            assert!(err.to_string().contains("--lifetime <MINUTES>"), "{refused}: {err}");
        }
        // A negative number reads as an unknown flag: refused all the same.
        assert_eq!(up("-5").unwrap_err().exit_code(), 2);
        let taken = on_runner(up("90").unwrap().command);
        assert!(matches!(taken, RunnerCommand::Lifecycle(Lifecycle::Up(UpArgs { lifetime: Some(90), .. }))));
    }

    #[test]
    fn options_parse_after_the_run_folder() {
        let cli = Cli::parse_from([
            "isothrum",
            "up",
            "runs/web",
            "--env",
            "ISOLOOM_KEY=k3y9ab",
            "--secret-from-env",
            "DEMO_SECRET",
            "--provider",
            "virtualbox",
            "--json",
        ]);
        let RunnerCommand::Lifecycle(Lifecycle::Up(UpArgs { op, .. })) = on_runner(cli.command) else {
            panic!("not `up`")
        };
        assert_eq!(op.provider, Some(Provider::Virtualbox));
        assert_eq!(
            (op.run.path, op.env, op.secrets, cli.json),
            ("runs/web".into(), vec!["ISOLOOM_KEY=k3y9ab".to_owned()], vec!["DEMO_SECRET".to_owned()], true)
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
    fn exec_takes_its_machine_and_the_command_after_two_dashes() {
        let exec = Cli::parse_from(["isothrum", "exec", "web", "runs/web", "--", "id", "-u"]);
        assert!(
            matches!(on_runner(exec.command), RunnerCommand::Exec { machine, op, argv } if machine == "web" && op.run.path.as_os_str() == "runs/web" && argv == ["id", "-u"])
        );
        assert!(Cli::try_parse_from(["isothrum", "exec", "web"]).is_err(), "a command is required");
    }

    #[test]
    fn a_format_names_the_project_when_its_folder_holds_several() {
        let up = Cli::parse_from(["isothrum", "up", "--format", "vagrant"]);
        assert!(matches!(on_runner(up.command), RunnerCommand::Lifecycle(Lifecycle::Up(UpArgs { op, .. })) if op.run.format == Some(FormatArg::Vagrant)));
        assert!(Cli::try_parse_from(["isothrum", "up", "--format", "docker"]).is_err());
    }

    #[test]
    fn a_console_takes_its_machine_and_the_run_defaults_to_here() {
        let console = Cli::parse_from(["isothrum", "console", "web"]);
        assert!(matches!(on_runner(console.command), RunnerCommand::Console { machine: m, run, user: None } if m == "web" && run.path.as_os_str() == "."));
        let as_user = Cli::parse_from(["isothrum", "console", "--user", "user", "web"]);
        assert!(matches!(on_runner(as_user.command), RunnerCommand::Console { user: Some(u), .. } if u == "user"));
    }

    #[test]
    fn a_screen_takes_its_machine_and_the_values_its_tools_get() {
        let screen = Cli::parse_from(["isothrum", "screen", "web", "--env", "PROXMOX_VE_ENDPOINT=https://pve:8006"]);
        assert!(
            matches!(on_runner(screen.command), RunnerCommand::Screen { machine: m, app: false, show_password: false, op } if m == "web" && op.env.len() == 1)
        );
        let app = Cli::parse_from(["isothrum", "screen", "--app", "web"]);
        assert!(matches!(on_runner(app.command), RunnerCommand::Screen { app: true, .. }));
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

    #[test]
    fn serve_takes_its_idle_time_and_any_command_runs_without_the_service_when_asked() {
        let serve = Cli::parse_from(["isothrum", "serve", "--home", "/h", "--idle-timeout", "5", "--detach"]);
        assert!(matches!(serve.command, Command::Serve(ServeArgs { idle_timeout: 5, detach: true })));
        let default = Cli::parse_from(["isothrum", "serve"]);
        assert!(matches!(
            default.command,
            Command::Serve(ServeArgs {
                idle_timeout: 600,
                detach: false
            })
        ));
        assert!(Cli::parse_from(["isothrum", "up", "--no-service"]).no_service);
    }
}
