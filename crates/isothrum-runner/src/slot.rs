//! A run's slot on its place (`${run.slot}`, 1 to 99): Isoloom's server modules name everything
//! they create after it (`iso<slot>-web`, the SDN zone `iso<slot>`, the VNet names `i<slot>n0`), so
//! two runs on one Proxmox server must never share one. The runner gives each run the lowest slot
//! no other live run on the same place (the embedder's `host_label`) holds, keeps it in the run's
//! record from the first start to the `down`, and gives it back the same at every operation (a
//! destroy goes to the names the apply made). Isoloom's modules defaulted it to 1 and nothing set
//! it, so a second run on a server collided with the first.

use std::collections::BTreeSet;

use isoloom_manifest::{Manifest, Part, Placeholder, RunVar};

use crate::Result;
use crate::error::Error;
use crate::name::RunName;
use crate::state::RunState;
use crate::store::RunRecord;

/// The highest slot.
const LAST: u8 = 99;

/// Whether a run of `manifest` needs a slot: a template names `${run.slot}`.
pub(crate) fn needed(manifest: &Manifest) -> bool {
    manifest
        .templates()
        .iter()
        .any(|(_, t)| t.parts().iter().any(|p| matches!(p, Part::Placeholder(Placeholder::Run(RunVar::Slot)))))
}

/// The slots `records` hold on `place`, `me` aside: those of runs not gone (`absent`).
pub(crate) fn taken(records: &[RunRecord], me: &RunName, place: Option<&str>) -> BTreeSet<u8> {
    records
        .iter()
        .filter(|r| &r.name != me && r.host_label.as_deref() == place && r.state != RunState::Absent)
        .filter_map(|r| r.slot)
        .collect()
}

/// The run's slot: the one it holds when no other run took it since, else the lowest free one.
pub(crate) fn pick(own: Option<u8>, taken: &BTreeSet<u8>, place: Option<&str>) -> Result<u8> {
    if let Some(slot) = own.filter(|s| (1..=LAST).contains(s) && !taken.contains(s)) {
        return Ok(slot);
    }
    (1..=LAST).find(|s| !taken.contains(s)).ok_or_else(|| Error::SlotsFull {
        place: place.unwrap_or("this host").to_owned(),
        held: taken.len(),
    })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn record(name: &str, place: Option<&str>, slot: Option<u8>, state: RunState) -> RunRecord {
        let mut r = RunRecord::fresh(
            RunName::new(name).unwrap(),
            "/l/run.json".into(),
            isoloom_manifest::Target::Proxmox,
            isoloom_manifest::Driver::Terraform,
        );
        r.host_label = place.map(String::from);
        r.slot = slot;
        r.state = state;
        r
    }

    #[test]
    fn a_run_gets_the_lowest_slot_the_others_on_its_place_dont_hold() {
        // Regression: the modules' `slot` defaulted to 1 and was never set, so a second run on a
        // server took the first one's names.
        let me = RunName::new("b").unwrap();
        let records = [
            record("a", Some("pve"), Some(1), RunState::Ready),
            record("c", Some("pve"), Some(3), RunState::Failed),
            record("d", Some("pve"), Some(2), RunState::Absent),
            record("e", Some("other"), Some(2), RunState::Ready),
            record("b", Some("pve"), Some(1), RunState::Absent),
        ];
        let taken = taken(&records, &me, Some("pve"));
        assert_eq!(taken, BTreeSet::from([1, 3]));
        assert_eq!(pick(None, &taken, Some("pve")).unwrap(), 2);
        assert_eq!(pick(Some(5), &taken, Some("pve")).unwrap(), 5, "a run keeps its own");
        assert_eq!(pick(Some(1), &taken, Some("pve")).unwrap(), 2, "unless another took it since");
    }

    #[test]
    fn a_full_place_is_refused_with_its_name() {
        let all: BTreeSet<u8> = (1..=LAST).collect();
        let e = pick(None, &all, Some("pve")).unwrap_err();
        assert_eq!(e.code(), crate::ErrorCode::SlotsFull);
        assert_eq!(e.to_string(), "no slot is free on `pve`: 99 runs hold one; stop one first");
    }

    proptest! {
        #[test]
        fn a_picked_slot_is_free_and_in_range(taken in proptest::collection::btree_set(1u8..=99, 0..98), own in proptest::option::of(0u8..=120)) {
            let s = pick(own, &taken, None).unwrap();
            prop_assert!((1..=LAST).contains(&s) && !taken.contains(&s));
        }
    }
}
