//! A console's command, as a list of arguments, written out for the shell of the platform's
//! terminal: POSIX `sh` (Linux, macOS) or `cmd.exe` (Windows).

use std::path::{Path, PathBuf};

/// What a console runs: a program, its arguments, and the folder it runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Console {
    pub dir: Option<PathBuf>,
    pub argv: Vec<String>,
}

impl Console {
    pub(crate) fn new(argv: &[&str]) -> Self {
        Console {
            dir: None,
            argv: argv.iter().map(std::string::ToString::to_string).collect(),
        }
    }

    pub(crate) fn in_dir(self, dir: &Path) -> Self {
        Console {
            dir: Some(dir.to_path_buf()),
            ..self
        }
    }

    /// As a POSIX shell line: `cd '<dir>' && <argv>`. Built only where a POSIX terminal runs it.
    #[cfg(not(windows))]
    pub(crate) fn sh_line(&self) -> String {
        self.line(sh_quote, |d| format!("cd {} && ", sh_quote(d)))
    }

    /// As a `cmd.exe` line: `cd /d "<dir>" && <argv>`. Built only on Windows, where `cmd` runs it.
    #[cfg(windows)]
    pub(crate) fn cmd_line(&self) -> String {
        self.line(cmd_quote, |d| format!("cd /d {} && ", cmd_quote(d)))
    }

    fn line(&self, quote: fn(&str) -> String, cd: impl Fn(&str) -> String) -> String {
        let args: Vec<String> = self.argv.iter().map(|a| quote(a)).collect();
        let cd = self
            .dir
            .as_ref()
            .map(|d| cd(&d.display().to_string()))
            .unwrap_or_default();
        format!("{cd}{}", args.join(" "))
    }
}

/// An argument for `sh`: as is when plain, else in single quotes.
#[cfg(not(windows))]
fn sh_quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./_-".contains(c));
    if plain {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

/// An argument for `cmd.exe`: as is when plain, else in double quotes, which also keep cmd's
/// operators (`&`, `|`, `>`) in the argument. A quote inside is escaped for the program (`\"`).
#[cfg(windows)]
fn cmd_quote(arg: &str) -> String {
    let plain = !arg.is_empty() && !arg.chars().any(|c| " \t\"&|<>^()%!".contains(c));
    if plain {
        arg.to_owned()
    } else {
        format!("\"{}\"", arg.replace('"', "\\\""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell_in(id: &str) -> Console {
        Console::new(&[
            "docker",
            "exec",
            "-it",
            id,
            "sh",
            "-c",
            "command -v bash >/dev/null && exec bash || exec sh",
        ])
    }

    #[cfg(not(windows))]
    #[test]
    fn posix_shells_get_single_quotes() {
        assert_eq!(
            shell_in("abc").sh_line(),
            "docker exec -it abc sh -c 'command -v bash >/dev/null && exec bash || exec sh'"
        );
        let c = Console::new(&["vagrant", "ssh", "pc"]).in_dir(Path::new("/labs/it's"));
        assert_eq!(c.sh_line(), "cd '/labs/it'\\''s' && vagrant ssh pc");
    }

    #[cfg(windows)]
    #[test]
    fn cmd_gets_double_quotes() {
        assert_eq!(
            shell_in("abc").cmd_line(),
            "docker exec -it abc sh -c \"command -v bash >/dev/null && exec bash || exec sh\""
        );
        let c = Console::new(&["vagrant", "ssh", "pc"]).in_dir(Path::new("C:\\Users\\a b\\labs"));
        assert_eq!(
            c.cmd_line(),
            "cd /d \"C:\\Users\\a b\\labs\" && vagrant ssh pc"
        );
    }
}
