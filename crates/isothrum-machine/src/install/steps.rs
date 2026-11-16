//! Running install and removal steps: one program each, through the [`Context`]'s runner, every
//! line it prints handed on as it comes, and what its output says beyond its exit code read on the
//! way: pkexec's own refusals told apart from the script's failures, `sudo -n` refusing to run
//! without a password.

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
    /// A private folder of files the step reads (a root step's proxy settings), removed with
    /// the step.
    pub(crate) private: Option<tempfile::TempDir>,
}

/// A step that runs `program` with `args`.
#[must_use]
pub fn step(program: impl Into<String>, args: &[&str]) -> Step {
    Step {
        program: program.into(),
        args: args.iter().map(ToString::to_string).collect(),
        private: None,
    }
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
            StepFailed::Exited { program, code: Some(code) } => write!(f, "`{program}` exited with code {code}"),
            StepFailed::Exited { program, code: None } => write!(f, "`{program}` was ended by a signal"),
        }
    }
}

impl std::error::Error for StepFailed {}

/// Runs `steps` in order, each announced as [`command_line`] first; the first that fails stops
/// them.
///
/// # Errors
///
/// The first step's failure.
pub async fn run_steps(cx: &Context, steps: &[Step], on_line: &mut (dyn FnMut(&str) + Send)) -> Result<(), StepFailed> {
    for step in steps {
        on_line(&command_line(step));
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
}

impl Watch {
    fn new(step: &Step) -> Watch {
        Watch {
            program: Program::of(step),
            pkexec_refusal: None,
            sudo_refused: false,
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
        Err(StepFailed::Exited {
            program: step.program.clone(),
            code,
        })
    }
}

#[cfg(test)]
mod tests;
