//! A VM's screen in its hypervisor's own app, beside the embedder's viewer: which app the run's
//! hypervisor has ([`NativeApp`], in a status), and what opens a running VM in it
//! ([`NativeConsole`], [`crate::Running::native_console`]). The runner runs nothing to open it:
//! it hands the embedder a program and its arguments (never a shell line), a macOS app and what
//! it opens, or an `https` URL, each argument checked first ([`argument`]).
//!
//! Each format's driver says which app its machines open in and how; this module holds what they
//! share.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::no_console;
use crate::Result;

/// What of a hypervisor's app must be on this computer for a way to open it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "at")]
#[non_exhaustive]
pub enum NativeNeed {
    /// A program, by its name on `PATH` or its full path.
    Program(String),
    /// A macOS app bundle.
    MacApp(PathBuf),
    /// The default browser (a web console).
    Browser,
}

/// The hypervisor's own app that shows a running VM's screen in a window of its own, and what of
/// it this computer must have (any one of `needs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct NativeApp {
    /// Its name as its maker writes it (`VirtualBox`, `VMware Fusion`): what a button names.
    pub name: String,
    /// What opens it, best first: the program or app [`NativeConsole::ways`] runs, in the same
    /// order.
    pub needs: Vec<NativeNeed>,
}

impl NativeApp {
    /// App `name`, opened by `needs`.
    pub(crate) fn new(name: &str, needs: Vec<NativeNeed>) -> NativeApp {
        NativeApp { name: name.to_owned(), needs }
    }
}

/// One way to open a VM in its hypervisor's app: what to run, never through a shell.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NativeOpen {
    /// Run `program` (a name on `PATH`, or a full path) with `args`, each passed as it is.
    Program {
        /// The program.
        program: String,
        /// Its arguments.
        args: Vec<String>,
    },
    /// Open the macOS app `app` on `args` (`open -a <app> <args>…`: a file or link it opens).
    MacApp {
        /// The app bundle.
        app: PathBuf,
        /// What it opens.
        args: Vec<String>,
    },
    /// Open `url` (`https`) in the default browser.
    Url {
        /// The URL.
        url: String,
    },
}

impl NativeOpen {
    /// What of the app this way needs on this computer.
    #[must_use]
    pub fn need(&self) -> NativeNeed {
        match self {
            NativeOpen::Program { program, .. } => NativeNeed::Program(program.clone()),
            NativeOpen::MacApp { app, .. } => NativeNeed::MacApp(app.clone()),
            NativeOpen::Url { .. } => NativeNeed::Browser,
        }
    }
}

/// What opens a running VM's screen in its hypervisor's own app.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NativeConsole {
    /// The machine.
    pub machine: String,
    /// The app's name ([`NativeApp::name`]).
    pub app: String,
    /// The ways to open it, best first: the first whose program or app this computer has is
    /// the one to take.
    pub ways: Vec<NativeOpen>,
}

impl NativeConsole {
    /// `machine` opened in `app` by `ways`.
    pub(crate) fn new(machine: &str, app: &NativeApp, ways: Vec<NativeOpen>) -> NativeConsole {
        NativeConsole {
            machine: machine.to_owned(),
            app: app.name.clone(),
            ways,
        }
    }
}

/// `program` with `args`.
pub(crate) fn program<'a>(program: &str, args: impl IntoIterator<Item = &'a str>) -> NativeOpen {
    NativeOpen::Program {
        program: program.to_owned(),
        args: args.into_iter().map(str::to_owned).collect(),
    }
}

/// `program` as a need.
pub(crate) fn needs_program(program: &str) -> NativeNeed {
    NativeNeed::Program(program.to_owned())
}

/// `value` as one argument of a program: refused when it could be read as anything else (empty,
/// an option, a control character, a line break).
pub(crate) fn argument<'a>(machine: &str, value: &'a str) -> Result<&'a str> {
    if value.trim().is_empty() || value.starts_with('-') || value.chars().any(char::is_control) {
        return Err(no_console(
            machine,
            format!("its VM's name or file `{}` can't be handed to its app", value.escape_debug()),
        ));
    }
    Ok(value)
}

/// Whether `h` is a host name or an IPv4 address, nothing else (no user, path or query).
pub(crate) fn host_name(h: &str) -> bool {
    !h.is_empty() && h.len() <= 253 && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') && !h.starts_with('-')
}

/// The error a machine whose hypervisor has no app gets.
pub(crate) fn no_app(machine: &str) -> crate::error::Error {
    no_console(machine, "its hypervisor has no app of its own to show it in")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_argument_that_could_be_read_as_more_is_refused() {
        assert_eq!(argument("web", "web-vm - web").unwrap(), "web-vm - web");
        for bad in ["", "  ", "--type=gui", "a\nb", "a\u{0}b"] {
            assert!(argument("web", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_host_name_is_only_a_name() {
        assert!(host_name("172.16.1.20") && host_name("pve.lab.example"));
        for bad in ["", "-x", "a b", "user@host", "host/path", "host?q", "host#x", "h:1"] {
            assert!(!host_name(bad), "{bad}");
        }
    }
}
