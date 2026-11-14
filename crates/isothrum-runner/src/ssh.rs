//! The runner's own SSH key (`<home>/ssh/id_ed25519`), made on first use: what an embedder
//! authorizes where a run's tools reach a machine over SSH as the runner ([`crate::Runner::ssh_public_key`]).

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

#[cfg(test)]
mod tests;
