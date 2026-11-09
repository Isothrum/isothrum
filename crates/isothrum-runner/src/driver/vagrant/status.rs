//! What `vagrant status --machine-readable` and `vagrant plugin list --machine-readable` say.
//! Lines are `timestamp,target,type,data…`; Vagrant writes a comma in a field as
//! `%!(VAGRANT_COMMA)`, so a state is kept whole as printed.

use std::collections::BTreeMap;

use crate::status::MachineState;

/// A status read: each machine's state by its define name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Reading {
    /// Each machine's state label, as printed.
    pub(crate) states: BTreeMap<String, String>,
}

/// One machine-readable line: its target, type and data (the rest of the line).
fn fields(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.trim_end_matches('\r').splitn(4, ',');
    let (_ts, target, kind, data) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    Some((target, kind, data))
}

/// `vagrant status --machine-readable`'s output, read.
pub(crate) fn parse(out: &str) -> Reading {
    let mut read = Reading::default();
    for (target, kind, data) in out.lines().filter_map(fields) {
        if kind == "state" && !target.is_empty() {
            read.states.insert(target.to_owned(), data.to_owned());
        }
    }
    read
}

/// The plugins `vagrant plugin list --machine-readable` names (`plugin-name` lines).
pub(crate) fn plugins(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(fields)
        .filter(|(_, kind, data)| *kind == "plugin-name" && !data.is_empty())
        .map(|(_, _, data)| data.to_owned())
        .collect()
}

/// The address `vagrant ssh-config <vm>` gives its VM (`HostName`): on ESXi, the one its VMware
/// Tools report.
pub(crate) fn host_name(out: &str) -> Option<String> {
    out.lines()
        .filter_map(|line| line.trim().strip_prefix("HostName"))
        .map(str::trim)
        .find(|host| !host.is_empty())
        .map(str::to_owned)
}

/// A Vagrant state label as a machine state, every provider's labels included: a VM that is
/// there but doesn't run is stopped (powered off, suspended, saved, paused), one never created
/// or destroyed is absent; any other label is kept as it is. UTM (`vagrant_utm`) calls a running
/// VM `started`: it has no running state of its own, so `started` is what it reports once the
/// VM answers.
pub(crate) fn machine_state(label: &str) -> MachineState {
    match label {
        "running" | "started" => MachineState::Running,
        "not_created" => MachineState::Absent,
        "poweroff" | "powered_off" | "off" | "shutoff" | "stopped" | "not_running" | "saved" | "suspended" | "paused" | "pmsuspended" | "aborted"
        | "aborted-saved" => MachineState::Stopped,
        other => MachineState::Unknown(other.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_configs_host_name_is_the_vms_address() {
        let out = "Host web\r\n  HostName 172.16.1.53\r\n  User vagrant\r\n  Port 22\r\n";
        assert_eq!(host_name(out).as_deref(), Some("172.16.1.53"));
        assert_eq!(host_name("Host web\n  HostName \n  User vagrant\n"), None);
        assert_eq!(host_name(""), None);
    }

    #[test]
    fn reads_machine_readable_status() {
        let out = "1700000000,pfsense-1,metadata,provider,virtualbox\n\
                   1700000000,pfsense-1,provider-name,virtualbox\n\
                   1700000000,pfsense-1,state,running\n\
                   1700000000,debian-1,state,poweroff\n\
                   1700000000,,ui,info,Current machine states:\n";
        let r = parse(out);
        assert_eq!(r.states.len(), 2);
        assert_eq!(r.states["pfsense-1"], "running");
        assert_eq!(r.states["debian-1"], "poweroff");
    }

    #[test]
    fn windows_line_endings_and_blank_lines_are_read_the_same() {
        let out = "1,dc01,provider-name,hyperv\r\n\r\n1,dc01,state,running\r\n1,isoloom-controller,state,off\r\n";
        let r = parse(out);
        assert_eq!(r.states["dc01"], "running", "no carriage return in the state");
        assert_eq!(machine_state(&r.states["isoloom-controller"]), MachineState::Stopped);
    }

    #[test]
    fn a_state_with_vagrants_comma_escape_is_kept_whole_and_is_not_running() {
        let r = parse("1,a,state,running%!(VAGRANT_COMMA) with a note\n");
        assert_eq!(r.states["a"], "running%!(VAGRANT_COMMA) with a note");
        assert!(matches!(machine_state(&r.states["a"]), MachineState::Unknown(_)));
    }

    #[test]
    fn non_state_lines_and_malformed_output_give_no_machine() {
        let out = "ts,,ui,info,Current machine states:\nts,box,metadata,provider,virtualbox\nts,,state,running\nshort,line\n";
        assert!(parse(out).states.is_empty());
        assert!(parse("").states.is_empty());
    }

    #[test]
    fn every_providers_labels_map_to_a_state() {
        assert_eq!(machine_state("running"), MachineState::Running);
        assert_eq!(machine_state("started"), MachineState::Running, "UTM's running VM");
        assert_eq!(machine_state("not_created"), MachineState::Absent);
        for stopped in [
            "poweroff",
            "saved",
            "aborted",
            "aborted-saved",
            "suspended",
            "shutoff",
            "off",
            "not_running",
            "powered_off",
            "stopped",
            "paused",
        ] {
            assert_eq!(machine_state(stopped), MachineState::Stopped, "{stopped}");
        }
        assert_eq!(machine_state("gurumeditation"), MachineState::Unknown("gurumeditation".into()));
    }

    #[test]
    fn plugins_are_read_by_name() {
        let out = "1,,plugin-name,vagrant-libvirt\n1,vagrant-libvirt,plugin-version,0.12.2%!(VAGRANT_COMMA) global\n1,,plugin-name,vagrant-qemu\n";
        assert_eq!(plugins(out), ["vagrant-libvirt", "vagrant-qemu"]);
        assert_eq!(plugins(""), Vec::<String>::new());
    }
}

#[cfg(test)]
mod proptests {
    use proptest::prelude::*;

    use super::*;

    /// A machine name (no comma: machine-readable output is comma-separated).
    fn machine() -> impl Strategy<Value = String> {
        prop_oneof![Just("isoloom-controller".to_owned()), "[a-z][a-z0-9_-]{0,12}"]
    }

    /// A state label as Vagrant prints it, for any provider.
    fn state() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("running".to_owned()),
            Just("poweroff".to_owned()),
            Just("not_created".to_owned()),
            "[a-z_ ,]{1,16}"
        ]
    }

    proptest! {
        #[test]
        fn any_output_is_read_without_panicking(text in "(?s).{0,400}") {
            let _ = parse(&text);
            let _ = plugins(&text);
        }

        #[test]
        fn every_machines_last_state_reads_back_whole(
            machines in proptest::collection::vec((machine(), state(), "[0-9]{10}"), 0..6),
            noise in proptest::collection::vec("[0-9]{10},[a-z]{0,6},(metadata|ui),[^\n]{0,20}", 0..4),
        ) {
            let mut lines: Vec<String> = machines.iter().map(|(name, state, ts)| format!("{ts},{name},state,{state}")).collect();
            lines.extend(noise);
            let read = parse(&lines.join("\n"));
            let mut want = BTreeMap::new();
            for (name, state, _) in &machines {
                want.insert(name.clone(), state.clone());
            }
            prop_assert_eq!(read.states, want);
        }
    }
}
