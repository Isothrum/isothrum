//! What setups added, on record, so a clean-up removes what they added and only that: each
//! install's [`Installed`] ([`super::added`]), never the items that were there before it. A tool
//! whose install only opened its vendor's installer (a macOS cask), or which isn't found yet,
//! counts once a later check finds it ([`settle`]). The rules are here; the caller keeps the
//! records where it keeps its data, and reads and writes them in one transaction around
//! [`remember`] and [`settle`].

use serde::{Deserialize, Serialize};

use super::Dependency;
use super::added::{Installed, Item};

/// What setups added of a tool, on record.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InstalledTool {
    /// The tool.
    pub dependency: Dependency,
    /// When an install last added to it, in seconds since the Unix epoch.
    pub at: u64,
    /// What its installs added: what a clean-up removes ([`super::removal`]), and nothing else.
    pub added: Vec<Item>,
    /// What an install left to its vendor's installer, or didn't find yet ([`Installed::waiting`]):
    /// added once a later check finds it ([`settle`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting: Vec<Item>,
}

/// How long an opened installer may take to be finished before what waits for it is dropped: a
/// tool that shows up later than that was most likely installed some other way.
pub const PENDING_FOR_SECS: u64 = 24 * 3600;

/// `tools` with what an install of `dependency` added (`installed`) recorded at `now`, beside what
/// earlier installs added: one record a tool. An install that added nothing, nor left anything
/// waiting, records nothing.
pub fn remember(tools: &mut Vec<InstalledTool>, dependency: Dependency, installed: &Installed, now: u64) {
    if installed.is_empty() {
        return;
    }
    let mut added = tools.iter().find(|t| t.dependency == dependency).map(|t| t.added.clone()).unwrap_or_default();
    for item in &installed.added {
        if !added.contains(item) {
            added.push(item.clone());
        }
    }
    let waiting = installed.waiting.iter().filter(|item| !added.contains(item)).cloned().collect();
    tools.retain(|t| t.dependency != dependency);
    tools.push(InstalledTool {
        dependency,
        at: now,
        added,
        waiting,
    });
}

/// Adds to each record the items it waited for that are now there (`there`, by tool), and drops
/// what has waited longer than [`PENDING_FOR_SECS`] at `now`; a record left with nothing goes.
pub fn settle(tools: &mut Vec<InstalledTool>, there: &[(Dependency, Vec<Item>)], now: u64) {
    for t in tools.iter_mut() {
        let found = there.iter().filter(|(d, _)| *d == t.dependency).flat_map(|(_, items)| items);
        for item in found {
            if t.waiting.contains(item) {
                t.waiting.retain(|w| w != item);
                t.added.push(item.clone());
            }
        }
        if now.saturating_sub(t.at) >= PENDING_FOR_SECS {
            t.waiting.clear();
        }
    }
    tools.retain(|t| !t.added.is_empty() || !t.waiting.is_empty());
}

/// The records of tools something was added to: what a clean-up may offer to remove.
#[must_use]
pub fn removable(tools: Vec<InstalledTool>) -> Vec<InstalledTool> {
    tools.into_iter().filter(|t| !t.added.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(name: &str) -> Item {
        Item::Package(name.to_owned())
    }

    #[test]
    fn an_install_that_added_nothing_records_nothing() {
        let mut tools = Vec::new();
        remember(&mut tools, Dependency::Qemu, &Installed::default(), 10);
        assert_eq!(tools, []);
    }

    #[test]
    fn a_tool_is_recorded_once_with_what_each_install_added() {
        let mut tools = Vec::new();
        remember(&mut tools, Dependency::Qemu, &Installed::new(vec![package("genisoimage")], vec![]), 10);
        remember(
            &mut tools,
            Dependency::Qemu,
            &Installed::new(vec![package("virtiofsd"), package("genisoimage")], vec![]),
            20,
        );
        assert_eq!(
            tools,
            [InstalledTool {
                dependency: Dependency::Qemu,
                at: 20,
                added: vec![package("genisoimage"), package("virtiofsd")],
                waiting: vec![],
            }]
        );
        let json = serde_json::to_value(&tools[0]).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "dependency": "qemu", "at": 20, "added": [{ "package": "genisoimage" }, { "package": "virtiofsd" }] }),
            "nothing waiting: no key"
        );
    }

    #[test]
    fn what_waits_is_never_removable_until_found_and_is_dropped_once_stale() {
        let utm = Item::Tool(Dependency::Utm);
        let docker = Item::Tool(Dependency::Docker);
        let mut tools = Vec::new();
        remember(&mut tools, Dependency::Utm, &Installed::new(vec![], vec![utm.clone()]), 0);
        remember(&mut tools, Dependency::Docker, &Installed::new(vec![], vec![docker.clone()]), PENDING_FOR_SECS);
        assert_eq!(removable(tools.clone()), [], "waiting isn't removable");
        settle(&mut tools, &[(Dependency::Docker, vec![docker.clone()])], PENDING_FOR_SECS);
        assert_eq!(tools.len(), 1, "UTM's waited too long");
        assert_eq!(removable(tools.clone())[0].added, [docker]);
    }
}
