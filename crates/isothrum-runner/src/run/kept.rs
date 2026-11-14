//! What a run keeps on this computer beside its machines, and when each thing goes. Each thing is
//! a `local` resource row of the home's store, written when the thing is ([`keep`]): its kind
//! says how long it holds ([`ResourceKind::lifetime`], never the caller), and every way a run's
//! machines go (`down`, the leftovers an `up` clears, a recovery's steps) forgets what that ties
//! to them and marks its rows gone, so a new kind of thing kept is placed here once and no path
//! can miss it. The seal and the last status are columns of the run's row, not things kept.

use std::path::Path;

use super::Running;
use crate::Result;
use crate::executor::Cx;
use crate::store::{Lifetime, NewResource, ResourceKind};

/// One thing a run keeps on this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kept {
    /// The state files the manifest names, written before the steps run.
    StateFiles,
    /// The machines' pinned SSH host keys and the SSH configuration naming them.
    HostKeys,
    /// The seal param recorded once the run was sealed (a column of the run's row).
    Seal,
    /// The archive of the run's project, copied to its machines.
    Archive,
    /// The machines a status saw last (a column of the run's row).
    Seen,
    /// The console ports and passwords the runner chose for the machines.
    Consoles,
}

impl Kept {
    /// Everything a run keeps, in the order a `down` forgets it.
    const ALL: [Kept; 6] = [Kept::StateFiles, Kept::HostKeys, Kept::Seal, Kept::Archive, Kept::Seen, Kept::Consoles];

    /// Its resource kind in the store; none for what the run's row holds itself.
    fn kind(self) -> Option<ResourceKind> {
        match self {
            Kept::StateFiles => Some(ResourceKind::StateFiles),
            Kept::HostKeys => Some(ResourceKind::HostKeys),
            Kept::Archive => Some(ResourceKind::Archive),
            Kept::Consoles => Some(ResourceKind::Consoles),
            Kept::Seal | Kept::Seen => None,
        }
    }

    /// How long it holds: its kind's lifetime; the run's row's columns, as long as the run.
    fn lifetime(self) -> Lifetime {
        self.kind().and_then(ResourceKind::lifetime).unwrap_or(Lifetime::Run)
    }
}

/// Records that `cx`'s run keeps a thing of `kind` at `path`, written by its operation: one live
/// row per kind and path (writing it again keeps the row). Nothing for a query, which writes
/// nothing kept.
pub(crate) fn keep(cx: &Cx<'_>, kind: ResourceKind, path: &Path) -> Result<()> {
    let (run, runner) = (cx.run.name(), cx.run.runner);
    let Some(by) = runner.operation_of(run) else {
        return Ok(());
    };
    let path = path.to_string_lossy();
    let db = runner.store.db();
    let made = db.write(|tx| {
        let live = tx.live_resources(run)?;
        if live.iter().any(|r| r.kind == kind && r.native_id.as_deref() == Some(&*path)) {
            return Ok(None);
        }
        let key = crate::encoding::wire_name(&kind);
        let id = tx.creating(&NewResource::new(run, &key, kind, by))?;
        tx.created(id, &path)?;
        Ok(Some(id))
    })?;
    super::recovery::say(cx.events, db, &made.into_iter().collect::<Vec<_>>());
    Ok(())
}

impl Running<'_> {
    /// Forgets `kept` for `cx`'s run, then marks its rows gone.
    fn forget(&self, cx: &Cx<'_>, kept: Kept) -> Result<()> {
        match kept {
            Kept::StateFiles => crate::executor::state_files::forget(cx)?,
            Kept::HostKeys => crate::ssh::forget(cx)?,
            Kept::Seal => self.forget_seal()?,
            Kept::Archive => crate::archive::forget(cx)?,
            // A status while the next start holds the run shows no machine of this one.
            Kept::Seen => self.runner.store.forget_seen(self.name())?,
            Kept::Consoles => crate::graphical::chosen::forget(cx)?,
        }
        let Some(kind) = kept.kind() else {
            return Ok(());
        };
        let by = self.runner.operation_of(self.name());
        let db = self.runner.store.db();
        let gone = db.write(|tx| {
            let mut gone = Vec::new();
            for row in tx.live_resources(self.name())?.into_iter().filter(|r| r.kind == kind) {
                if let Some(by) = by {
                    tx.destroying(row.id, by)?;
                }
                tx.gone(row.id)?;
                gone.push(row.id);
            }
            Ok(gone)
        })?;
        super::recovery::say(cx.events, db, &gone);
        Ok(())
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
    use super::{Kept, Lifetime, ResourceKind};

    #[test]
    fn what_was_learned_from_the_machines_goes_with_them_and_the_rest_with_the_run() {
        let with_machines: Vec<Kept> = Kept::ALL.into_iter().filter(|k| k.lifetime() == Lifetime::Machines).collect();
        assert_eq!(with_machines, [Kept::HostKeys]);
        for (i, k) in Kept::ALL.iter().enumerate() {
            assert!(!Kept::ALL[..i].contains(k), "{k:?} listed twice");
        }
    }

    #[test]
    fn a_kept_things_lifetime_is_its_kinds_never_the_callers() {
        for k in Kept::ALL {
            if let Some(kind) = k.kind() {
                assert_eq!(kind.lifetime(), Some(k.lifetime()), "{k:?}");
            }
        }
        assert_eq!(ResourceKind::HostKeys.lifetime(), Some(Lifetime::Machines));
    }
}
