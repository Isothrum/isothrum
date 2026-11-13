//! What a run keeps on this computer beside its machines, and when each thing goes. Every way a
//! run's machines go (`down`, the leftovers an `up` clears, a recovery's steps) forgets what this
//! list ties to them, so a new kind of thing kept is placed here once and no path can miss it.

use super::Running;
use crate::Result;
use crate::executor::Cx;

/// One thing a run keeps on this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kept {
    /// The state files the manifest names, written before the steps run.
    StateFiles,
    /// The machines' pinned SSH host keys and the SSH configuration naming them.
    HostKeys,
    /// The seal param recorded once the run was sealed.
    Seal,
    /// The archive of the run's project, copied to its machines.
    Archive,
    /// The machines a status saw last.
    Seen,
    /// The console ports and passwords the runner chose for the machines.
    Consoles,
}

/// How long a [`Kept`] thing holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifetime {
    /// As long as the machines it was learned from: made again (at the same addresses, with new
    /// host keys), they make it wrong.
    Machines,
    /// As long as the run: chosen or recorded before its machines are made, so a start that makes
    /// them again keeps it; only a `down` forgets it.
    Run,
}

impl Kept {
    /// Everything a run keeps, in the order a `down` forgets it.
    const ALL: [Kept; 6] = [Kept::StateFiles, Kept::HostKeys, Kept::Seal, Kept::Archive, Kept::Seen, Kept::Consoles];

    fn lifetime(self) -> Lifetime {
        match self {
            Kept::HostKeys => Lifetime::Machines,
            // The consoles and the state files are chosen and written before an `up` clears
            // leftovers; the seal, the archive and the last status belong to the run's record.
            Kept::StateFiles | Kept::Seal | Kept::Archive | Kept::Seen | Kept::Consoles => Lifetime::Run,
        }
    }
}

impl Running<'_> {
    /// Forgets `kept` for `cx`'s run.
    fn forget(&self, cx: &Cx<'_>, kept: Kept) -> Result<()> {
        match kept {
            Kept::StateFiles => crate::executor::state_files::forget(cx),
            Kept::HostKeys => crate::ssh::forget(cx),
            Kept::Seal => self.forget_seal(),
            Kept::Archive => crate::archive::forget(cx),
            // A status while the next start holds the run shows no machine of this one.
            Kept::Seen => self.runner.store.forget_seen(self.name()),
            Kept::Consoles => crate::graphical::chosen::forget(cx),
        }
    }

    /// After the run's machines were removed to be made again (the leftovers an `up` clears, a
    /// recovery's steps): forgets what was learned from them.
    pub(super) fn machines_gone(&self, cx: &Cx<'_>) -> Result<()> {
        Kept::ALL
            .into_iter()
            .filter(|k| k.lifetime() == Lifetime::Machines)
            .try_for_each(|k| self.forget(cx, k))
    }

    /// After a successful `down`: forgets everything the run kept.
    pub(super) fn run_gone(&self, cx: &Cx<'_>) -> Result<()> {
        Kept::ALL.into_iter().try_for_each(|k| self.forget(cx, k))
    }
}

#[cfg(test)]
mod tests {
    use super::{Kept, Lifetime};

    #[test]
    fn what_was_learned_from_the_machines_goes_with_them_and_the_rest_with_the_run() {
        let with_machines: Vec<Kept> = Kept::ALL.into_iter().filter(|k| k.lifetime() == Lifetime::Machines).collect();
        assert_eq!(with_machines, [Kept::HostKeys]);
        for (i, k) in Kept::ALL.iter().enumerate() {
            assert!(!Kept::ALL[..i].contains(k), "{k:?} listed twice");
        }
    }
}
