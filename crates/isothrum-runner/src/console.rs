//! The way into a machine as the embedder runs it: [`ShellCommand`], the command that opens a
//! console there, and the one line a POSIX shell runs it from, its words quoted as a POSIX shell
//! reads them back ([`sh_quote`], which every script the runner writes for a shell uses); the
//! account a console may be asked to open as ([`check_user`]).

use std::path::PathBuf;

use isoloom_manifest::Machine;
use serde::Serialize;

use crate::error::{Error, Result};

/// The command that opens an interactive console into a machine (a shell in a container, `vagrant
/// ssh`, an SSH login): the runner builds it (it alone knows the runtime: the Compose project and
/// files, the Vagrant define, the address and user a module gives), the embedder runs it in its
/// own terminal. Addresses the machine by service or define name, or by the address the run gives
/// it, never a guessed container.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ShellCommand {
    /// The program to run (`docker`, `vagrant`, `ssh`).
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// The folder to run it in (the run's folder).
    pub cwd: PathBuf,
    /// Variables to run it with, besides the embedder's own (a Vagrant run's state folder,
    /// `VAGRANT_DOTFILE_PATH`, without which Vagrant doesn't find the VM; its forwarded ports).
    /// Empty for a Compose service on this computer and for an SSH login.
    pub env: Vec<(String, String)>,
}

impl ShellCommand {
    /// `program` with `args`, run in `cwd`, with no variable of its own.
    #[must_use]
    pub fn new(program: impl Into<String>, args: impl IntoIterator<Item = impl Into<String>>, cwd: impl Into<PathBuf>) -> ShellCommand {
        ShellCommand {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            cwd: cwd.into(),
            env: Vec::new(),
        }
    }

    /// The same, run with `env` besides the embedder's own variables.
    #[must_use]
    pub fn with_env(mut self, env: Vec<(String, String)>) -> ShellCommand {
        self.env = env;
        self
    }

    /// The command as one line a POSIX shell runs as it is, to paste in a terminal:
    /// `cd <cwd> && VAR=value… program args…`, each word that needs it single-quoted (a `sh -c`
    /// script with `;` or `>`, a JSON value), so the shell splits it back into this argv.
    #[must_use]
    pub fn sh_line(&self) -> String {
        let cwd = sh_word(&self.cwd.display().to_string());
        let env = self.env.iter().map(|(k, v)| format!("{k}={}", sh_word(v)));
        let argv = std::iter::once(&self.program).chain(&self.args).map(|w| sh_word(w));
        let words: Vec<String> = env.chain(argv).collect();
        format!("cd {cwd} && {}", words.join(" "))
    }
}

/// Checks `user`, the account a console into `machine` is asked to open as: a name a Unix account
/// has (1 to 32 lowercase letters, digits, `_` or `-`, starting with a letter or `_`), so it goes
/// into a shell script, a `docker exec --user` or a `sudo -u` as it is; on a machine that isn't
/// Windows, whose consoles have no such account switch.
///
/// # Errors
///
/// `unsupported` for a name no account has, or a Windows machine.
pub(crate) fn check_user<'u>(user: &'u str, machine: &Machine) -> Result<&'u str> {
    let first = |c: char| c.is_ascii_lowercase() || c == '_';
    let rest = |c: char| first(c) || c.is_ascii_digit() || c == '-';
    let named = user.len() <= 32 && user.chars().next().is_some_and(first) && user.chars().all(rest);
    if !named {
        return Err(Error::Unsupported {
            what: format!("a console as `{user}`, which names no account (1 to 32 lowercase letters, digits, `_` or `-`, starting with a letter or `_`),"),
        });
    }
    if machine.windows {
        return Err(Error::Unsupported {
            what: format!("a console as `{user}` into `{}`, which runs Windows,", machine.name),
        });
    }
    Ok(user)
}

/// `word` as a POSIX shell reads it back: as it is when nothing in it is special, else
/// single-quoted.
fn sh_word(word: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_-.,/:@%+=".contains(c);
    if !word.is_empty() && word.chars().all(plain) {
        word.to_owned()
    } else {
        sh_quote(word)
    }
}

/// `s` single-quoted for a POSIX shell (`'` written `'\''`): a local terminal's line, a remote
/// shell's command, a script's argument.
pub(crate) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn a_quoted_word_reads_back_as_itself(s in ".{0,40}") {
            // `'…'` with each `'` as `'\''`: undo it the way sh reads it.
            let q = sh_quote(&s);
            let inner = q.strip_prefix('\'').and_then(|q| q.strip_suffix('\'')).unwrap();
            prop_assert_eq!(inner.replace(r"'\''", "'"), s);
        }
    }

    /// The line a shell is given splits back into the command: its folder, its variables, and
    /// each argument whole, whatever it holds.
    #[cfg(unix)]
    #[test]
    fn a_shell_line_splits_back_into_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("a run's folder");
        std::fs::create_dir(&cwd).unwrap();
        let args = [
            "-c",
            r#"printf '%s\n' "$(pwd -P)" "$J" "$@""#,
            "argv0",
            "a b",
            "x; echo no > /dev/null",
            "it's",
            "",
            "$HOME",
        ];
        let cmd = ShellCommand::new("sh", args, cwd.clone()).with_env(vec![("J".into(), r#"{"a": [1, "b c"]}"#.into())]);
        let line = cmd.sh_line();
        assert!(line.starts_with("cd '"), "{line}");
        let out = std::process::Command::new("sh").args(["-c", &line]).output().unwrap();
        let said = String::from_utf8(out.stdout).unwrap();
        let pwd = cwd.canonicalize().unwrap().display().to_string();
        let expected = [pwd.as_str(), r#"{"a": [1, "b c"]}"#, "a b", "x; echo no > /dev/null", "it's", "", "$HOME"];
        assert_eq!(said.lines().collect::<Vec<_>>(), expected, "{line}");
    }

    #[test]
    fn a_plain_word_stays_as_it_is() {
        let cmd = ShellCommand::new("vagrant", ["ssh", "web"], "/work/web-vm")
            .with_env(vec![("VAGRANT_DOTFILE_PATH".into(), "/home/u/.isothrum/runs/web-vm/vagrant".into())]);
        assert_eq!(
            (cmd.program.as_str(), cmd.args.as_slice(), cmd.cwd.as_path()),
            ("vagrant", &["ssh".to_owned(), "web".to_owned()][..], std::path::Path::new("/work/web-vm"))
        );
        assert_eq!(ShellCommand::new("docker", ["ps"], "/w").env, [], "no variable of its own until given");
        assert_eq!(
            cmd.sh_line(),
            "cd /work/web-vm && VAGRANT_DOTFILE_PATH=/home/u/.isothrum/runs/web-vm/vagrant vagrant ssh web"
        );
    }
}
