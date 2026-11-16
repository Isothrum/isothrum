//! A module's VM whose screen answers only on the module's own networks, reached through an SSH
//! tunnel to the Linux machine beside it: a Windows VM's Remote Desktop, or a VNC server the
//! module names. Everything is read from the module's own outputs and state, as `output -json`
//! and `show -json` give them:
//!
//! - **Which machines.** The module's `consoles` output, when it has one, names them, each with
//!   its protocol (`{ dc = "rdp" }`) or more (`{ dc = { protocol = "vnc", port = 5901, host =
//!   "10.9.0.20", via = "203.0.113.7", user = "admin" } }`), and no other machine has one. Without
//!   it, each VM the state says runs Windows ([`crate::cli::terraform::VmFacts::windows`]) has
//!   Remote Desktop (3389) when the module gives its password (`windows_password`, sensitive): the
//!   outputs Isoloom's modules give.
//! - **Through where.** The module's `ip` output (where a user stands: its access machine, its
//!   router, its first Linux VM), signed in as the user of the machine at that address
//!   (`ssh_user_<machine>`, the one whose `address_<machine>` it is), else as `ssh_user`; a
//!   `consoles` entry's `via` and `user` win. The key is the run's `ssh_private_key_file` value
//!   (the module's variable, as the run's start was given it and every operation replays), else
//!   the runner's own.
//! - **To where.** The VM's address on the module's networks, as its state says
//!   ([`crate::cli::terraform::VmFacts::private_address`], else its only one); a `consoles`
//!   entry's `host` wins.
//! - **Signing in.** Remote Desktop's user is the module's `windows_user` output, else
//!   `Administrator`; its password the sensitive `windows_password` output, else the run's own
//!   `windows_password` value or secret (a module that takes it as an input). A VNC server's
//!   password is the `vnc_password` output, when there is one.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use super::{Module, machines_of, text_of};
use crate::Result;
use crate::cli::terraform::{self as tf, StateAt};
use crate::events::Stream;
use crate::formats::{Output, SeenMachine};
use crate::graphical::tunnel::To;
use crate::graphical::{Console, ConsoleOffer, ConsoleProtocol, no_console};
use crate::secrets::SecretValue;
use crate::ssh::Login;

/// The output naming the machines whose screen is tunnelled, and how.
const CONSOLES_OUTPUT: &str = "consoles";

/// The sensitive output holding the Windows machines' administrator password.
const PASSWORD_OUTPUT: &str = "windows_password";

/// The output naming the Windows machines' administrator, when it isn't [`ADMINISTRATOR`].
const USER_OUTPUT: &str = "windows_user";

/// The output holding a VNC server's password.
const VNC_PASSWORD_OUTPUT: &str = "vnc_password";

/// The output naming where a user stands: the machine the tunnel goes through.
const VIA_OUTPUT: &str = "ip";

/// The output naming the user there, when no machine's address output is that address.
const VIA_USER_OUTPUT: &str = "ssh_user";

/// The run's value naming the private key that signs in to the module's machines.
pub(super) const KEY_VAR: &str = "ssh_private_key_file";

/// Windows' own administrator.
const ADMINISTRATOR: &str = "Administrator";

/// Remote Desktop's port.
const RDP_PORT: u16 = 3389;

/// VNC's first display's port.
const VNC_PORT: u16 = 5900;

/// How long a read of the outputs may take.
const READ_LIMIT: Duration = Duration::from_secs(60);

/// The outputs as text, by name: a string as it is, anything else as JSON; a null or empty one
/// left out (an output with no value yet).
pub(super) fn texts(outputs: &BTreeMap<String, Output>) -> BTreeMap<String, String> {
    outputs
        .iter()
        .filter(|(_, o)| !o.value.is_empty() && o.value != "null")
        .map(|(name, o)| (name.clone(), o.value.clone()))
        .collect()
}

/// One machine's entry in the `consoles` output.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Entry {
    /// Its protocol only.
    Protocol(String),
    /// Its protocol and where it is.
    Full(Declared),
}

/// A `consoles` entry in full.
#[derive(Debug, Default, Deserialize)]
struct Declared {
    protocol: String,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    via: Option<String>,
    #[serde(default)]
    user: Option<String>,
}

/// One machine's tunnelled screen, as the outputs and the state say.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Plan {
    protocol: ConsoleProtocol,
    port: u16,
    host: Option<String>,
    via: Option<String>,
    user: Option<String>,
}

/// `machine`'s tunnelled screen, as the outputs (`out`) say: the `consoles` entry naming it
/// (none for a machine it leaves out), else Remote Desktop for a Windows VM the module gives the
/// password of. `Err` saying why for a `consoles` output that can't be read.
fn plan(out: &BTreeMap<String, String>, machine: &SeenMachine) -> Option<std::result::Result<Plan, String>> {
    if let Some(consoles) = out.get(CONSOLES_OUTPUT) {
        let entries: BTreeMap<String, Entry> = match serde_json::from_str(consoles) {
            Ok(entries) => entries,
            Err(e) => return Some(Err(format!("the module's `{CONSOLES_OUTPUT}` output isn't a map of machines to consoles: {e}"))),
        };
        let declared = match entries.into_iter().find(|(name, _)| *name == machine.name)?.1 {
            Entry::Protocol(protocol) => Declared {
                protocol,
                ..Declared::default()
            },
            Entry::Full(declared) => declared,
        };
        let (protocol, port) = match declared.protocol.to_ascii_lowercase().as_str() {
            "rdp" => (ConsoleProtocol::Rdp, RDP_PORT),
            "vnc" => (ConsoleProtocol::Vnc, VNC_PORT),
            other => {
                return Some(Err(format!(
                    "the module's `{CONSOLES_OUTPUT}` output gives it a console of `{}`: `rdp` or `vnc` only",
                    other.escape_debug()
                )));
            }
        };
        return Some(Ok(Plan {
            protocol,
            port: declared.port.unwrap_or(port),
            host: declared.host,
            via: declared.via,
            user: declared.user,
        }));
    }
    (machine.windows && out.contains_key(PASSWORD_OUTPUT)).then_some(Ok(Plan {
        protocol: ConsoleProtocol::Rdp,
        port: RDP_PORT,
        host: None,
        via: None,
        user: None,
    }))
}

/// Where `plan`'s tunnel goes through, as the outputs (`out`) say: its `via`, else the `ip`
/// output; as its `user`, else the user of the machine whose address output is that address,
/// else the `ssh_user` output.
fn via(out: &BTreeMap<String, String>, plan: &Plan) -> std::result::Result<Login, String> {
    let host = plan
        .via
        .clone()
        .or_else(|| out.get(VIA_OUTPUT).cloned())
        .ok_or_else(|| format!("the module names no Linux machine to tunnel it through (its `{VIA_OUTPUT}` output)"))?;
    let user =
        plan.user.clone().or_else(|| user_at(out, &host)).ok_or_else(|| {
            format!("the module names no SSH user on {host}, the machine its tunnel goes through (`ssh_user_<machine>`, `{VIA_USER_OUTPUT}`)")
        })?;
    Login::new(&user, &host).ok_or_else(|| format!("`{}@{}` isn't an SSH login the runner signs in with", user.escape_debug(), host.escape_debug()))
}

/// The SSH user the outputs (`out`) give for the machine at address `host`: `ssh_user_<machine>`
/// of the machine whose `address_<machine>` it is, else `ssh_user`.
pub(super) fn user_at(out: &BTreeMap<String, String>, host: &str) -> Option<String> {
    out.iter()
        .filter(|(_, v)| *v == host)
        .find_map(|(name, _)| out.get(&format!("ssh_user_{}", name.strip_prefix("address_")?)))
        .or_else(|| out.get(VIA_USER_OUTPUT))
        .cloned()
}

/// What a status says of `machine`'s screen, as the outputs (`out`) say: its protocol, through
/// the machine it is tunnelled through (unavailable, saying why, when the outputs name none); none
/// for a Windows VM the module gives no way to; nothing for any other machine.
pub(super) fn offer(out: &BTreeMap<String, String>, machine: &SeenMachine) -> Option<ConsoleOffer> {
    let plan = match plan(out, machine) {
        Some(Ok(plan)) => plan,
        Some(Err(why)) => return Some(ConsoleOffer::none(&why)),
        None if machine.windows => {
            return Some(ConsoleOffer::none(&format!(
                "the module gives no Remote Desktop password for it (`{PASSWORD_OUTPUT}`) nor a `{CONSOLES_OUTPUT}` output naming it"
            )));
        }
        None => return None,
    };
    let offer = match via(out, &plan) {
        Ok(login) => ConsoleOffer::of(plan.protocol, Some(&format!("through an SSH tunnel to {}", login.target()))),
        Err(why) => ConsoleOffer::of(plan.protocol, Some("through an SSH tunnel")).unavailable(Some(why)),
    };
    Some(offer)
}

/// The module's outputs as text ([`texts`]), read quietly from the state at `state`.
pub(super) async fn outputs(module: &Module<'_, '_>, state: StateAt<'_>) -> Result<BTreeMap<String, String>> {
    if matches!(state, StateAt::File(file) if !file.exists()) {
        return Ok(BTreeMap::new());
    }
    let quiet = |_: Stream, _: &str| {};
    let read = tf::outputs(&module.call(READ_LIMIT, &quiet), state)
        .await
        .map_err(|e| module.error("output", e))?;
    let outputs = read
        .into_iter()
        .map(|(name, o)| {
            let output = Output {
                value: text_of(&o.value),
                sensitive: o.sensitive,
            };
            (name, output)
        })
        .collect();
    Ok(texts(&outputs))
}

/// `machine`'s screen (`vm`, of the state at `state`), through an SSH tunnel the core opens: where
/// it goes and how to sign in, from the module's outputs, its state and the run's values.
pub(super) async fn reach(module: &Module<'_, '_>, state: StateAt<'_>, vm: &tf::Resource, machine: &str) -> Result<Console> {
    let cx = module.cx;
    let out = outputs(module, state).await?;
    let seen = machines_of(std::slice::from_ref(vm))
        .pop()
        .ok_or_else(|| no_console(machine, "it isn't a VM"))?;
    let plan = match plan(&out, &seen) {
        Some(Ok(plan)) => plan,
        Some(Err(why)) => return Err(no_console(machine, why)),
        None if seen.windows => {
            return Err(no_console(
                machine,
                format!("the module gives no Remote Desktop password for it (`{PASSWORD_OUTPUT}`) nor a `{CONSOLES_OUTPUT}` output naming it"),
            ));
        }
        None => {
            return Err(no_console(
                machine,
                format!(
                    "a `{}` gives no screen this runner reaches (its module names none in a `{CONSOLES_OUTPUT}` output)",
                    vm.kind
                ),
            ));
        }
    };
    let login = via(&out, &plan).map_err(|why| no_console(machine, why))?;
    let host = plan
        .host
        .clone()
        .or_else(|| vm.vm.private_address.clone())
        .or_else(|| vm.vm.address.clone())
        .ok_or_else(|| no_console(machine, "the module's state gives no address of it to tunnel to"))?;
    let note = format!("through an SSH tunnel to {}", login.target());
    let to = To {
        via: login,
        host,
        port: plan.port,
        key: cx.var(KEY_VAR).map(PathBuf::from),
    };
    let mut console = Console::tunnelled(machine, plan.protocol, to).with_note(Some(&note));
    match plan.protocol {
        ConsoleProtocol::Rdp => {
            console.user = Some(out.get(USER_OUTPUT).cloned().unwrap_or_else(|| ADMINISTRATOR.to_owned()));
            console.password = out.get(PASSWORD_OUTPUT).cloned().or_else(|| cx.var(PASSWORD_OUTPUT)).map(SecretValue::new);
        }
        _ => console.password = out.get(VNC_PASSWORD_OUTPUT).cloned().map(SecretValue::new),
    }
    Ok(console)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm(windows: bool) -> SeenMachine {
        SeenMachine {
            name: "dc".into(),
            state: crate::MachineState::Running,
            published: Vec::new(),
            kind: Some("aws_instance".into()),
            address: Some("198.51.100.20".into()),
            windows,
        }
    }

    fn outputs(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn a_windows_vm_with_its_password_has_remote_desktop_through_the_modules_ip() {
        let out = outputs(&[
            ("windows_password", "s3cret!"),
            ("ip", "203.0.113.7"),
            ("address_web", "203.0.113.7"),
            ("ssh_user_web", "admin"),
            ("address_dc", "198.51.100.20"),
            ("ssh_user_dc", "isoloom"),
        ]);
        let plan = plan(&out, &vm(true)).unwrap().unwrap();
        assert_eq!((plan.protocol, plan.port), (ConsoleProtocol::Rdp, 3389));
        assert_eq!(via(&out, &plan).unwrap().target(), "admin@203.0.113.7");
        let offer = offer(&out, &vm(true)).unwrap();
        assert_eq!(offer.protocol, ConsoleProtocol::Rdp);
        assert_eq!(offer.unavailable, None);
        assert_eq!(super::offer(&out, &vm(false)), None, "a Linux VM has none");
    }

    #[test]
    fn without_a_jump_host_the_offer_says_why_and_without_a_password_there_is_none() {
        let out = outputs(&[("windows_password", "x")]);
        assert!(offer(&out, &vm(true)).unwrap().unavailable.unwrap().contains("`ip`"));
        let none = offer(&outputs(&[("ip", "203.0.113.7")]), &vm(true)).unwrap();
        assert_eq!(none.protocol, ConsoleProtocol::None);
    }

    #[test]
    fn the_consoles_output_names_the_machines_and_wins() {
        let out = outputs(&[
            ("consoles", r#"{"dc":{"protocol":"vnc","port":5901,"via":"10.0.0.1","user":"ops"},"db":"rdp"}"#),
            ("windows_password", "x"),
            ("ip", "203.0.113.7"),
            ("ssh_user", "admin"),
        ]);
        let plan = plan(&out, &vm(false)).unwrap().unwrap();
        assert_eq!((plan.protocol, plan.port), (ConsoleProtocol::Vnc, 5901));
        assert_eq!(via(&out, &plan).unwrap().target(), "ops@10.0.0.1");
        let mut db = vm(false);
        db.name = "db".into();
        let rdp = super::plan(&out, &db).unwrap().unwrap();
        assert_eq!(via(&out, &rdp).unwrap().target(), "admin@203.0.113.7");
        let mut other = vm(true);
        other.name = "other".into();
        assert!(super::plan(&out, &other).is_none(), "a machine the output leaves out has none");
        let spice = super::plan(&outputs(&[("consoles", r#"{"dc":"spice"}"#)]), &vm(false)).unwrap();
        assert!(
            spice.as_ref().is_err_and(|why| why.ends_with("a console of `spice`: `rdp` or `vnc` only")),
            "{spice:?}"
        );
    }
}
