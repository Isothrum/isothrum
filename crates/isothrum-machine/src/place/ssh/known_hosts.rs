//! A server's SSH host keys, trusted on first use: the keys it offers are read (`ssh-keyscan`)
//! and given to the caller with their SHA-256 fingerprints, as `ssh-keygen -lf` and the server's
//! own console show them; once the caller confirms them (a user who compared them, say), they are
//! written to a `known_hosts` file the caller names, and every [`Target`](super::Target) checks
//! them strictly. A server offering other keys is refused until the new ones are confirmed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use isothrum_runner::ProcessSpec;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use super::{SshError, safe_token};
use crate::Context;

/// How long reading a server's keys may take.
const SCAN_LIMIT: Duration = Duration::from_secs(20);

/// The key types read, strongest first.
const KEY_TYPES: &str = "ed25519,ecdsa,rsa";

/// One host key of a server, as `known_hosts` writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HostKey {
    /// Its type (`ssh-ed25519`).
    algorithm: String,
    /// Its public key, base64.
    blob: String,
}

impl HostKey {
    /// Its SHA-256 fingerprint, as OpenSSH writes it (`SHA256:` and unpadded base64).
    fn fingerprint(&self) -> Option<String> {
        let raw = base64_decode(&self.blob)?;
        Some(format!("SHA256:{}", base64_encode_unpadded(&Sha256::digest(raw))))
    }
}

/// One key a server offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(rename = "SshHostKey"))]
#[serde(rename_all = "camelCase")]
pub struct Shown {
    /// Its type (`ssh-ed25519`).
    pub algorithm: String,
    /// Its SHA-256 fingerprint (`SHA256:…`).
    pub fingerprint: String,
}

/// Whether the keys a server offers are the ones confirmed for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(rename = "SshHostKeyState"))]
#[serde(rename_all = "camelCase")]
pub enum State {
    /// None was confirmed for it yet.
    New,
    /// They are.
    Confirmed,
    /// Others were: the server was reinstalled, or the connection is intercepted.
    Changed,
}

/// The keys a server offers and whether they are the confirmed ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-rs", ts(rename = "SshHostKeys"))]
#[serde(rename_all = "camelCase")]
pub struct Offered {
    /// The server's address and SSH port, as `known_hosts` names it (`pve.lan`, `[pve.lan]:2222`).
    pub pattern: String,
    /// The keys, strongest first.
    pub keys: Vec<Shown>,
    /// Whether they are the confirmed ones.
    pub state: State,
}

/// How `known_hosts` names `host` reached on `port`.
#[must_use]
pub fn pattern(host: &str, port: u16) -> String {
    if port == 22 { host.to_owned() } else { format!("[{host}]:{port}") }
}

/// The keys of the lines of `text` (a scan, or `ssh-keygen -F`'s answer) that name `pattern`.
fn keys_in(text: &str, pattern: &str) -> Vec<HostKey> {
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with('@'))
        .filter_map(|l| {
            let mut fields = l.split_whitespace();
            let (hosts, algorithm, blob) = (fields.next()?, fields.next()?, fields.next()?);
            // A scan names the host; `ssh-keygen -F` may answer a hashed name (`|1|…`).
            let ours = hosts.starts_with("|1|") || hosts.split(',').any(|n| n == pattern);
            ours.then(|| HostKey {
                algorithm: algorithm.to_owned(),
                blob: blob.to_owned(),
            })
        })
        .collect()
}

/// Runs `program args` within [`SCAN_LIMIT`].
async fn run(ctx: &Context, program: &str, args: &[&str]) -> Result<isothrum_runner::Captured, SshError> {
    Ok(ctx.probe_within(SCAN_LIMIT, &ProcessSpec::new(program, args.iter().copied())).await?)
}

/// The keys `host` offers on `port`.
async fn scan(ctx: &Context, host: &str, port: u16) -> Result<Vec<HostKey>, SshError> {
    if !safe_token(host) {
        return Err(SshError::Invalid("host"));
    }
    let port_arg = port.to_string();
    let scanned = run(ctx, "ssh-keyscan", &["-T", "10", "-p", &port_arg, "-t", KEY_TYPES, host]).await?;
    let out = scanned.output().map_err(|f| SshError::Failed(f.to_string()))?;
    let mut keys = keys_in(&out, &pattern(host, port));
    keys.dedup();
    if keys.is_empty() {
        return Err(SshError::Failed(format!("{host} offered no SSH host key on port {port}")));
    }
    Ok(keys)
}

/// The keys confirmed for `pattern` in `file`; none for a file not written yet.
async fn confirmed(ctx: &Context, file: &Path, pattern: &str) -> Result<Vec<HostKey>, SshError> {
    if !file.is_file() {
        return Ok(Vec::new());
    }
    let path = file.to_string_lossy();
    let found = run(ctx, "ssh-keygen", &["-F", pattern, "-f", &path]).await?;
    if found.success() {
        return Ok(keys_in(&found.stdout.join("\n"), pattern));
    }
    // Not found: `ssh-keygen -F` exits 1 without a word.
    if found.stderr_tail.iter().all(|l| l.trim().is_empty()) {
        return Ok(Vec::new());
    }
    Err(SshError::Failed(found.reason()))
}

/// Whether the keys `offered` are the ones confirmed (`known`).
fn state(offered: &[HostKey], known: &[HostKey]) -> State {
    match known {
        [] => State::New,
        known if known.iter().all(|k| offered.contains(k)) => State::Confirmed,
        _ => State::Changed,
    }
}

/// The keys `host` offers on `port`, to be confirmed, and whether they are the ones confirmed in
/// `file` already.
///
/// # Errors
///
/// The host isn't a plain word, its keys couldn't be read, or it offered none.
pub async fn offered(ctx: &Context, host: &str, port: u16, file: &Path) -> Result<Offered, SshError> {
    let pattern = pattern(host, port);
    let keys = scan(ctx, host, port).await?;
    let known = confirmed(ctx, file, &pattern).await?;
    let shown = keys
        .iter()
        .filter_map(|k| {
            Some(Shown {
                algorithm: k.algorithm.clone(),
                fingerprint: k.fingerprint()?,
            })
        })
        .collect();
    Ok(Offered {
        state: state(&keys, &known),
        keys: shown,
        pattern,
    })
}

/// Confirms the keys `host` offers on `port`, which were shown as `fingerprints`: read again,
/// they must be exactly those (a server that changed its keys meanwhile is refused), then they
/// replace any keys each of `files` had for it, the other hosts' kept.
///
/// # Errors
///
/// The keys couldn't be read, aren't the ones shown, or a file couldn't be written.
pub async fn confirm(ctx: &Context, host: &str, port: u16, fingerprints: &[String], files: &[PathBuf]) -> Result<(), SshError> {
    let pattern = pattern(host, port);
    let keys = scan(ctx, host, port).await?;
    let mut now: Vec<String> = keys.iter().filter_map(HostKey::fingerprint).collect();
    let mut shown = fingerprints.to_vec();
    now.sort();
    shown.sort();
    if now != shown {
        return Err(SshError::Failed(format!("the SSH host keys of {host} changed while they were being confirmed")));
    }
    let mut lines = String::new();
    for k in &keys {
        for field in [pattern.as_str(), " ", &k.algorithm, " ", &k.blob, "\n"] {
            lines.push_str(field);
        }
    }
    for file in files {
        forget(ctx, file, &pattern).await?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let kept = match std::fs::read_to_string(file) {
            Ok(text) if text.is_empty() || text.ends_with('\n') => text,
            Ok(text) => text + "\n",
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.into()),
        };
        write_private(file, &(kept + &lines))?;
    }
    Ok(())
}

/// Writes `text` to `file`, readable by its owner only (on unix).
fn write_private(file: &Path, text: &str) -> std::io::Result<()> {
    std::fs::write(file, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Removes the keys `file` has for `pattern`, hashed or not (`ssh-keygen -R`, which keeps the
/// previous file as `.old`).
async fn forget(ctx: &Context, file: &Path, pattern: &str) -> Result<(), SshError> {
    if !file.is_file() {
        return Ok(());
    }
    let removed = run(ctx, "ssh-keygen", &["-R", pattern, "-f", &file.to_string_lossy()]).await?;
    removed.output().map(drop).map_err(|f| SshError::Failed(f.to_string()))
}

/// The standard base64 alphabet.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `text`, standard base64 (padded or not), decoded; none when it isn't base64.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let digits: Vec<u8> = text
        .trim_end_matches('=')
        .bytes()
        .map(|b| ALPHABET.iter().position(|a| *a == b).and_then(|p| u8::try_from(p).ok()))
        .collect::<Option<_>>()?;
    if digits.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(digits.len() * 3 / 4);
    for chunk in digits.chunks(4) {
        let mut n: u32 = 0;
        for (i, d) in chunk.iter().enumerate() {
            n |= u32::from(*d) << (18 - 6 * i);
        }
        let bytes = n.to_be_bytes();
        out.extend(bytes.iter().skip(1).take(chunk.len().saturating_sub(1)));
    }
    Some(out)
}

/// `bytes` as standard base64 without padding.
fn base64_encode_unpadded(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut n: u32 = 0;
        for (i, b) in chunk.iter().enumerate() {
            n |= u32::from(*b) << (16 - 8 * i);
        }
        for i in 0..=chunk.len() {
            let index = (n >> (18 - 6 * i)) & 0x3f;
            let digit = usize::try_from(index).ok().and_then(|at| ALPHABET.get(at)).copied().unwrap_or(b'A');
            out.push(char::from(digit));
        }
    }
    out
}

#[cfg(test)]
mod tests;
