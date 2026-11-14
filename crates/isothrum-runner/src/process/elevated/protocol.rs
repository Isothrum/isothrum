//! What the runner and its elevated broker share: the files they talk through in the session's
//! folder, and what is elevated (the programs, and the variables a request may carry). Both sides
//! take them from here: the runner's reads and writes, the broker's PowerShell (built by
//! [`super::script`] from the same names and lists, so the two can't drift apart) and the tests'
//! fake broker.

use std::fmt::Display;

/// The programs the broker runs elevated: what Hyper-V needs an administrator for. Each is found
/// by the broker itself at an administrator-owned path ([`super::script`]), never on a `PATH`;
/// any other program of the operation runs as the account, unelevated.
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

/// The variables that say where Windows and its programs are: the broker's own, which a request
/// never sets (the program's search path among them: the broker sets it).
pub(super) const SYSTEM_VARS: [&str; 7] = ["PATH", "PATHEXT", "COMSPEC", "PSMODULEPATH", "SYSTEMROOT", "WINDIR", "PROGRAMFILES"];

/// The prefixes of the variables that choose which code Vagrant's Ruby loads (Ruby's load path
/// and options, its gems' and Bundler's settings): a request never sets one, and the broker's
/// programs never inherit one.
pub(super) const LOADER_PREFIXES: [&str; 3] = ["GEM_", "BUNDLE_", "RUBY"];

/// Whether the variable `name` chooses which code the elevated program loads: a loader variable
/// ([`LOADER_PREFIXES`]) or a `VAGRANT_*` not in [`VAGRANT_VARS`] (names compared case aside, as
/// Windows reads them).
pub(super) fn loads_code(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    if upper.starts_with("VAGRANT_") {
        return !VAGRANT_VARS.contains(&upper.as_str());
    }
    LOADER_PREFIXES.iter().any(|p| upper.starts_with(p))
}

/// Whether an elevated request may set the variable `name`: neither one of the broker's own
/// ([`SYSTEM_VARS`]) nor one that chooses which code the program loads ([`loads_code`]).
pub(super) fn passes(name: &str) -> bool {
    !(name.is_empty() || name.contains('=') || SYSTEM_VARS.contains(&name.to_ascii_uppercase().as_str()) || loads_code(name))
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

    #[test]
    fn only_the_loaders_variables_are_taken_from_what_the_broker_inherits() {
        assert!(loads_code("RubyOpt") && loads_code("GEM_PATH") && loads_code("VAGRANT_PLUGINS"));
        for kept in ["VAGRANT_HOME", "SystemRoot", "PATH", "ComSpec", "TEMP"] {
            assert!(!loads_code(kept), "{kept}: Windows' own stay the program's");
        }
    }
}
