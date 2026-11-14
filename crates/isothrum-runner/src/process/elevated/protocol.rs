//! What the runner and its elevated broker share: the files they talk through in the session's
//! folder, and what is elevated (the programs, and the variables a request may carry). Both sides
//! take the files' names from here: the runner's reads and writes, the broker's PowerShell (built
//! by [`super::script`] with the same names) and the tests' fake broker.

use std::fmt::Display;

/// The programs the broker runs elevated: what Hyper-V needs an administrator for. Any other
/// program of the operation runs as the account, unelevated.
pub(super) const PROGRAMS: [&str; 2] = ["vagrant", "powershell"];

/// Whether `program` is one the broker elevates.
pub(super) fn elevates(program: &str) -> bool {
    PROGRAMS.contains(&program)
}

/// The `VAGRANT_*` variables an elevated request may set: where Vagrant keeps its boxes and its
/// state, which Vagrantfile it reads, and how it reports. Any other `VAGRANT_*` is dropped.
pub(super) const VAGRANT_VARS: [&str; 8] = [
    "VAGRANT_HOME",
    "VAGRANT_CWD",
    "VAGRANT_DOTFILE_PATH",
    "VAGRANT_VAGRANTFILE",
    "VAGRANT_DEFAULT_PROVIDER",
    "VAGRANT_CHECKPOINT_DISABLE",
    "VAGRANT_NO_COLOR",
    "VAGRANT_LOG",
];

/// The variables, and the prefixes, an elevated request never sets: those that choose which code
/// the elevated program loads (its search path, Ruby's and its gems' load paths and options) or
/// where Windows finds itself.
pub(super) const REFUSED_VARS: [&str; 9] = [
    "PATH",
    "PATHEXT",
    "RUBYOPT",
    "RUBYLIB",
    "COMSPEC",
    "PSMODULEPATH",
    "SYSTEMROOT",
    "WINDIR",
    "PROGRAMFILES",
];

/// See [`REFUSED_VARS`].
pub(super) const REFUSED_PREFIXES: [&str; 3] = ["GEM_", "BUNDLE_", "RUBY"];

/// Whether an elevated request may set the variable `name` (names compared case aside, as Windows
/// reads them).
pub(super) fn passes(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    if upper.starts_with("VAGRANT_") {
        return VAGRANT_VARS.contains(&upper.as_str());
    }
    !(name.is_empty() || name.contains('=') || REFUSED_VARS.contains(&upper.as_str()) || REFUSED_PREFIXES.iter().any(|p| upper.starts_with(p)))
}

/// Written by the broker once it takes requests.
pub(super) const READY: &str = "ready";
/// Written by the runner to end the session.
pub(super) const DONE: &str = "done";
/// Written by a broker that stops other than at [`DONE`], holding why.
pub(super) const ENDED: &str = "ended";

/// Request `n`'s program, arguments and folder. `n` is a number on the runner's side and the
/// broker's counter (`$n`) in its PowerShell.
pub(super) fn request(n: impl Display) -> String {
    format!("req-{n}.json")
}

/// Request `n`'s variables.
pub(super) fn variables(n: impl Display) -> String {
    format!("env-{n}.json")
}

/// Request `n`'s output lines.
pub(super) fn output(n: impl Display) -> String {
    format!("out-{n}.log")
}

/// Request `n`'s exit code.
pub(super) fn exit(n: impl Display) -> String {
    format!("exit-{n}")
}

/// Written by the runner to cancel request `n` alone.
pub(super) fn cancel(n: impl Display) -> String {
    format!("cancel-{n}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_hyperv_s_programs_are_elevated() {
        assert!(elevates("vagrant") && elevates("powershell"));
        for other in ["ssh", "cmd", "C:\\Users\\ada\\vagrant.exe", "vagrant.exe", "Vagrant", ""] {
            assert!(!elevates(other), "{other}");
        }
    }

    #[test]
    fn a_variable_choosing_the_code_the_elevated_program_loads_never_passes() {
        for refused in [
            "PATH",
            "Path",
            "RUBYOPT",
            "rubylib",
            "RUBY_DLL_PATH",
            "GEM_HOME",
            "BUNDLE_GEMFILE",
            "VAGRANT_PLUGINS",
            "vagrant_installer_embedded_dir",
            "ComSpec",
            "",
            "A=B",
        ] {
            assert!(!passes(refused), "{refused}");
        }
        for passed in [
            "VAGRANT_HOME",
            "vagrant_dotfile_path",
            "DEMO_SECRET",
            "HTTPS_PROXY",
            "no_proxy",
            "SSL_CERT_FILE",
            "ISOLOOM_HOSTPORTS",
        ] {
            assert!(passes(passed), "{passed}");
        }
    }
}
