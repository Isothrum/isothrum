//! The way into a machine as the embedder runs it: [`ShellCommand`], the command that opens a
//! console there, and the one line a POSIX shell runs it from.

use std::path::PathBuf;

use serde::Serialize;

/// The command that opens an interactive console into a machine (a shell in a container, `vagrant
/// ssh`, an SSH login): the runner builds it (it alone knows the runtime: the Compose project and
/// files, the Vagrant define, the address and user a module gives), the embedder runs it in its
/// own terminal. Addresses the machine by service or define name, or by the address the run gives
/// it, never a guessed container.
#[derive(Debug, Clone, Serialize)]
pub struct ShellCommand {
    /// The program to run (`docker`, `vagrant`, `ssh`).
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// The folder to run it in (the run's folder).
    pub cwd: PathBuf,
    /// Variables to run it with, besides the embedder's own (a Vagrant run's state folder,
    /// `VAGRANT_DOTFILE_PATH`, without which Vagrant doesn't find the VM; its forwarded ports).
    /// Empty for a Compose service and an SSH login.
    pub env: Vec<(String, String)>,
}

impl ShellCommand {
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

/// `word` as a POSIX shell reads it back: as it is when nothing in it is special, else
/// single-quoted.
fn sh_word(word: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_-.,/:@%+=".contains(c);
    if !word.is_empty() && word.chars().all(plain) {
        word.to_owned()
    } else {
        crate::ssh::sh_quote(word)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let cmd = ShellCommand {
            program: "sh".into(),
            args: args.map(str::to_owned).to_vec(),
            cwd: cwd.clone(),
            env: vec![("J".into(), r#"{"a": [1, "b c"]}"#.into())],
        };
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
        let cmd = ShellCommand {
            program: "vagrant".into(),
            args: vec!["ssh".into(), "web".into()],
            cwd: PathBuf::from("/labs/web-vm"),
            env: vec![("VAGRANT_DOTFILE_PATH".into(), "/home/u/.isothrum/runs/web-vm/vagrant".into())],
        };
        assert_eq!(
            cmd.sh_line(),
            "cd /labs/web-vm && VAGRANT_DOTFILE_PATH=/home/u/.isothrum/runs/web-vm/vagrant vagrant ssh web"
        );
    }
}
