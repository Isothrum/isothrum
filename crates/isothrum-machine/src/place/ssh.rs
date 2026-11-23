//! SSH to a server's own system (a Proxmox VE node, say): with a key the caller keeps at a path
//! it chooses ([`ensure_key`]), the server's host keys trusted on first use ([`known_hosts`]) and
//! checked strictly from then on. Every `ssh`, `ssh-keygen` and `ssh-keyscan` runs through the
//! context's [`ProcessRunner`](isothrum_runner::ProcessRunner).
//!
//! Which keys a user is shown and how they confirm them is the caller's: [`known_hosts::offered`]
//! reads what a server offers, [`known_hosts::confirm`] writes the ones confirmed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use isothrum_runner::ProcessSpec;

use crate::Context;
use crate::exec::Unanswered;

pub mod known_hosts;

/// How long making a key may take.
const KEYGEN_LIMIT: Duration = Duration::from_secs(60);

/// How long an unattended `ssh` may wait for its connection.
const CONNECT_TIMEOUT_S: u32 = 10;

/// Why an SSH step failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SshError {
    /// A host, user or path that isn't put on a command line (`-o…`, `a;b`, a space).
    #[error("unexpected SSH {0}")]
    Invalid(&'static str),
    /// The tool couldn't be started (OpenSSH not installed), or ran out of time: why.
    #[error("{0}")]
    Unanswered(String),
    /// The server's host key isn't one confirmed for it (none yet, or another): nothing was sent.
    #[error("the SSH host key of {host} isn't one confirmed for it")]
    HostKeyRefused {
        /// The host asked.
        host: String,
    },
    /// The tool ran and failed: the runner's wording of it.
    #[error("{0}")]
    Failed(String),
    /// A file couldn't be read or written: why.
    #[error("{0}")]
    Io(String),
}

impl From<Unanswered> for SshError {
    fn from(e: Unanswered) -> SshError {
        SshError::Unanswered(e.to_string())
    }
}

impl From<std::io::Error> for SshError {
    fn from(e: std::io::Error) -> SshError {
        SshError::Io(e.to_string())
    }
}

/// The key at `path`, made when there is none (`ssh-keygen -t ed25519`, no passphrase, `comment`
/// as its comment), and its public half, one line. A key already there is kept as it is: servers
/// that trust it keep working.
///
/// # Errors
///
/// `ssh-keygen` couldn't be run or failed, or the public half can't be read.
pub async fn ensure_key(ctx: &Context, path: &Path, comment: &str) -> Result<String, SshError> {
    if !path.is_file() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = path.to_string_lossy();
        let spec = ProcessSpec::new("ssh-keygen", ["-q", "-t", "ed25519", "-N", "", "-C", comment, "-f", &file]);
        let made = ctx.probe_within(KEYGEN_LIMIT, &spec).await?;
        made.output().map_err(|f| SshError::Failed(f.to_string()))?;
    }
    Ok(std::fs::read_to_string(public_half(path))?.trim().to_owned())
}

/// Where the public half of the key at `path` is (`<path>.pub`).
#[must_use]
pub fn public_half(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".pub");
    PathBuf::from(name)
}

/// Where to SSH, and as whom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The address to connect to.
    pub host: String,
    /// Its SSH port.
    pub port: u16,
    /// The user to sign in as.
    pub user: String,
    /// The private key to sign in with.
    pub identity: PathBuf,
    /// The host keys confirmed for it ([`known_hosts`]): the only ones accepted.
    pub known_hosts: PathBuf,
    /// A password to sign in with instead of the key, answered once to ssh's prompt
    /// ([`Target::with_password`]); none to sign in with the key.
    pub password: Option<Password>,
}

/// A password ssh is answered with, through its askpass hook: a script written into `askpass_dir`
/// (the caller's own folder, never a shared temporary one) that prints the password ssh is given
/// in its environment. Never shown by `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct Password {
    /// The password.
    pub secret: String,
    /// The folder the askpass script is written to.
    pub askpass_dir: PathBuf,
}

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Password").field("askpass_dir", &self.askpass_dir).finish_non_exhaustive()
    }
}

/// The variable the askpass script reads the password from.
const PASSWORD_VAR: &str = "ISOTHRUM_SSH_PASSWORD";

/// The askpass script's file name and contents for this computer's ssh: a `sh` script, or on
/// Windows (whose OpenSSH can't run one) a `.cmd` printing the password after the line is parsed
/// (delayed expansion), so `&`, `|` or `%` in it are printed, not run.
fn askpass_script(windows: bool) -> (&'static str, &'static str) {
    if windows {
        (
            "askpass.cmd",
            "@echo off\r\nsetlocal EnableDelayedExpansion\r\necho(!ISOTHRUM_SSH_PASSWORD!\r\n",
        )
    } else {
        ("askpass.sh", "#!/bin/sh\nprintf '%s\\n' \"$ISOTHRUM_SSH_PASSWORD\"\n")
    }
}

/// Writes the askpass script into `dir`, runnable by its owner only (on unix), and returns its path.
fn write_askpass(dir: &Path) -> std::io::Result<PathBuf> {
    let (name, script) = askpass_script(cfg!(windows));
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    std::fs::write(&path, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(path)
}

/// A user or host put on a command line: plain characters only, never an option.
pub(crate) fn safe_token(s: &str) -> bool {
    !s.is_empty() && s.len() <= 253 && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || ".-_:".contains(c))
}

/// `s` single-quoted for a POSIX shell (the remote command's).
#[must_use]
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A path as the value of an ssh `-o Key=value` option. OpenSSH splits option values into words
/// (on whitespace, honouring quotes and backslash escapes) and expands `%` tokens in file paths:
/// unescaped, a path with a space (`~/Library/Application Support/…`) became two files. Escaped
/// here so ssh reads the path back verbatim (a tab can't be escaped this way).
#[must_use]
pub fn config_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            '\\' | '"' | '\'' | ' ' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out
}

/// The `-o` values of an SSH that runs unattended: only the host keys in `known_hosts` are
/// accepted (a host with none or another is refused, never pinned silently), no prompt, a short
/// connect timeout, and keepalives, so a server that stops answering mid-command ends it in under
/// a minute.
#[must_use]
pub fn batch_options(known_hosts: &Path) -> Vec<String> {
    vec![
        "BatchMode=yes".to_owned(),
        format!("ConnectTimeout={CONNECT_TIMEOUT_S}"),
        "ServerAliveInterval=15".to_owned(),
        "ServerAliveCountMax=3".to_owned(),
        "StrictHostKeyChecking=yes".to_owned(),
        format!("UserKnownHostsFile={}", config_path(&known_hosts.to_string_lossy())),
        "LogLevel=ERROR".to_owned(),
    ]
}

impl Target {
    /// `user@host` on port 22, signed in with `identity`, its host keys checked against
    /// `known_hosts`.
    #[must_use]
    pub fn new(host: impl Into<String>, user: impl Into<String>, identity: PathBuf, known_hosts: PathBuf) -> Target {
        Target {
            host: host.into(),
            port: 22,
            user: user.into(),
            identity,
            known_hosts,
            password: None,
        }
    }

    /// The same, signing in with `secret` rather than the key, ssh's prompt answered by a script
    /// written into `askpass_dir`.
    #[must_use]
    pub fn with_password(self, secret: impl Into<String>, askpass_dir: PathBuf) -> Target {
        Target {
            password: Some(Password {
                secret: secret.into(),
                askpass_dir,
            }),
            ..self
        }
    }

    /// The same on `port`.
    #[must_use]
    pub fn on_port(self, port: u16) -> Target {
        Target { port, ..self }
    }

    /// The `ssh` arguments that run `command` on it (no local shell in between).
    ///
    /// # Errors
    ///
    /// [`SshError::Invalid`] for a host or user that isn't a plain word.
    pub fn exec_args(&self, command: &str) -> Result<Vec<String>, SshError> {
        if !safe_token(&self.host) {
            return Err(SshError::Invalid("host"));
        }
        if !safe_token(&self.user) {
            return Err(SshError::Invalid("user"));
        }
        let mut args = vec!["-p".to_owned(), self.port.to_string()];
        let options = if self.password.is_some() {
            // A password is asked through askpass, which batch mode turns off: only the password
            // is tried, once.
            let mut options = vec![
                "PreferredAuthentications=password,keyboard-interactive".to_owned(),
                "NumberOfPasswordPrompts=1".to_owned(),
            ];
            options.extend(batch_options(&self.known_hosts).into_iter().filter(|o| o != "BatchMode=yes"));
            options
        } else {
            args.splice(0..0, ["-i".to_owned(), self.identity.to_string_lossy().into_owned()]);
            batch_options(&self.known_hosts)
        };
        for option in options {
            args.extend(["-o".to_owned(), option]);
        }
        args.extend(["--".to_owned(), format!("{}@{}", self.user, self.host), command.to_owned()]);
        Ok(args)
    }

    /// Runs `command` on it, within `limit`, and returns its standard output.
    ///
    /// # Errors
    ///
    /// [`SshError::HostKeyRefused`] when ssh refused the server's host key; else why it didn't
    /// answer or failed.
    pub async fn exec(&self, ctx: &Context, command: &str, limit: Duration) -> Result<String, SshError> {
        let mut spec = ProcessSpec::new("ssh", self.exec_args(command)?);
        if let Some(password) = &self.password {
            let askpass = write_askpass(&password.askpass_dir)?;
            spec.env.extend([
                ("SSH_ASKPASS".to_owned(), askpass.to_string_lossy().into_owned()),
                ("SSH_ASKPASS_REQUIRE".to_owned(), "force".to_owned()),
                ("DISPLAY".to_owned(), ":0".to_owned()),
                (PASSWORD_VAR.to_owned(), password.secret.clone()),
            ]);
        }
        let captured = ctx.probe_within(limit, &spec).await?;
        if !captured.success() && captured.stderr_tail.iter().any(|l| l.contains("Host key verification failed")) {
            return Err(SshError::HostKeyRefused { host: self.host.clone() });
        }
        captured.output().map_err(|f| SshError::Failed(f.to_string()))
    }
}

#[cfg(test)]
mod tests;
