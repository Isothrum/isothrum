//! What a setup installed, on record, so a clean-up removes what it installed and only that: a
//! tool that was there before its install is never recorded, and one whose install only opened its
//! vendor's installer (a macOS cask), or which isn't found yet, counts once a later check finds it.
//! The rules are here; the caller keeps the records where it keeps its data, and reads and writes
//! them in one transaction around [`remember`] and [`settle`].

use serde::{Deserialize, Serialize};

use super::Dependency;

/// A tool a setup installed, on record.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InstalledTool {
    /// The tool.
    pub dependency: Dependency,
    /// When it was installed, in seconds since the Unix epoch.
    pub at: u64,
    /// The install only opened the vendor's own installer, or the tool isn't found yet: it counts
    /// as installed once a later check finds it ([`settle`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
}

/// How long an opened installer may take to be finished before the record is dropped: a tool that
/// shows up later than that was most likely installed some other way.
pub const PENDING_FOR_SECS: u64 = 24 * 3600;

/// What an install leaves on record.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Outcome {
    /// Already there before: someone else installed it, so a clean-up must never remove it.
    NotOurs,
    /// Missing before, found now: the setup installed it.
    Installed,
    /// Missing before and still not found (an installer left open, `PATH` not refreshed yet):
    /// confirmed by a later check.
    Pending,
}

/// What an install leaves on record, from whether the tool was there before it (`present_before`)
/// and after it (`present_after`).
#[must_use]
pub fn outcome(present_before: bool, present_after: bool) -> Outcome {
    match (present_before, present_after) {
        (true, _) => Outcome::NotOurs,
        (false, true) => Outcome::Installed,
        (false, false) => Outcome::Pending,
    }
}

/// `tools` with `dependency` recorded once as the install's `outcome` says, at `now`: installed,
/// or waiting for its installer (one not ours is not recorded). A tool already confirmed stays
/// confirmed.
pub fn remember(tools: &mut Vec<InstalledTool>, dependency: Dependency, outcome: Outcome, now: u64) {
    let waiting = match outcome {
        Outcome::NotOurs => return,
        Outcome::Installed => false,
        Outcome::Pending => true,
    };
    let pending = waiting && !tools.iter().any(|t| t.dependency == dependency && !t.pending);
    tools.retain(|t| t.dependency != dependency);
    tools.push(InstalledTool { dependency, at: now, pending });
}

/// Confirms the pending records whose tool is now `found`, and drops those pending for longer than
/// [`PENDING_FOR_SECS`] at `now`.
pub fn settle(tools: &mut Vec<InstalledTool>, found: &[Dependency], now: u64) {
    for t in tools.iter_mut().filter(|t| t.pending && found.contains(&t.dependency)) {
        t.pending = false;
    }
    tools.retain(|t| !t.pending || now.saturating_sub(t.at) < PENDING_FOR_SECS);
}

/// The confirmed records only: what a clean-up may remove.
#[must_use]
pub fn confirmed(tools: Vec<InstalledTool>) -> Vec<InstalledTool> {
    tools.into_iter().filter(|t| !t.pending).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_install_is_ours_only_when_the_tool_was_missing_before() {
        assert_eq!(outcome(true, true), Outcome::NotOurs);
        assert_eq!(outcome(true, false), Outcome::NotOurs);
        assert_eq!(outcome(false, true), Outcome::Installed);
        assert_eq!(outcome(false, false), Outcome::Pending);
    }

    #[test]
    fn a_tool_is_recorded_once_and_a_confirmed_one_stays_confirmed() {
        let mut tools = Vec::new();
        remember(&mut tools, Dependency::Docker, Outcome::NotOurs, 10);
        assert_eq!(tools, []);
        remember(&mut tools, Dependency::Docker, Outcome::Installed, 10);
        remember(&mut tools, Dependency::Docker, Outcome::Pending, 20);
        assert_eq!(
            tools,
            [InstalledTool {
                dependency: Dependency::Docker,
                at: 20,
                pending: false
            }]
        );
        remember(&mut tools, Dependency::Utm, Outcome::Pending, 30);
        assert_eq!(confirmed(tools.clone()).len(), 1, "a pending install isn't removable yet");
    }

    #[test]
    fn a_pending_install_is_confirmed_once_found_and_dropped_once_stale() {
        let pending = |dependency, at| InstalledTool { dependency, at, pending: true };
        let mut tools = vec![
            pending(Dependency::Utm, 0),
            pending(Dependency::Vagrant, 0),
            pending(Dependency::Docker, PENDING_FOR_SECS),
        ];
        settle(&mut tools, &[Dependency::Utm], PENDING_FOR_SECS);
        assert_eq!(
            tools,
            [
                InstalledTool {
                    dependency: Dependency::Utm,
                    at: 0,
                    pending: false
                },
                pending(Dependency::Docker, PENDING_FOR_SECS)
            ]
        );
        let json = serde_json::to_value(&tools).unwrap();
        assert_eq!(json[0], serde_json::json!({ "dependency": "utm", "at": 0 }), "confirmed: no pending key");
        assert_eq!(json[1]["pending"], true);
    }
}
