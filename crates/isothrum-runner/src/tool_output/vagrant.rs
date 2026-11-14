//! What Vagrant prints, read: `vagrant status --machine-readable` (a state kept whole as printed,
//! then read as a [`MachineState`]), `vagrant ssh-config`, `vagrant plugin list
//! --machine-readable`, the `error-exit` record a machine-readable call fails with, and the
//! failures a driver acts on ([`Failure`]). Each wording is backed by a fixture of the text as the
//! named Vagrant or plugin version prints it (`fixtures/vagrant/`).

use std::collections::BTreeMap;

use crate::status::MachineState;

/// A status read: each machine's state by its define name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Reading {
    /// Each machine's state label, as printed.
    pub(crate) states: BTreeMap<String, String>,
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

/// The address `vagrant ssh-config <vm>` gives its VM (`HostName`): on ESXi, the one its VMware
/// Tools report.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "a VM's address in the status (the Vagrant driver's gap (b): ESXi publishes no port here) reads it"
    )
)]
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
/// VM answers. ESXi (`vagrant-vmware-esxi`) calls a VM `powered_on` until it has its SSH
/// address, `running` after: both run (a Windows guest before its tools report an address read as
/// unknown and never ready).
pub(crate) fn machine_state(label: &str) -> MachineState {
    match label {
        "running" | "started" | "powered_on" => MachineState::Running,
        "not_created" => MachineState::Absent,
        "poweroff" | "powered_off" | "off" | "shutoff" | "stopped" | "not_running" | "saved" | "suspended" | "paused" | "pmsuspended" | "aborted"
        | "aborted-saved" => MachineState::Stopped,
        other => MachineState::Unknown(other.to_owned()),
    }
}

/// Each plugin `vagrant plugin list --machine-readable` names (its `plugin-name` lines), with its
/// version when a `plugin-version` line gives one (`0.12.2%!(VAGRANT_COMMA) global`: the version
/// before Vagrant's escaped comma).
#[must_use]
pub fn plugins(out: &str) -> Vec<(String, Option<String>)> {
    let lines: Vec<(&str, &str, &str)> = out.lines().filter_map(fields).collect();
    let version = |name: &str| {
        lines
            .iter()
            .find(|(target, kind, _)| *target == name && *kind == "plugin-version")
            .and_then(|(_, _, data)| {
                let v = data.split("%!(VAGRANT_COMMA)").next().unwrap_or_default().trim();
                (!v.is_empty()).then(|| v.to_owned())
            })
    };
    lines
        .iter()
        .filter(|(_, kind, data)| *kind == "plugin-name" && !data.is_empty())
        .map(|(_, _, name)| ((*name).to_owned(), version(name)))
        .collect()
}

/// The error a Vagrant call printed as its `--machine-readable` `error-exit` record among `stdout`
/// (its last, when there are more): its message as Vagrant words it for a person, followed by
/// its class (`… because it is locked! … (Vagrant::Errors::MachineLocked)`). With
/// `--machine-readable`, Vagrant prints its error there, on standard output, and nothing of it on
/// standard error.
pub(crate) fn error(stdout: &[String]) -> Option<String> {
    let data = stdout
        .iter()
        .rev()
        .filter_map(|line| fields(line))
        .find_map(|(_, kind, data)| (kind == "error-exit").then_some(data))?;
    let (class, message) = data.split_once(',').unwrap_or((data, ""));
    let (class, message) = (unescape(class), unescape(message));
    match (class.trim(), message.trim()) {
        ("", "") => None,
        (class, "") => Some(class.to_owned()),
        ("", message) => Some(message.to_owned()),
        (class, message) => Some(format!("{message} ({class})")),
    }
}

/// What Vagrant says when another process holds a machine's lock, its action lock or its index
/// lock: each error's class, which `--machine-readable` prints in its `error-exit` record, and its
/// message, which a human-readable call prints on standard error (the record repeats it too).
const VAGRANT_LOCKED: [&str; 4] = [
    "Vagrant::Errors::MachineActionLockedError",
    "another process is already executing an action on the machine",
    "Vagrant::Errors::MachineLocked,",
    "Vagrant can't use the requested machine because it is locked!",
];

/// What vagrant-vmware-esxi says when the VM it is asked to halt is already off: ESXi refuses the
/// guest shutdown (`vim.fault.InvalidPowerState`, the VM `poweredOff`).
const ESXI_ALREADY_OFF: [&str; 2] = ["existingState = \"poweredOff\"", "in the current state (Powered off)"];

/// Whether any of `lines` holds any of `wordings`.
fn said_any<'a>(lines: impl IntoIterator<Item = &'a String>, wordings: &[&str]) -> bool {
    lines.into_iter().any(|line| wordings.iter().any(|said| line.contains(said)))
}

/// A `--machine-readable` field as Vagrant wrote it, its escapes undone: a comma written as
/// `%!(VAGRANT_COMMA)`, a line break as `\n`, a carriage return as `\r`.
fn unescape(field: &str) -> String {
    field.replace("%!(VAGRANT_COMMA)", ",").replace("\\r", "").replace("\\n", "\n")
}

/// One line of Vagrant's `--machine-readable` output: its target, type and data (the rest of the
/// line). Lines are `timestamp,target,type,data…`; Vagrant writes a comma in a field as
/// `%!(VAGRANT_COMMA)`, so the data is kept whole as printed.
pub(crate) fn fields(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.trim_end_matches('\r').splitn(4, ',');
    let (_ts, target, kind, data) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    Some((target, kind, data))
}

/// Why a Vagrant call failed, as far as a driver acts on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    /// Another process held the machine's lock, its action lock or the index lock
    /// ([`VAGRANT_LOCKED`]): the same call goes through once it is released.
    Locked,
    /// ESXi refused to halt a VM that was already off ([`ESXI_ALREADY_OFF`]).
    AlreadyOff,
    /// Any other failure.
    Other,
}

impl Failure {
    /// What a failed call that printed `lines` (its standard output and error) failed with.
    pub(crate) fn of<'a>(lines: impl IntoIterator<Item = &'a String> + Clone) -> Failure {
        if said_any(lines.clone(), &VAGRANT_LOCKED) {
            Failure::Locked
        } else if said_any(lines, &ESXI_ALREADY_OFF) {
            Failure::AlreadyOff
        } else {
            Failure::Other
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_is_read_in_each_of_vagrant_s_wordings() {
        // As Vagrant 2.4 prints them: the action lock (machine-readable, then human), the index lock.
        let action = lines(
            "1794307174,,error-exit,Vagrant::Errors::MachineActionLockedError,An action 'read_state' was attempted on the machine 'web'%!(VAGRANT_COMMA)\\nbut another process is already executing an action on the machine.",
        );
        let human = lines(
            "An action 'read_state' was attempted on the machine 'web',\nbut another process is already executing an action on the machine.\nVagrant locks each machine for access by only one process at a time.",
        );
        let index = lines("1794342958,,error-exit,Vagrant::Errors::MachineLocked,Vagrant can't use the requested machine because it is locked! This");
        assert!([&action, &human, &index].iter().all(|said| Failure::of(said.iter()) == Failure::Locked));
        assert_eq!(
            Failure::of(&lines("VBoxManage: error: Cannot unregister the machine 'web-vm - web' while it is locked")),
            Failure::Other
        );
        assert_eq!(Failure::of(&Vec::new()), Failure::Other);
    }

    #[test]
    fn esxi_refusing_to_halt_a_vm_already_off_is_read() {
        // As vagrant-vmware-esxi prints it.
        let off = lines(
            "There was an error talking to ESXi.\n  Unable to shutdown the VM:\n    (vim.fault.InvalidPowerState) {\n   existingState = \"poweredOff\"\n   msg = \"shutdownGuest\nThe attempted operation cannot be performed in the current state (Powered off).\"\n}",
        );
        assert_eq!(Failure::of(&off), Failure::AlreadyOff);
        assert_eq!(
            Failure::of(&lines("There was an error talking to ESXi.\n  Unable to shutdown the VM: timed out")),
            Failure::Other
        );
    }

    #[test]
    fn plugins_are_read_by_name_with_their_versions() {
        // As Vagrant 2.4 prints them (a `ui` line repeats the human list).
        let out = "1794305819,,ui,info,vagrant-qemu (0.7.0%!(VAGRANT_COMMA) global)\n\
                   1794305819,,plugin-name,vagrant-qemu\n\
                   1794305819,vagrant-qemu,plugin-version,0.7.0%!(VAGRANT_COMMA) global\n\
                   1794305819,,plugin-name,vagrant-vmware-esxi\r\n\
                   1794305819,,plugin-name,vagrant_utm\n\
                   1794305819,vagrant_utm,plugin-version,0.2.0\n";
        assert_eq!(
            plugins(out),
            [
                ("vagrant-qemu".to_owned(), Some("0.7.0".to_owned())),
                ("vagrant-vmware-esxi".to_owned(), None),
                ("vagrant_utm".to_owned(), Some("0.2.0".to_owned())),
            ]
        );
        assert_eq!(plugins(""), Vec::<(String, Option<String>)>::new());
    }

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn a_machine_readable_error_is_read_whole_with_its_class() {
        // As Vagrant 2.4.9 prints it on libvirt while another process holds the machine's lock.
        let said = "Vagrant can't use the requested machine because it is locked! This\\nmeans that another Vagrant \
                    process is currently reading or modifying\\nthe machine.\\n\\nName: probe\\nProvider: libvirt";
        let out = lines(&format!(
            "1794342958,,ui,error,{said}\n1794342958,,error-exit,Vagrant::Errors::MachineLocked,{said}\n"
        ));
        assert_eq!(
            error(&out).as_deref(),
            Some(
                "Vagrant can't use the requested machine because it is locked! This\nmeans that another Vagrant \
                 process is currently reading or modifying\nthe machine.\n\nName: probe\nProvider: libvirt \
                 (Vagrant::Errors::MachineLocked)"
            )
        );
        let commas = lines("1,,error-exit,Vagrant::Errors::VagrantError,a%!(VAGRANT_COMMA) b\\r\\n");
        assert_eq!(error(&commas).as_deref(), Some("a, b (Vagrant::Errors::VagrantError)"));
        assert_eq!(error(&lines("1,,error-exit,Vagrant::Errors::X,")).as_deref(), Some("Vagrant::Errors::X"));
        assert_eq!(error(&lines("1,web,state,running\n==> web: up")), None);
        assert_eq!(error(&lines("1,,error-exit,")), None);
    }

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
        assert_eq!(machine_state("powered_on"), MachineState::Running, "ESXi's VM before its SSH address");
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
            drop(plugins(&text));
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

    fn fixture_lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    /// Each state Vagrant prints, read from what its versions print: Linux's and Windows' line
    /// ends alike, and a listing cut in the middle of a state read as unknown, never absent.
    #[test]
    fn status_fixtures_read_as_vagrant_prints_them() {
        let read = parse(include_str!("fixtures/vagrant/2.4.1-status-virtualbox.txt"));
        assert_eq!(read.states.keys().collect::<Vec<_>>(), ["db", "web"]);
        assert_eq!(
            (machine_state(&read.states["web"]), machine_state(&read.states["db"])),
            (MachineState::Running, MachineState::Stopped)
        );
        let hyperv = parse(include_str!("fixtures/vagrant/2.4.3-windows-status-hyperv.txt"));
        assert_eq!(
            machine_state(&hyperv.states["dc01"]),
            MachineState::Stopped,
            "no carriage return left in the label"
        );
        assert_eq!(machine_state(&hyperv.states["ws01"]), MachineState::Absent);
        let cut = parse(include_str!("fixtures/vagrant/2.4.1-status-cut.txt"));
        assert_eq!(machine_state(&cut.states["db"]), MachineState::Unknown("shut".into()));
        assert_eq!(parse("").states.len(), 0);
        let ssh = include_str!("fixtures/vagrant/2.4.1-ssh-config-windows.txt");
        assert_eq!(host_name(ssh).as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn failure_fixtures_read_as_vagrant_prints_them() {
        let of = |text: &str| Failure::of(&fixture_lines(text));
        assert_eq!(of(include_str!("fixtures/vagrant/2.4.9-error-machine-locked.txt")), Failure::Locked);
        assert_eq!(of(include_str!("fixtures/vagrant/2.4.1-error-action-locked-human.txt")), Failure::Locked);
        assert_eq!(of(include_str!("fixtures/vagrant/vmware-esxi-2.5.5-halt-already-off.txt")), Failure::AlreadyOff);
        assert_eq!(of(include_str!("fixtures/vagrant/2.4.1-status-virtualbox.txt")), Failure::Other);
        let locked = fixture_lines(include_str!("fixtures/vagrant/2.4.9-error-machine-locked.txt"));
        assert!(error(&locked).is_some_and(|e| e.ends_with("Provider: libvirt (Vagrant::Errors::MachineLocked)")));
        let plugins = plugins(include_str!("fixtures/vagrant/2.4.1-plugin-list.txt"));
        assert_eq!(
            plugins,
            [
                ("vagrant-libvirt".to_owned(), Some("0.12.2".to_owned())),
                ("vagrant-qemu".to_owned(), Some("0.3.12".to_owned()))
            ]
        );
    }
}
