//! A whole setup on one computer, in order: the options worked out for it ([`super::options`]),
//! for the kinds of project, engine and hypervisors wanted.

use serde::Deserialize;

use super::{Action, HypervisorOption, Machine, ManualReason, Options, engines_on, vagrant_steps};
use crate::docker::Engine;
use crate::host::HostOs;
use crate::install::Dependency;
use crate::providers::{Provider, Unavailable};

/// A kind of project a setup readies this computer for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SetupPath {
    /// Containers: WSL 2 on Windows, a Docker engine.
    Docker,
    /// VMs: a hypervisor, Vagrant and what it needs to drive it.
    Vm,
}

/// What a whole setup is for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Wanted {
    /// The kinds of project to ready the computer for.
    pub paths: Vec<SetupPath>,
    /// The engine to set up; none: the setup's own ([`Options::engine`]).
    pub engine: Option<Engine>,
    /// The hypervisors to set up, in order: the first a setup can set up is the one. None: the
    /// setup's own ([`Options::hypervisor`]).
    pub hypervisors: Vec<Provider>,
}

/// One action of a plan, with the item it is about (`wsl`, `docker`, `engine:<id>`, `vagrant`,
/// `<hypervisor>`, `plugin:<name>`, `vmware-utility`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// The item it is about.
    pub item: String,
    /// What it does.
    pub action: Action,
}

/// Why a hypervisor wanted isn't the one a setup sets up. A UI says each in its own words.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PassedOver {
    /// Its user gets it from its vendor: the page, when there is one.
    LinksOut {
        /// The vendor's page.
        url: Option<String>,
    },
    /// A setup doesn't install on this Linux distribution yet: its user installs it.
    Distribution,
    /// It can't run here.
    CantRun {
        /// Why.
        reason: Unavailable,
    },
    /// Not offered on this computer.
    NotOffered,
    /// Not needed: `chosen`, wanted before it, is set up.
    NotNeeded {
        /// The hypervisor set up.
        chosen: Provider,
    },
}

/// A whole setup on one computer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// What to do, in order: what ends a pass (a restart, a new login) last.
    pub steps: Vec<Step>,
    /// The hypervisor set up, when VMs are wanted and one can be.
    pub hypervisor: Option<Provider>,
    /// The wanted hypervisors not set up, each with why.
    pub passed_over: Vec<(Provider, PassedOver)>,
    /// Every item the setup covers, whether or not it has anything to do for it.
    pub items: Vec<String>,
}

/// Why a setup leaves a step alone: its user's to take, or it can't run here ([`left_alone`]).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LeftAlone {
    /// A setup doesn't install on this Linux distribution yet: its user installs `dependency`.
    Distribution {
        /// The tool, when it is one a setup installs elsewhere.
        dependency: Option<Dependency>,
    },
    /// Its user gets it from its vendor: the page, when there is one.
    LinksOut {
        /// The vendor's page.
        url: Option<String>,
    },
    /// It can't run here: a failure, not a skip.
    CantRun {
        /// Why.
        reason: Unavailable,
    },
}

impl LeftAlone {
    /// Whether it counts as the step failing (it can't run here), not as one skipped.
    #[must_use]
    pub fn fails(&self) -> bool {
        matches!(self, LeftAlone::CantRun { .. })
    }
}

/// Why a setup doesn't run `action` itself, when it doesn't: its user's to take, or it can't run
/// here. None for a step a setup runs (a restart and a new login it leaves to its caller).
#[must_use]
pub fn left_alone(action: &Action) -> Option<LeftAlone> {
    Some(match action {
        Action::Manual {
            reason: ManualReason::Distribution,
            dependency,
            ..
        } => LeftAlone::Distribution { dependency: *dependency },
        Action::Manual {
            reason: ManualReason::Vendor,
            url,
            ..
        } => LeftAlone::LinksOut { url: url.clone() },
        Action::Blocked { reason } => LeftAlone::CantRun { reason: reason.clone() },
        _ => return None,
    })
}

/// Why `option` can't be the hypervisor a setup sets up, if it can't: something its user gets,
/// or why it can't run here.
fn not_settable(option: &HypervisorOption) -> Option<PassedOver> {
    option.get.iter().find_map(|action| match action {
        Action::Manual {
            reason: ManualReason::Vendor,
            url,
            ..
        } => Some(PassedOver::LinksOut { url: url.clone() }),
        Action::Manual {
            reason: ManualReason::Distribution,
            ..
        } => Some(PassedOver::Distribution),
        // A restart sets up one blocked until it (VirtualBox's driver, built for a newer kernel).
        Action::Blocked { reason } if !action.ends_pass() => Some(PassedOver::CantRun { reason: reason.clone() }),
        _ => None,
    })
}

/// The hypervisor a setup sets up of `wanted`, and why each other one wanted isn't. None given:
/// the setup's own, else the first other one offered here that can be set up, in the check's
/// order (VirtualBox while KVM holds the CPU's virtualization, or with a driver that won't load,
/// gives way to libvirt or QEMU), those before it that can't passed over.
fn pick_hypervisor(o: &Options, wanted: &[Provider]) -> (Option<Provider>, Vec<(Provider, PassedOver)>) {
    let mut passed_over = Vec::new();
    if wanted.is_empty() {
        let others = o.hypervisors.iter().map(|h| h.provider).filter(|p| Some(*p) != o.hypervisor);
        for option in o
            .hypervisor
            .into_iter()
            .chain(others)
            .filter_map(|p| o.hypervisors.iter().find(|h| h.provider == p))
        {
            match not_settable(option) {
                Some(why) => passed_over.push((option.provider, why)),
                None => return (Some(option.provider), passed_over),
            }
        }
        return (None, passed_over);
    }
    let mut chosen: Option<Provider> = None;
    for &provider in wanted {
        let Some(option) = o.hypervisors.iter().find(|h| h.provider == provider) else {
            passed_over.push((provider, PassedOver::NotOffered));
            continue;
        };
        match (chosen, not_settable(option)) {
            (_, Some(why)) => passed_over.push((provider, why)),
            (Some(first), None) => passed_over.push((provider, PassedOver::NotNeeded { chosen: first })),
            (None, None) => chosen = Some(provider),
        }
    }
    (chosen, passed_over)
}

/// Whether `action` is Docker's own install left to its user (a distribution without apt or dnf).
fn docker_install(action: &Action) -> bool {
    matches!(
        action,
        Action::Manual {
            reason: ManualReason::Distribution,
            ..
        }
    )
}

/// The whole setup `wanted` on machine `m` with its `options` ([`super::options`]), in a setup's
/// order: WSL 2 and the engine, then the hypervisor and what Vagrant needs for it; a restart or a
/// new login last, so everything else is done first.
#[must_use]
pub fn plan(m: &Machine, options: &Options, wanted: &Wanted) -> Plan {
    let mut plan = Plan::default();
    if wanted.paths.contains(&SetupPath::Docker) {
        let wsl = &options.wsl;
        plan.steps.extend(wsl.iter().map(|a| Step {
            item: "wsl".into(),
            action: a.clone(),
        }));
        let engine = wanted.engine.filter(|e| engines_on(m.host.os).contains(e)).unwrap_or(options.engine);
        let engine_item = format!("engine:{}", engine.id());
        let actions = options
            .engines
            .iter()
            .find(|e| e.engine == engine)
            .map(|e| e.actions.clone())
            .unwrap_or_default();
        for action in actions {
            // Docker Desktop runs on WSL 2: it starts once WSL is ready, after the restart.
            if !wsl.is_empty() && matches!(action, Action::StartEngine { .. }) {
                continue;
            }
            let item = if matches!(action, Action::Install { .. } | Action::Relogin) || docker_install(&action) {
                "docker".into()
            } else {
                engine_item.clone()
            };
            plan.steps.push(Step { item, action });
        }
        if m.host.os == HostOs::Windows {
            plan.items.push("wsl".into());
        }
        plan.items.extend(["docker".to_owned(), engine_item]);
    }
    if wanted.paths.contains(&SetupPath::Vm) {
        let (chosen, passed_over) = pick_hypervisor(options, &wanted.hypervisors);
        plan.hypervisor = chosen;
        plan.passed_over = passed_over;
        if let Some(p) = chosen.and_then(|c| m.local().find(|p| p.provider == c)) {
            let option = options.hypervisors.iter().find(|h| h.provider == p.provider);
            plan.steps.extend(option.into_iter().flat_map(|o| {
                o.get.iter().map(|a| Step {
                    item: p.provider.id().into(),
                    action: a.clone(),
                })
            }));
            plan.steps.extend(vagrant_steps(m, p).into_iter().map(|(item, action)| Step { item, action }));
            plan.items.extend(["vagrant".to_owned(), p.provider.id().to_owned()]);
            plan.items.extend(p.plugin.iter().map(|name| format!("plugin:{name}")));
            if p.utility.is_some() {
                plan.items.push("vmware-utility".into());
            }
        }
    }
    // What ends a pass goes last, in its order.
    let (ends, rest): (Vec<Step>, Vec<Step>) = plan.steps.into_iter().partition(|s| s.action.ends_pass());
    plan.steps = rest.into_iter().chain(ends).collect();
    plan
}
