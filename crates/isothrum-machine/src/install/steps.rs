//! Running install and removal steps: one program each, through the [`Context`]'s runner, every
//! line it prints handed on as it comes, and what its output says beyond its exit code read on the
//! way: pkexec's own refusals told apart from the script's failures, `sudo -n` refusing to run
//! without a password, a broken Homebrew.

use std::sync::Mutex;

use isothrum_runner::{ProcessError, ProcessSpec, Stream};

use crate::context::Context;

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
    /// Homebrew downloads the tool's installer, which then opens in its own window (macOS).
    Downloading(super::Dependency),
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
    /// The program ran and failed: its exit code, `None` when a signal ended it.
    Exited {
        /// The program.
        program: String,
        /// Its exit code.
        code: Option<i32>,
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
            StepFailed::Exited { program, code: Some(code) } => write!(f, "`{program}` exited with code {code}"),
            StepFailed::Exited { program, code: None } => write!(f, "`{program}` was ended by a signal"),
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
/// # Errors
///
/// [`StepFailed`]: why it failed.
pub async fn run_step(cx: &Context, step: &Step, on_line: &mut (dyn FnMut(&str) + Send)) -> Result<(), StepFailed> {
    let spec = ProcessSpec::new(&step.program, step.args.iter().map(String::as_str));
    let watch = Mutex::new(Watch::new(step));
    let exit = {
        let mut each = |stream: Stream, line: &str| {
            let mut watched = watch.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
}

impl Watch {
    fn new(step: &Step) -> Watch {
        Watch {
            program: Program::of(step),
            pkexec_refusal: None,
            sudo_refused: false,
            brew: is_brew_step(step),
            brew_trouble: None,
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
    fn end(self, step: &Step, code: Option<i32>) -> Result<(), StepFailed> {
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
            return Ok(());
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
        Err(StepFailed::Exited {
            program: step.program.clone(),
            code,
        })
    }
}

#[cfg(test)]
mod tests;
