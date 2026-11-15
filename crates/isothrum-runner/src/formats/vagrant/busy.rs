//! What a status shows of the machines another process acts on (a start, a provision from a
//! terminal), whose locks refused the read past its patience: busy, the run's other machines as
//! last read (the core keeps those).

use super::{MachineState, Seen, SeenMachine};

/// `machines`, busy: what a status shows of them while another process acts on them.
pub(super) fn busy(machines: Vec<String>) -> Seen {
    let busy = |name| SeenMachine {
        name,
        state: MachineState::Busy,
        published: Vec::new(),
        kind: None,
        address: None,
        windows: false,
    };
    Seen {
        machines: machines.into_iter().map(busy).collect(),
        ..Seen::default()
    }
}
