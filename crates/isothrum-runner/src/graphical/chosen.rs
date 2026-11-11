//! The graphical consoles the runner chooses for a run (`source.from: runner`, VMware's VNC
//! server): a free port of this computer's loopback and an 8-character password per machine,
//! chosen by an operation that holds the run's lock (`up`, and each one on a run that exists:
//! `resume`, `update`, `attach`…) before its first Vagrant command, and kept in the run's
//! `consoles.json` (owner-only) until its `down`. A query (`status`, `graphical_console`) only
//! reads it. Every Vagrant process of the run's operations gets them in
//! `ISOLOOM_CONSOLES` ([`var`]); a command handed to an embedder never does.

use std::collections::BTreeMap;
use std::path::PathBuf;

use isoloom_manifest::contract::CONSOLES;
use isoloom_manifest::{ConsoleSource, Machine, Protocol};
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::error::Error;
use crate::executor::Cx;
use crate::secrets::SecretValue;

/// The file, in the run's state folder.
const FILE: &str = "consoles.json";

/// How long a password is: the most VNC's authentication reads.
const PASSWORD_LEN: usize = 8;

/// One machine's console, as chosen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Chosen {
    port: u16,
    password: String,
}

/// The run's file.
fn path(cx: &Cx<'_>) -> PathBuf {
    cx.state_dir().join(FILE)
}

/// What the run's file holds, by machine; nothing when there is none.
fn read(cx: &Cx<'_>) -> Result<BTreeMap<String, Chosen>> {
    Ok(crate::store::read_json(&path(cx))?.unwrap_or_default())
}

/// The machines whose console under the run's provider is the runner's to choose.
fn wanted<'m>(cx: &'m Cx<'_>) -> impl Iterator<Item = &'m Machine> {
    let provider = cx.scope.provider;
    cx.run
        .manifest()
        .machines
        .iter()
        .filter(move |m| m.console(provider).is_some_and(|c| matches!(c.source, Some(ConsoleSource::Runner {}))))
}

/// Chooses a console for each machine that wants one and has none yet, before an operation's
/// Vagrant commands run: a free loopback port and a random password, kept in the run's file. Only
/// an operation holding the run's lock calls it, so no other process rewrites the file meanwhile.
pub(crate) fn ensure(cx: &Cx<'_>) -> Result<()> {
    let mut table = read(cx)?;
    let missing: Vec<String> = wanted(cx).map(|m| m.name.to_string()).filter(|m| !table.contains_key(m)).collect();
    if missing.is_empty() {
        return Ok(());
    }
    let taken: Vec<u16> = table.values().map(|c| c.port).collect();
    let mut held = Vec::new();
    for machine in missing {
        let port = loop {
            let Ok(Some(free)) = cx.run.runner.config.ports.pick_free(Protocol::Tcp) else {
                return Err(Error::NoConsole {
                    machine,
                    why: "no free port on this computer's loopback for its console".into(),
                });
            };
            if !taken.contains(&free.port) {
                let port = free.port;
                held.push(free);
                break port;
            }
            held.push(free);
        };
        table.insert(machine, Chosen { port, password: password()? });
    }
    let path = path(cx);
    let text = serde_json::to_vec(&table).map_err(|e| Error::io(&path, "written")(e.into()))?;
    crate::store::write(&path, &text)
}

/// A random password of [`PASSWORD_LEN`] letters and digits.
fn password() -> Result<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let mut bytes = [0u8; PASSWORD_LEN];
    getrandom::fill(&mut bytes).map_err(|e| Error::io("consoles.json", "written")(std::io::Error::other(e.to_string())))?;
    Ok(bytes
        .iter()
        .filter_map(|b| ALPHABET.get(usize::from(*b) % ALPHABET.len()).copied().map(char::from))
        .collect())
}

/// `ISOLOOM_CONSOLES` and what it holds, when the run has a chosen console.
pub(crate) fn var(cx: &Cx<'_>) -> Result<Option<(String, String)>> {
    let table = read(cx)?;
    if table.is_empty() {
        return Ok(None);
    }
    let text = serde_json::to_string(&table).map_err(|e| Error::io(path(cx), "read")(e.into()))?;
    Ok(Some((CONSOLES.to_owned(), text)))
}

/// The port and password chosen for `machine`.
pub(crate) fn of(cx: &Cx<'_>, machine: &Machine) -> Result<(u16, SecretValue)> {
    match read(cx)?.remove(machine.name.as_str()) {
        Some(c) => Ok((c.port, SecretValue::new(c.password))),
        None => Err(super::none(machine, "the runner chose none for it: it boots with one at the run's next start")),
    }
}

/// Forgets the run's chosen consoles (after a successful `down`).
pub(crate) fn forget(cx: &Cx<'_>) -> Result<()> {
    crate::store::remove(&path(cx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_is_eight_unambiguous_letters_and_digits() {
        let a = password().unwrap();
        assert_eq!(a.len(), PASSWORD_LEN);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() && !"0O1lI".contains(c)), "{a}");
        assert_ne!(a, password().unwrap());
    }
}
