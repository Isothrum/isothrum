//! Planning a step, the executor's pure core: whether it runs on this host and with the run's
//! provider, and a command step's program and arguments for this host, its placeholders filled
//! from the operation's values. Nothing here runs a tool: the executor runs what it plans.

use isoloom_manifest::{Argv, HostArgv, HostOs, Step};

use crate::encoding::wire_name;
use crate::scope::Scope;
use crate::vars::Vars;
use crate::{Error, Result};

/// A command step, planned: its program and its arguments, filled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Command {
    /// The program, a bare tool name.
    pub(crate) program: String,
    /// Its arguments, filled.
    pub(crate) args: Vec<String>,
}

/// Whether `step` runs in `scope`: its host and provider filters (empty: every one).
pub(crate) fn applies(scope: &Scope, step: &Step) -> bool {
    scope.admits(&step.hosts, &step.providers)
}

/// Where step `step` of list `list` is, for errors (`lifecycle.up.compose_up`).
pub(crate) fn at(list: &str, step: &Step) -> String {
    format!("{list}.{}", step.id)
}

/// The command of `argv` on `host`, filled from `vars`; `at` names the step for errors.
///
/// # Errors
///
/// [`Error::Unsupported`] when `argv` has none for `host`, [`Error::Unbound`] for a placeholder
/// with no value.
pub(crate) fn command(argv: &HostArgv, host: HostOs, vars: &Vars, at: &str) -> Result<Command> {
    let chosen = for_host(argv, host).ok_or_else(|| Error::Unsupported {
        what: format!("{at} on this host"),
    })?;
    let filled = chosen.args().iter().map(|a| vars.render(a, at)).collect::<Result<Vec<_>>>()?;
    Ok(Command {
        program: chosen.program().as_str().to_owned(),
        args: filled,
    })
}

/// How long a step the manifest gives no time limit may run: every external call has a limit.
const DEFAULT_TIMEOUT_S: u64 = 3600;

/// How long `step` may run in `scope`, in seconds: its own limit on the run's provider when it
/// has one there (`provider_timeout_s`), else its `timeout_s`, else an hour.
pub(crate) fn limit_s(step: &Step, scope: &Scope) -> u64 {
    let on_provider = step
        .provider_timeout_s
        .iter()
        .find(|(p, _)| scope.provider.as_deref() == Some(wire_name(*p).as_str()))
        .map(|(_, limit)| *limit);
    on_provider.or(step.timeout_s).map_or(DEFAULT_TIMEOUT_S, |limit| u64::from(limit.get()))
}

/// The argv of `argv` for `host`: the one argv, or the host's own, else the default.
fn for_host(argv: &HostArgv, host: HostOs) -> Option<&Argv> {
    match argv {
        HostArgv::Every(a) => Some(a),
        HostArgv::PerHost(p) => match host {
            HostOs::Linux => p.linux.as_ref(),
            HostOs::Macos => p.macos.as_ref(),
            HostOs::Windows => p.windows.as_ref(),
        }
        .or(p.default.as_ref()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use isoloom_manifest::RunVar;

    use super::*;

    fn step(json: &str) -> Step {
        serde_json::from_str(json).unwrap()
    }

    fn scope(host: HostOs, provider: Option<&str>) -> Scope {
        Scope {
            host,
            provider: provider.map(str::to_owned),
        }
    }

    #[test]
    fn a_providers_own_time_limit_is_picked_by_the_runs_scope() {
        let status = step(r#"{"id": "status", "argv": ["vagrant", "status"], "timeout_s": 25, "provider_timeout_s": {"vmware_esxi": 120}}"#);
        assert_eq!(limit_s(&status, &scope(HostOs::Macos, Some("vmware_esxi"))), 120);
        assert_eq!(limit_s(&status, &scope(HostOs::Macos, Some("virtualbox"))), 25);
        assert_eq!(limit_s(&status, &scope(HostOs::Macos, None)), 25);
        let bare = step(r#"{"id": "s", "argv": ["vagrant", "status"]}"#);
        assert_eq!(limit_s(&bare, &scope(HostOs::Macos, Some("vmware_esxi"))), DEFAULT_TIMEOUT_S);
    }

    #[test]
    fn a_per_host_argv_falls_back_to_its_default() {
        let per: HostArgv = serde_json::from_str(r#"{"windows": ["where", "az"], "default": ["which", "az"]}"#).unwrap();
        assert_eq!(for_host(&per, HostOs::Windows).unwrap().program().as_str(), "where");
        assert_eq!(for_host(&per, HostOs::Linux).unwrap().program().as_str(), "which");
        let only: HostArgv = serde_json::from_str(r#"{"windows": ["where", "az"]}"#).unwrap();
        assert!(for_host(&only, HostOs::Macos).is_none());
    }

    #[test]
    fn a_command_is_filled_for_its_host() {
        let mut vars = Vars::default();
        vars.set_run(RunVar::Name, "flag-portal");
        vars.set_params(BTreeMap::from([("P".to_owned(), "1".to_owned())]));
        let argv: HostArgv =
            serde_json::from_str(r#"{"default": ["docker", "compose", "-p", "${run.name}", "up"], "windows": ["docker.exe", "${params.P}"]}"#).unwrap();
        assert_eq!(
            command(&argv, HostOs::Linux, &vars, "lifecycle.up.up").unwrap(),
            Command {
                program: "docker".into(),
                args: ["compose", "-p", "flag-portal", "up"].map(String::from).into()
            }
        );
        let windows = command(&argv, HostOs::Windows, &vars, "lifecycle.up.up").unwrap();
        assert_eq!((windows.program.as_str(), windows.args.as_slice()), ("docker.exe", ["1".to_owned()].as_slice()));
    }

    #[test]
    fn a_step_for_another_host_or_provider_does_not_apply() {
        let only_macos = step(r#"{"id": "relay", "hosts": ["macos"], "argv": ["true"]}"#);
        assert!(!applies(&scope(HostOs::Linux, None), &only_macos));
        assert!(applies(&scope(HostOs::Macos, None), &only_macos));
        let hyperv = step(r#"{"id": "switch", "providers": ["hyperv"], "argv": ["true"]}"#);
        assert!(!applies(&scope(HostOs::Windows, Some("virtualbox")), &hyperv));
        assert!(!applies(&scope(HostOs::Windows, None), &hyperv), "no provider");
        assert!(applies(&scope(HostOs::Windows, Some("hyperv")), &hyperv));
        let any = step(r#"{"id": "any", "argv": ["true"]}"#);
        assert!(applies(&scope(HostOs::Linux, None), &any));
    }

    #[test]
    fn an_unbound_placeholder_or_a_missing_host_argv_is_refused_where_it_is() {
        let vars = Vars::default();
        let unbound: HostArgv = serde_json::from_str(r#"["vagrant", "up", "--provider", "${run.provider}"]"#).unwrap();
        let err = command(&unbound, HostOs::Linux, &vars, "lifecycle.up.up").unwrap_err();
        assert_eq!(err.to_string(), "lifecycle.up.up: `${run.provider}` has no value in this operation");
        let windows_only: HostArgv = serde_json::from_str(r#"{"windows": ["where", "az"]}"#).unwrap();
        let refused = command(&windows_only, HostOs::Linux, &vars, "tools.w").unwrap_err();
        assert!(refused.to_string().contains("tools.w on this host"), "{refused}");
        let pin = step(r#"{"id": "pin", "action": "compose.pin_ports"}"#);
        assert_eq!(at("lifecycle.up", &pin), "lifecycle.up.pin");
    }
}
