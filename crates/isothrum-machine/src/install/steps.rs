//! Running install and removal steps: one program each, through the [`Context`]'s runner, every
//! line it prints handed on as it comes, and what its output says beyond its exit code read on the
//! way: pkexec's own refusals told apart from the script's failures, `sudo -n` refusing to run
//! without a password, a broken Homebrew.

use std::sync::Mutex;

use isothrum_runner::{ProcessError, ProcessSpec, Stream};

use crate::context::Context;
use crate::host::tool_path::{self, Env};

/// One command of an install or removal.
#[derive(Debug)]
#[non_exhaustive]
pub struct Step {
    /// The program to run.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// What a UI shows before it instead of its command line, when it says one.
    pub note: Option<Note>,
    /// A private folder of files the step reads (a root step's proxy settings), removed with
    /// the step.
    pub(crate) private: Option<tempfile::TempDir>,
    /// What the step's script finds this system isn't, when it exits with [`NOT_SERVED`] before
    /// changing anything.
    pub(crate) not_served: Option<NotServed>,
}

/// The exit code of a step's script that stops before changing anything because this system isn't
/// one it serves ([`Step`]'s `not_served` says which).
pub(crate) const NOT_SERVED: i32 = 69;

/// What a step found this system isn't, before it changed anything. A UI says each in its own
/// words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NotServed {
    /// Docker's install serves Debian, Ubuntu and the distributions built on them; elsewhere its
    /// user installs Docker Engine from their distribution or Docker's documentation.
    DockerDistribution,
    /// This edition of Windows (Home) has no Hyper-V to turn on.
    HypervEdition,
}

impl Step {
    /// This step, its script exiting with [`NOT_SERVED`] when this system isn't one it serves.
    pub(crate) fn not_served(mut self, why: NotServed) -> Step {
        self.not_served = Some(why);
        self
    }
}

/// A step that runs `program` with `args`.
#[must_use]
pub fn step(program: impl Into<String>, args: &[&str]) -> Step {
    Step {
        program: program.into(),
        args: args.iter().map(ToString::to_string).collect(),
        note: None,
        private: None,
        not_served: None,
    }
}

/// What a step is, said before it instead of its command line (a UI words it): an install that
/// opens its vendor's own installer, a step that raises an administrator's prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Note {
    /// The tool installed from its vendor's disk image or package, which Homebrew downloads, as
    /// an administrator (macOS asks for the user's password): its app copied into Applications or
    /// its package installed, each checked as signed by its vendor. No window is left to finish.
    InstallingFromVendor {
        /// The tool.
        dependency: super::Dependency,
        /// The licence the install accepts on the user's behalf, when it does (Docker Desktop's
        /// unattended install accepts Docker's): a UI shows it, and asks their consent, before
        /// the step runs.
        accepts: Option<Licence>,
    },
    /// QEMU's install, Homebrew updated first if it is out of date for this macOS.
    QemuUpdatingHomebrewFirst,
    /// The Azure CLI's Homebrew tap trusted (Homebrew 7 asks for it; older ones don't).
    TrustingAzureTap,
    /// The prebuilt Azure CLI cask installed (no long source build).
    AzureCliCask,
    /// HashiCorp's Vagrant VMware Utility downloaded, at this version.
    DownloadingVmwareUtility {
        /// Its version.
        version: &'static str,
    },
    /// The tool removed by its vendor's own uninstaller or by a script, macOS asking for the
    /// user's password when the step is an administrator's.
    Removing(super::Dependency),
}

/// A vendor's licence an install accepts on its user's behalf ([`Note::InstallingFromVendor`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Licence {
    /// Its name, as its vendor writes it.
    pub name: &'static str,
    /// Where its vendor publishes it.
    pub url: &'static str,
}

/// How a step shows in a log: `$ program args`, as typed. A root step's proxy is never on it
/// (it is read from a private file); a caller that shows the line to someone may still redact it.
#[must_use]
pub fn command_line(step: &Step) -> String {
    format!("$ {} {}", step.program, step.args.join(" "))
}

/// Why a step failed. A UI says each in its own words.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StepFailed {
    /// pkexec isn't installed, so no password prompt can be shown (Debian ships it apart from
    /// polkit).
    NoPkexec,
    /// The step's program isn't installed.
    Missing {
        /// The program.
        program: String,
    },
    /// The program couldn't be started for another reason: what the OS said.
    NotStarted(String),
    /// pkexec's password prompt was dismissed, or the password refused: nothing was changed.
    PromptRefused,
    /// pkexec could show no password prompt: no polkit agent runs.
    NoPolkitAgent,
    /// `sudo -n` (an unattended root step) needed a password: nothing was changed.
    SudoRefused,
    /// The step's script stopped before changing anything: this system isn't one it serves.
    NotServed(NotServed),
    /// Homebrew itself is broken (a root-owned lock, too old for this macOS, an Xcode too old): the
    /// user's to repair, which an install can't mend.
    Homebrew(super::brew::Trouble),
    /// The program ran and failed: its exit code, `None` when a signal ended it, and what its codes
    /// say happened when that is known (winget's, an installer's run through PowerShell).
    Exited {
        /// The program.
        program: String,
        /// Its exit code.
        code: Option<i32>,
        /// What its codes say happened, when known.
        known: Option<super::exit_codes::Known>,
    },
}

impl std::fmt::Display for StepFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepFailed::NoPkexec => f.write_str("pkexec isn't installed: no password prompt can be shown"),
            StepFailed::Missing { program } => write!(f, "{program} isn't installed"),
            StepFailed::NotStarted(why) => write!(f, "the step couldn't be started: {why}"),
            StepFailed::PromptRefused => f.write_str("the password prompt was dismissed or the password refused"),
            StepFailed::NoPolkitAgent => f.write_str("no password prompt could be shown: no polkit agent runs"),
            StepFailed::SudoRefused => f.write_str("sudo needs a password, and none can be asked for unattended"),
            StepFailed::NotServed(why) => write!(f, "this system isn't one the step serves: {why:?}"),
            StepFailed::Homebrew(trouble) => write!(f, "Homebrew is broken: {trouble:?}"),
            StepFailed::Exited { program, code: Some(code), .. } => write!(f, "`{program}` exited with code {code}"),
            StepFailed::Exited { program, code: None, .. } => write!(f, "`{program}` was ended by a signal"),
        }
    }
}

impl std::error::Error for StepFailed {}

/// Runs `steps` in order, each without a [`Note`] announced as [`command_line`] first (a caller
/// that shows notes says them itself); the first that fails stops them.
///
/// # Errors
///
/// The first step's failure.
pub async fn run_steps(cx: &Context, steps: &[Step], on_line: &mut (dyn FnMut(&str) + Send)) -> Result<(), StepFailed> {
    for step in steps {
        if step.note.is_none() {
            on_line(&command_line(step));
        }
        run_step(cx, step, on_line).await?;
    }
    Ok(())
}

/// Runs one step, each line it prints (standard output and error) handed to `on_line`. It runs
/// in a process group of its own, like every tool of the runner: a step dropped mid-way is ended
/// with what it started, not left installing unattended.
///
/// Done, it says how when its codes say more than "done" ([`super::exit_codes::Done`]: winget's
/// "already installed", "restart to finish").
///
/// # Errors
///
/// [`StepFailed`]: why it failed.
pub async fn run_step(cx: &Context, step: &Step, on_line: &mut (dyn FnMut(&str) + Send)) -> Result<Option<super::exit_codes::Done>, StepFailed> {
    run_step_on(cx, step, &Env::here(), on_line).await
}

/// [`run_step`], this process's variables read from `env`. On Windows the step runs with the
/// `PATH` installers have written to the registry since this process started merged in
/// ([`tool_path::refreshed_value`]): a running process never reads it again, so a step after
/// an install (Vagrant's plugin after Vagrant, in one setup or a process started before it)
/// would not find the tool just installed.
pub(crate) async fn run_step_on(
    cx: &Context,
    step: &Step,
    env: &Env,
    on_line: &mut (dyn FnMut(&str) + Send),
) -> Result<Option<super::exit_codes::Done>, StepFailed> {
    let mut spec = ProcessSpec::new(&step.program, step.args.iter().map(String::as_str));
    if let Some(path) = tool_path::refreshed_value(cx, env).await {
        spec.env.push(("PATH".to_owned(), path));
    }
    let watch = Mutex::new(Watch::new(step));
    let exit = {
        let mut each = |stream: Stream, line: &str| {
            let mut watched = watch.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            watched.line(line);
            if stream == Stream::Stderr {
                watched.error_line(line);
            }
            drop(watched);
            on_line(line);
        };
        cx.runner.run(&spec, &mut each).await
    };
    let exit = match exit {
        Ok(exit) => exit,
        Err(ProcessError::Missing { .. }) if step.program == "pkexec" => return Err(StepFailed::NoPkexec),
        Err(ProcessError::Missing { program }) => return Err(StepFailed::Missing { program }),
        Err(e) => return Err(StepFailed::NotStarted(e.to_string())),
    };
    watch.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner).end(step, exit.code)
}

/// The programs whose output a step's watch reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Program {
    /// winget: the installer it ran exits with a code of its own.
    Winget,
    /// PowerShell running an installer (WSL's, Hyper-V's DISM, an MSI): exits with its code.
    Powershell,
    /// pkexec: its own refusals, told apart from the script's.
    Pkexec,
    /// `sudo -n` (an unattended root step): its refusal to run without a password.
    UnattendedSudo,
    /// Any other.
    Other,
}

/// Whether `step` drives Homebrew (directly, or through a shell line that calls it), so its output
/// is worth reading for a broken-Homebrew cause. apt, winget and pkexec never are.
fn is_brew_step(step: &Step) -> bool {
    step.program.contains("brew") || step.args.iter().any(|a| a.contains("brew"))
}

impl Program {
    /// The program `step` runs.
    fn of(step: &Step) -> Program {
        match step.program.as_str() {
            "winget" => Program::Winget,
            "powershell" => Program::Powershell,
            "pkexec" => Program::Pkexec,
            "sudo" if step.args.first().is_some_and(|a| a == "-n") => Program::UnattendedSudo,
            _ => Program::Other,
        }
    }
}

/// What a step's error output says beyond its exit code: pkexec's own refusal (told apart from
/// the script's exit code), sudo's refusal to run without a password.
#[derive(Debug)]
struct Watch {
    program: Program,
    pkexec_refusal: Option<super::pkexec::Refusal>,
    sudo_refused: bool,
    /// Whether the step drives Homebrew, and the first broken-Homebrew cause it said (an
    /// update's lock error comes before the install error it leads to, so it wins).
    brew: bool,
    brew_trouble: Option<super::brew::Trouble>,
    /// The exit code of the installer winget ran, from its output.
    installer_code: Option<u32>,
}

impl Watch {
    fn new(step: &Step) -> Watch {
        Watch {
            program: Program::of(step),
            pkexec_refusal: None,
            sudo_refused: false,
            brew: is_brew_step(step),
            brew_trouble: None,
            installer_code: None,
        }
    }

    /// A line of the step's output, either stream.
    fn line(&mut self, line: &str) {
        if self.program == Program::Winget {
            self.installer_code = super::exit_codes::installer_exit_code(line).or(self.installer_code);
        }
    }

    /// A line of the step's error output.
    fn error_line(&mut self, line: &str) {
        if self.program == Program::Pkexec
            && let Some(refusal) = super::pkexec::Refusal::of_line(line)
        {
            self.pkexec_refusal = Some(refusal);
        }
        if self.program == Program::UnattendedSudo && line.starts_with("sudo: ") && line.contains("password") {
            self.sudo_refused = true;
        }
        if self.brew && self.brew_trouble.is_none() {
            self.brew_trouble = super::brew::Trouble::of_line(line);
        }
    }

    /// How the step ended, once it exited with `code`.
    fn end(self, step: &Step, code: Option<i32>) -> Result<Option<super::exit_codes::Done>, StepFailed> {
        // pkexec exits 126 when the password prompt was dismissed or refused and 127 when no
        // polkit agent could show one, but the script's own 126/127 (a command missing or not
        // executable) come through as is: only pkexec's own message says which it was.
        if let Some(refusal) = self.pkexec_refusal.filter(|_| matches!(code, Some(126 | 127))) {
            return Err(match refusal {
                super::pkexec::Refusal::NoAgent => StepFailed::NoPolkitAgent,
                super::pkexec::Refusal::Refused => StepFailed::PromptRefused,
            });
        }
        if code == Some(0) {
            return Ok(None);
        }
        if self.program == Program::Winget
            && let Some(done) = super::exit_codes::winget_done(code)
        {
            return Ok(Some(done));
        }
        if self.sudo_refused {
            return Err(StepFailed::SudoRefused);
        }
        if let Some(why) = step.not_served.filter(|_| code == Some(NOT_SERVED)) {
            return Err(StepFailed::NotServed(why));
        }
        if let Some(trouble) = self.brew_trouble {
            return Err(StepFailed::Homebrew(trouble));
        }
        let known = match self.program {
            Program::Winget => super::exit_codes::winget_failure(code, self.installer_code),
            Program::Powershell => super::exit_codes::installer_failure(code),
            Program::Pkexec | Program::UnattendedSudo | Program::Other => None,
        };
        Err(StepFailed::Exited {
            program: step.program.clone(),
            code,
            known,
        })
    }
}

#[cfg(test)]
mod tests;
