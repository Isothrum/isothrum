//! The way the runner's SSH reaches a module's VM (a console, a command), as the module's outputs,
//! its state and the run's values say:
//!
//! - **To where.** The `address_<machine>` output (an address this computer reaches: a cloud VM's
//!   public one), else the address the `machines` output names (`{ web = "10.60.0.10" }`: on the
//!   module's own networks, behind its router), else the VM's address in the state.
//! - **Through where.** A machine at the `machines` output's address is reached through the
//!   module's `ip` output (its router, its access machine), signed in as the user of the machine
//!   whose address that is, else `ssh_user`; unless it is at that address itself.
//! - **First through where.** The run's `ssh_jump` value (`user@host`), when it has one: a host
//!   this computer reaches that reaches the rest (a server's node whose lab network is internal to
//!   it). Every console and command of the run goes through it first.
//! - **As whom.** The user asked for, else `ssh_user_<machine>`, else the `ssh_user` of the machine
//!   whose `address_<machine>` the address is, else `ssh_user`.
//! - **With which key.** The run's `ssh_private_key_file` value, else the runner's own key, for
//!   every hop.
//!
//! A Windows VM has no way in: its screen is its Remote Desktop.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::{Module, tunnelled, vm_named};
use crate::Result;
use crate::ssh::Login;

/// The output naming the machine those addresses are reached through.
const VIA_OUTPUT: &str = "ip";

/// The run's value naming the host every SSH of the run goes through first.
pub(crate) const JUMP_VAR: &str = "ssh_jump";

/// How to reach a machine: the hosts on the way (outermost first), the login at the end, and the
/// key that signs in to each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Route {
    /// The hosts the call goes through first, outermost first.
    pub(super) hops: Vec<Login>,
    /// The machine, and the user there.
    pub(super) to: Login,
    /// The private key.
    pub(super) key: PathBuf,
}

impl Route {
    /// The first host on the way: the one this computer reaches, pinned before a call.
    pub(super) fn first(&self) -> &Login {
        self.hops.first().unwrap_or(&self.to)
    }

    /// The `ssh` options of a call along the route, of `module`'s run, its first host pinned first
    /// ([`crate::ssh::pin`]).
    pub(super) async fn options(&self, module: &Module<'_, '_>) -> Result<Vec<String>> {
        let cx = module.cx;
        crate::ssh::pin(cx.processes, cx.state_dir, &self.first().host).await?;
        crate::ssh::options_through(&self.key, cx.state_dir, &self.hops)
    }
}

/// The route to `machine`, signed in as `user` when given. `Err` with why there is none, for the
/// caller to say as its own error: `Ok(Err(why))`.
///
/// # Errors
///
/// `machine_unknown` when the module has no such VM; as the state's and the outputs' reads fail.
pub(super) async fn to(module: &Module<'_, '_>, machine: &str, user: Option<&str>) -> Result<std::result::Result<Route, String>> {
    let cx = module.cx;
    let state = module.state()?;
    let held = module.shown(state).await?;
    let vm = vm_named(cx, &held, machine)?;
    if vm.vm.windows {
        return Ok(Err("it runs Windows: its screen is its Remote Desktop".to_owned()));
    }
    let out = tunnelled::outputs(module, state).await?;
    let jump = cx.var(JUMP_VAR);
    Ok(route(&out, vm.vm.address.as_deref(), machine, user, jump.as_deref()).map(|(hops, to)| Route {
        hops,
        to,
        key: cx.var(tunnelled::KEY_VAR).map_or_else(|| cx.ssh_key.to_path_buf(), PathBuf::from),
    }))
}

/// The hops and the login of the route to `machine` (`stated`: its address in the state), as the
/// outputs (`out`) and the run's jump (`jump`) say; `user` when given.
fn route(
    out: &BTreeMap<String, String>,
    stated: Option<&str>,
    machine: &str,
    user: Option<&str>,
    jump: Option<&str>,
) -> std::result::Result<(Vec<Login>, Login), String> {
    let (address, behind) = match (out.get(&format!("address_{machine}")), tunnelled::named_address(out, machine)) {
        (Some(address), _) => (address.clone(), false),
        (None, Some(address)) => (address, true),
        (None, None) => (
            stated
                .map(str::to_owned)
                .ok_or_else(|| format!("neither its state nor the module's outputs (`address_{machine}`, `machines`) give it an address"))?,
            false,
        ),
    };
    let mut hops = Vec::new();
    if let Some(jump) = jump {
        let (jump_user, jump_host) = jump
            .split_once('@')
            .ok_or_else(|| format!("the run's `{JUMP_VAR}` value `{}` isn't `user@host`", jump.escape_debug()))?;
        hops.push(Login::new(jump_user, jump_host).ok_or_else(|| format!("the run's `{JUMP_VAR}` value `{}` isn't an SSH login", jump.escape_debug()))?);
    }
    if behind {
        let via = out
            .get(VIA_OUTPUT)
            .ok_or_else(|| format!("the module names no machine to reach {address} through (its `{VIA_OUTPUT}` output)"))?;
        if *via != address {
            let login = tunnelled::user_at(out, via)
                .and_then(|u| Login::new(&u, via))
                .ok_or_else(|| format!("the module names no SSH user on {via}, the machine {address} is reached through (`ssh_user`)"))?;
            hops.push(login);
        }
    }
    let user = user
        .map(str::to_owned)
        .or_else(|| out.get(&format!("ssh_user_{machine}")).cloned())
        .or_else(|| tunnelled::user_at(out, &address))
        .ok_or_else(|| format!("the module names no SSH user for it (`ssh_user_{machine}`, `ssh_user`)"))?;
    let to = Login::new(&user, &address).ok_or_else(|| format!("`{}@{}` isn't an SSH login", user.escape_debug(), address.escape_debug()))?;
    Ok((hops, to))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outputs(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    /// Each hop and the login, as `user@host`.
    fn targets(route: &(Vec<Login>, Login)) -> Vec<String> {
        route.0.iter().chain(std::iter::once(&route.1)).map(Login::target).collect()
    }

    /// A server module's outputs: its machines behind its router at 192.0.2.10.
    fn behind_a_router() -> BTreeMap<String, String> {
        outputs(&[
            ("ip", "192.0.2.10"),
            ("ssh_user", "isoloom"),
            ("machines", r#"{"web":"10.60.0.10","cache":"10.60.0.20"}"#),
        ])
    }

    #[test]
    fn a_machine_on_the_modules_networks_is_reached_through_its_router_and_the_runs_jump_first() {
        let out = behind_a_router();
        assert_eq!(
            targets(&route(&out, None, "web", None, None).expect("a route")),
            ["isoloom@192.0.2.10", "isoloom@10.60.0.10"]
        );
        assert_eq!(
            targets(&route(&out, None, "web", Some("player"), Some("root@node.lab")).expect("a route")),
            ["root@node.lab", "isoloom@192.0.2.10", "player@10.60.0.10"],
            "the jump first, the router as its own user, the machine as the user asked for"
        );
        // The router itself: at its address in the state, through the jump alone.
        assert_eq!(
            targets(&route(&out, Some("192.0.2.10"), "isoloom_router", None, Some("root@node.lab")).expect("a route")),
            ["root@node.lab", "isoloom@192.0.2.10"]
        );
    }

    #[test]
    fn a_machine_at_an_address_of_its_own_is_reached_straight() {
        let out = outputs(&[
            ("ip", "203.0.113.7"),
            ("address_web", "203.0.113.9"),
            ("ssh_user_web", "admin"),
            ("machines", r#"{"web":"203.0.113.9"}"#),
        ]);
        assert_eq!(targets(&route(&out, None, "web", None, None).expect("a route")), ["admin@203.0.113.9"]);
        // A module with no such outputs: its state's address, as `ssh_user`.
        let plain = outputs(&[("ssh_user", "debian")]);
        assert_eq!(
            targets(&route(&plain, Some("198.51.100.4"), "db", None, None).expect("a route")),
            ["debian@198.51.100.4"]
        );
        // The machine at the router's address is the router: no hop to itself.
        let at_ip = outputs(&[("ip", "10.60.0.10"), ("ssh_user", "isoloom"), ("machines", r#"{"web":"10.60.0.10"}"#)]);
        assert_eq!(targets(&route(&at_ip, None, "web", None, None).expect("a route")), ["isoloom@10.60.0.10"]);
    }

    #[test]
    fn a_route_that_cant_be_made_says_why() {
        let out = behind_a_router();
        let why = |r: std::result::Result<(Vec<Login>, Login), String>| r.expect_err("no route");
        assert!(why(route(&out, None, "db", None, None)).contains("give it an address"));
        assert!(why(route(&outputs(&[("machines", r#"{"web":"10.60.0.10"}"#)]), None, "web", None, None)).contains("`ip` output"));
        assert!(why(route(&out, None, "web", None, Some("node.lab"))).contains("isn't `user@host`"));
        assert!(why(route(&out, None, "web", None, Some("root@-oProxyCommand=x"))).contains("isn't an SSH login"));
        let no_user = outputs(&[("machines", r#"{"web":"10.60.0.10"}"#), ("ip", "10.60.0.10")]);
        assert!(why(route(&no_user, None, "web", None, None)).contains("no SSH user"));
    }
}
