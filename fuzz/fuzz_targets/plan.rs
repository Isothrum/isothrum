//! The runner's step planning on any manifest: every step of a manifest that parses is planned on
//! every host and on every provider Isoloom names (and none), elsewhere, a command, an action, or
//! refused with why, never a panic, the same twice; and with every run value bound, no step is
//! refused for a `${run.…}` one.

#![no_main]

use isothrum_runner::fuzzing::{Planned, Values, plan};
use isothrum_runner::isoloom_manifest::{self, HostOs, Provider, RunVar};
use isothrum_runner::{every_name, from_wire_name};
use libfuzzer_sys::fuzz_target;

/// The run value after `var`, from [`RunVar::Name`] on. The match has no catch-all: a value Isoloom
/// adds fails this target's build until it is in the chain too.
const fn after(var: RunVar) -> Option<RunVar> {
    match var {
        RunVar::Name => Some(RunVar::Dir),
        RunVar::Dir => Some(RunVar::StateDir),
        RunVar::StateDir => Some(RunVar::Provider),
        RunVar::Provider => Some(RunVar::SshKey),
        RunVar::SshKey => Some(RunVar::KnownHosts),
        RunVar::KnownHosts => Some(RunVar::SshPublicKey),
        RunVar::SshPublicKey => Some(RunVar::Slot),
        RunVar::Slot => None,
    }
}

/// Every run value, bound to a plain word.
fn run_values() -> Values {
    let mut values = Values::default();
    for var in std::iter::successors(Some(RunVar::Name), |v| after(*v)) {
        values.run(var, "x");
    }
    values
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let Ok(manifest) = isoloom_manifest::parse(text) else { return };
    let values = run_values();
    let providers: Vec<Option<Provider>> = std::iter::once(None)
        .chain(every_name::<Provider>().iter().filter_map(|n| from_wire_name(n)).map(Some))
        .collect();
    for host in [HostOs::Linux, HostOs::Macos, HostOs::Windows] {
        for provider in &providers {
            let planned = plan(&manifest, host, *provider, &values);
            assert_eq!(planned, plan(&manifest, host, *provider, &values), "planning is deterministic");
            for (at, step) in &planned {
                match step {
                    Planned::Command { program, .. } => assert!(!program.is_empty(), "{at}: a command with no program"),
                    Planned::Refused(why) => assert!(!why.contains("`${run."), "{at}: a bound run value refused: {why}"),
                    _ => {}
                }
            }
        }
    }
});
