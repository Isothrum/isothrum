//! The runner's own SSH key (`<home>/ssh/id_ed25519`), made on first use: what an embedder
//! authorizes where a run's tools reach a machine over SSH as the runner ([`crate::Runner::ssh_public_key`]).
//!
//! And the runner's own SSH calls to a run's machines (a console's tunnel): with a key (the run's,
//! else the runner's) and the run's own `known_hosts`, in its state folder, never the user's
//! files (`-F none`, no global known hosts). A host's key is pinned there the first time the run
//! meets it ([`pin`]: `ssh-keyscan`, then written), and every call checks it strictly
//! (`StrictHostKeyChecking=yes`, never `accept-new`): a host whose key changed is refused. The
//! pins go with the run's machines ([`forget`]): a run brought up again meets new machines, maybe
//! at the same addresses, whose new keys a pin would refuse.

use std::path::{Path, PathBuf};

use crate::Result;
use crate::error::{Error, TimedOut};
use crate::process::{Captured, ProcessError, ProcessRunner, ProcessSpec};

/// The runner's key, under its home's `ssh/` folder.
const KEY_FILE: &str = "id_ed25519";

/// How long making the key may take.
const KEYGEN_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

/// The home's lock file held while its key is made ([`ensure_key`]).
const KEY_LOCK: &str = "ssh-key.lock";

/// The id a failure to make the key reads as.
const KEY_STEP: &str = "ssh-key";

/// The host keys the run's SSH pinned, in its state folder.
pub(crate) const KNOWN_HOSTS: &str = "known_hosts";

/// How long `ssh-keyscan` may take to read a host's keys.
const KEYSCAN_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

/// The id a failure to pin a host's key reads as.
const PIN_STEP: &str = "host-key";

/// The runner's SSH folder under `home`: its key.
pub(crate) fn dir(home: &Path) -> PathBuf {
    home.join("ssh")
}

/// The runner's private key: `<home>/ssh/id_ed25519`.
pub(crate) fn key_path(home: &Path) -> PathBuf {
    dir(home).join(KEY_FILE)
}

/// The public half of the runner's key, one line, when it exists.
///
/// # Errors
///
/// [`Error::Io`] when the file is there but can't be read.
pub(crate) fn public_key(home: &Path) -> Result<Option<String>> {
    let path = key_path(home).with_extension("pub");
    let text = crate::store::if_there(std::fs::read_to_string(&path)).map_err(Error::io(&path, "read"))?;
    Ok(text.map(|t| t.trim().to_owned()).filter(|k| !k.is_empty()))
}

/// Makes the runner's key when it has none (`ssh-keygen`, no passphrase), and returns its public
/// half. One maker at a time per home, under [`KEY_LOCK`]: two operations making it at once (two
/// runs started together, in one process or two) each wrote a key over the other's, and one of
/// them handed its machines a public half whose private one was gone (or failed on
/// `ssh-keygen`'s overwrite prompt).
pub(crate) async fn ensure_key(processes: &dyn ProcessRunner, home: &Path) -> Result<String> {
    if let Some(key) = public_key(home)? {
        return Ok(key);
    }
    let failed = |reason: String| Error::ToolFailed { step: KEY_STEP.into(), reason };
    let lock = home.join(KEY_LOCK);
    let wait = KEYGEN_LIMIT * 2;
    let Some(_making) = crate::store::hold(&lock, wait).await.map_err(Error::io(&lock, "locked"))? else {
        return Err(failed(format!(
            "another operation has been making the runner's SSH key for {} s",
            wait.as_secs()
        )));
    };
    // Made meanwhile by the one this waited for.
    if let Some(key) = public_key(home)? {
        return Ok(key);
    }
    let dir = dir(home);
    crate::store::create_dir(&dir)?;
    let path = key_path(home);
    if let Some(key) = public_half_again(processes, &path).await? {
        return Ok(key);
    }
    let spec = ProcessSpec::new(
        "ssh-keygen",
        ["-q", "-t", "ed25519", "-N", "", "-C", "isothrum", "-f"]
            .map(String::from)
            .into_iter()
            .chain([path.display().to_string()]),
    );
    let out = keygen(processes, &spec).await?;
    if !out.success() {
        return Err(failed(out.reason()));
    }
    public_key(home)?.ok_or_else(|| failed("ssh-keygen wrote no public key".into()))
}

/// The public half of a private key at `path` left without it (a disk full when `ssh-keygen`
/// wrote it, a make cut short between the two halves, the `.pub` removed by hand), derived from
/// it again (`ssh-keygen -y`) and written beside it: a machine that already trusts the key keeps
/// working. A private key that can't be read as one (a truncated write) is set aside as
/// `<key>.unusable`, so a new key can be made: `ssh-keygen` would otherwise stop at its overwrite
/// prompt, with no one to answer it, at every later run. `None` when there is no private key, or
/// once it is set aside.
async fn public_half_again(processes: &dyn ProcessRunner, path: &Path) -> Result<Option<String>> {
    if !path.is_file() {
        return Ok(None);
    }
    let derive = ProcessSpec::new("ssh-keygen", ["-y".to_owned(), "-f".to_owned(), path.display().to_string()]);
    let out = keygen(processes, &derive).await?;
    let derived = out.stdout.iter().map(|l| l.trim()).find(|l| l.starts_with("ssh-ed25519 "));
    if let (true, Some(key)) = (out.success(), derived) {
        // `-y` prints the comment only from OpenSSH 7.8 on.
        let key = if key.split_whitespace().count() < 3 {
            format!("{key} isothrum")
        } else {
            key.to_owned()
        };
        crate::store::write(&path.with_extension("pub"), format!("{key}\n").as_bytes())?;
        tracing::warn!(path = %path.display(), "the runner's SSH key had lost its public half: derived from the private one again");
        return Ok(Some(key));
    }
    let aside = path.with_extension("unusable");
    tracing::warn!(path = %path.display(), reason = %out.reason(), "the runner's SSH key can't be read: it is set aside and a new one made");
    std::fs::rename(path, &aside).map_err(Error::io(path, "set aside"))?;
    crate::store::remove(&path.with_extension("pub"))?;
    Ok(None)
}

/// Runs `ssh-keygen` as `spec` to its end within [`KEYGEN_LIMIT`].
async fn keygen(processes: &dyn ProcessRunner, spec: &ProcessSpec) -> Result<Captured> {
    match tokio::time::timeout(KEYGEN_LIMIT, crate::process::capture(processes, spec)).await {
        Err(_) => Err(Error::Timeout {
            step: KEY_STEP.into(),
            timed_out: TimedOut::Ceiling {
                after_s: KEYGEN_LIMIT.as_secs(),
            },
        }),
        Ok(Err(ProcessError::Missing { .. })) => Err(Error::ToolMissing {
            step: KEY_STEP.into(),
            tool: "ssh-keygen".into(),
        }),
        Ok(Err(e)) => Err(Error::ToolFailed {
            step: KEY_STEP.into(),
            reason: e.to_string(),
        }),
        Ok(Ok(out)) => Ok(out),
    }
}

/// Where one of the runner's SSH calls signs in: a user on a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Login {
    /// The user.
    pub(crate) user: String,
    /// The host: a name or an IP address.
    pub(crate) host: String,
}

impl Login {
    /// `user` on `host`; none unless each is plain (letters, digits, `.`, `-`, `_`, and `:` in
    /// a host): nothing `ssh` could read as an option.
    pub(crate) fn new(user: &str, host: &str) -> Option<Login> {
        let plain = |word: &str, extra: &str| {
            !word.is_empty() && !word.starts_with('-') && word.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c) || extra.contains(c))
        };
        (plain(user, "") && plain(host, ":")).then(|| Login {
            user: user.to_owned(),
            host: host.to_owned(),
        })
    }

    /// `user@host`, as `ssh` takes it.
    pub(crate) fn target(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }
}

/// The `ssh` options of a call of the run whose state folder is `state_dir`, signing in with
/// `key`: no user configuration, the run's `known_hosts` checked strictly, no prompt, and a host
/// that stops answering given up in under a minute.
///
/// # Errors
///
/// [`Error::Io`] when the run's folder is at a path no `-o` option can name.
pub(crate) fn options(key: &Path, state_dir: &Path) -> Result<Vec<String>> {
    let known = state_dir.join(KNOWN_HOSTS);
    let quoted = config_path(&known.to_string_lossy(), PathStyle::HERE).ok_or_else(|| {
        let why = "the run's folder is at a path SSH's options can't name (it holds `\"`, `\\` outside Windows, `${` or a control character)";
        Error::io(&known, "named")(std::io::Error::new(std::io::ErrorKind::InvalidInput, why))
    })?;
    let mut args = vec!["-F".to_owned(), "none".to_owned(), "-i".to_owned(), key.display().to_string()];
    for option in [
        "IdentitiesOnly=yes".to_owned(),
        format!("UserKnownHostsFile={quoted}"),
        "GlobalKnownHostsFile=none".to_owned(),
        "StrictHostKeyChecking=yes".to_owned(),
        "BatchMode=yes".to_owned(),
        "ConnectTimeout=10".to_owned(),
        "ServerAliveInterval=15".to_owned(),
        "ServerAliveCountMax=3".to_owned(),
        "LogLevel=ERROR".to_owned(),
    ] {
        args.extend(["-o".to_owned(), option]);
    }
    Ok(args)
}

/// A path as OpenSSH reads it in an option, the parser before 8.7 as the one after: between
/// double quotes, `%` doubled (a token there), and on Windows its `\` written `/` (which Windows'
/// OpenSSH reads as `\`). `None` for a path no parser reads as written: one holding `"` or a
/// control character, `${` (an environment variable from 8.4), or, but on Windows, `\`.
fn config_path(path: &str, style: PathStyle) -> Option<String> {
    let path = match style {
        PathStyle::Windows => path.replace('\\', "/"),
        PathStyle::Unix => path.to_owned(),
    };
    if path.contains(['"', '\\']) || path.contains("${") || path.chars().any(char::is_control) {
        return None;
    }
    Some(format!("\"{}\"", path.replace('%', "%%")))
}

/// How the paths of the computer the runner runs on are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathStyle {
    /// `/` separates.
    Unix,
    /// `\` separates.
    Windows,
}

impl PathStyle {
    /// This computer's.
    const HERE: PathStyle = if cfg!(windows) { PathStyle::Windows } else { PathStyle::Unix };
}

/// Whether `known_hosts` (a file's text) pins a key for `host` (on port 22).
fn pins(known_hosts: &str, host: &str) -> bool {
    known_hosts
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_whitespace().next())
        .any(|hosts| hosts.split(',').any(|h| h == host))
}

/// Pins `host`'s SSH keys in the run's `known_hosts` (in `state_dir`) the first time the run
/// meets it: what `ssh-keyscan` reads of it, written; nothing when a key is pinned for it
/// already, which every later call then checks strictly ([`options`]).
///
/// # Errors
///
/// `tool_missing` without `ssh-keyscan`; `tool_failed` when it reads no key of `host` (it
/// doesn't answer on SSH's port) or out of time; [`Error::Io`] when the file can't be read or
/// written.
pub(crate) async fn pin(processes: &dyn ProcessRunner, state_dir: &Path, host: &str) -> Result<()> {
    let path = state_dir.join(KNOWN_HOSTS);
    let known = crate::store::if_there(std::fs::read_to_string(&path))
        .map_err(Error::io(&path, "read"))?
        .unwrap_or_default();
    if pins(&known, host) {
        return Ok(());
    }
    let failed = |reason: String| Error::ToolFailed { step: PIN_STEP.into(), reason };
    let spec = ProcessSpec::new("ssh-keyscan", ["-T".to_owned(), "10".to_owned(), host.to_owned()]);
    let out = match tokio::time::timeout(KEYSCAN_LIMIT, crate::process::capture(processes, &spec)).await {
        Err(_) => {
            return Err(Error::Timeout {
                step: PIN_STEP.into(),
                timed_out: TimedOut::Ceiling {
                    after_s: KEYSCAN_LIMIT.as_secs(),
                },
            });
        }
        Ok(Err(ProcessError::Missing { .. })) => {
            return Err(Error::ToolMissing {
                step: PIN_STEP.into(),
                tool: "ssh-keyscan".into(),
            });
        }
        Ok(Err(e)) => return Err(failed(e.to_string())),
        Ok(Ok(out)) => out,
    };
    let keys: Vec<&str> = out
        .stdout
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.starts_with('#') && l.split_whitespace().count() >= 3 && pins(l, host))
        .collect();
    if keys.is_empty() {
        return Err(failed(format!("`ssh-keyscan` read no SSH host key of {host}: {}", out.reason())));
    }
    let mut text = known;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for key in keys {
        text.push_str(key);
        text.push('\n');
    }
    crate::store::create_dir(state_dir)?;
    crate::store::write(&path, text.as_bytes())?;
    tracing::info!(host, "pinned the SSH host keys of a machine of the run, first met");
    Ok(())
}

/// Forgets the host keys the run pinned (its machines are gone).
///
/// # Errors
///
/// [`Error::Io`] when the file is there and can't be removed.
pub(crate) fn forget(state_dir: &Path) -> Result<()> {
    crate::store::remove(&state_dir.join(KNOWN_HOSTS))
}

#[cfg(test)]
mod tests;
