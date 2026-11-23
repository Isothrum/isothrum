//! The names the driver reads and writes: a VM's record key, whether a name carries the run's
//! key, Vagrant's verbs, an account's name, and a machine's own address.

use super::{Error, READ_LIMIT, Record, Rerun, Result, STATUS_PATIENCE, ToolCx, Vagrant, Verb, retrying, vagrant};
use crate::Provider;

/// The key a machine's VM is recorded under: `vm:<machine>` on the provider asked for, and
/// `vm:<machine>@<provider>` for a record left on another one.
pub(super) fn key_of(record: &Record, asked: Option<Provider>) -> String {
    if asked.is_some_and(|p| p.id() == record.provider) {
        format!("vm:{}", record.machine)
    } else {
        format!("vm:{}@{}", record.machine, record.provider)
    }
}

/// The machine and provider a key names ([`key_of`]): the provider asked for when it names none.
pub(super) fn of_key(key: &str, asked: Option<Provider>) -> Option<(String, Option<String>)> {
    let rest = key.strip_prefix("vm:")?;
    Some(match rest.split_once('@') {
        Some((machine, provider)) => (machine.to_owned(), Some(provider.to_owned())),
        None => (rest.to_owned(), asked.map(|p| p.id().to_owned())),
    })
}

/// Whether `name` (a VM's or its folder's) carries the run's key `key` as a word of its own: a
/// run of letters and digits between characters that are neither (or its ends). The key is made
/// from this runner's home and the run's name, so another copy of the project never matches.
pub(super) fn ours(name: &str, key: &str) -> bool {
    !key.is_empty() && name.split(|c: char| !c.is_alphanumeric()).any(|word| word == key)
}

/// Whether a VM of `provider` (as Vagrant names it) is reached at an address of its own, which
/// `ssh-config` gives: every provider but those that reach it through a port forwarded on this
/// computer's loopback (VirtualBox, QEMU, UTM).
pub(super) fn own_address(provider: &str) -> bool {
    ![Provider::Virtualbox, Provider::Qemu, Provider::Utm].iter().any(|p| p.id() == provider)
}

impl Vagrant {
    /// The address `ssh-config` gives running `machine`, when it is one another computer
    /// reaches; none when it gives none, or can't be read (traced): the status stands without it.
    pub(super) async fn address(&self, cx: &ToolCx<'_>, machine: &str) -> Option<String> {
        let read = super::read(cx, READ_LIMIT);
        match retrying(cx.warn, "ssh-config", Rerun::Read, STATUS_PATIENCE, || vagrant::ssh_host(&read, machine)).await {
            Ok(host) => host
                .parse::<std::net::IpAddr>()
                .map_or(true, |ip| !ip.is_loopback() && !ip.is_unspecified())
                .then_some(host),
            Err(e) => {
                tracing::debug!(machine, error = %e, "the machine's address couldn't be read");
                None
            }
        }
    }
}

/// `verb` as Vagrant's command names it, for what is said of it.
pub(super) fn verb_word(verb: Verb) -> &'static str {
    match verb {
        Verb::Halt => "halt",
        Verb::Suspend => "suspend",
        Verb::Provision => "provision",
        Verb::Destroy => "destroy",
    }
}

/// `machine` alone, as a call's machine list.
pub(super) fn one(machine: &str) -> Vec<String> {
    vec![machine.to_owned()]
}

/// An account name as a console takes it: letters, digits, `.`, `_` and `-`, so nothing in the
/// script it goes into needs quoting.
pub(super) fn checked_user(user: &str) -> Result<&str> {
    let fine = !user.is_empty() && user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if fine {
        Ok(user)
    } else {
        Err(Error::Unsupported {
            what: format!("a console as `{user}`, which isn't an account name (letters, digits, `.`, `_`, `-`),"),
        })
    }
}
