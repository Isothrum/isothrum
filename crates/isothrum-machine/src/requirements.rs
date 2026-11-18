//! What a run needs on this computer, and what of it is missing here, with the plan that brings
//! it.
//!
//! - [`Uses`]: what a run will use, in a few words: its project's format, the hypervisor a
//!   Vagrant project runs on, the platform a Terraform module makes its machines on. Whoever
//!   places a run can say it (an embedder from its own placement, `isothrum up` from the project
//!   and its flags) without opening the project.
//! - [`Requirement`]: one thing the computer must have for it, and [`Uses::requirements`], the
//!   one table of which a run needs.
//! - [`needs`]: each requirement asked of the machine, present or not, with the plan that brings
//!   a missing one ([`Plan`]). Asking installs nothing.
//! - [`Requirement::of_error`]: the requirement a run's failure says is missing (a tool not on
//!   `PATH`, an engine that doesn't answer), so the failure can offer its plan.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;

use isothrum_runner::{Cloud, Detail, Error, Format};
use serde::{Deserialize, Serialize};

use crate::context::Context;
use crate::install::{self, Dependency, Elevation, Refusal, Step};
use crate::providers::{self, Provider, ProviderRow, ProviderStatus, Unavailable};
use crate::tools::{self, probes};

#[cfg(test)]
mod tests;

/// Where VMware's OVF Tool is downloaded: Broadcom's site, behind a sign-in, so its user fetches
/// it.
pub const OVFTOOL_URL: &str = "https://developer.broadcom.com/tools/open-virtualization-format-ovf-tool/latest";

/// The platform a Terraform module makes its machines on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[non_exhaustive]
pub enum Target {
    /// A public cloud, through its Terraform provider (and its CLI, for the clouds that sign in
    /// with one).
    Cloud {
        /// The cloud.
        cloud: Cloud,
    },
    /// A Proxmox VE server, through its API: nothing but Terraform.
    Proxmox,
    /// An ESXi host, its VMs uploaded with VMware's OVF Tool.
    Esxi,
}

/// What a run will use: its project's format, the hypervisor a Vagrant project runs on, the
/// platform a Terraform module makes its machines on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Uses {
    /// The project's format.
    pub format: Format,
    /// The hypervisor (Vagrant's `--provider`), when the run names one.
    pub provider: Option<Provider>,
    /// The platform a Terraform module makes its machines on, when known.
    pub target: Option<Target>,
}

impl Uses {
    /// A run of a project of `format`, on nothing named yet.
    #[must_use]
    pub fn new(format: Format) -> Uses {
        Uses {
            format,
            provider: None,
            target: None,
        }
    }

    /// A Vagrant project on hypervisor `provider`.
    #[must_use]
    pub fn vagrant(provider: Provider) -> Uses {
        Uses::new(Format::Vagrant).on(provider)
    }

    /// A Terraform module making its machines on `target`.
    #[must_use]
    pub fn terraform(target: Target) -> Uses {
        Uses::new(Format::Terraform).at(target)
    }

    /// The same, on hypervisor `provider`.
    #[must_use]
    pub fn on(self, provider: Provider) -> Uses {
        Uses {
            provider: Some(provider),
            ..self
        }
    }

    /// The same, making its machines on `target`.
    #[must_use]
    pub fn at(self, target: Target) -> Uses {
        Uses { target: Some(target), ..self }
    }

    /// What this computer must have for the run, in the order they are put right (a tool before
    /// what runs on it):
    ///
    /// - Compose: the container engine, its Compose plugin, the engine answering;
    /// - Vagrant on hypervisor P: Vagrant, P itself (unless it runs on a server), the VMware
    ///   Utility where P drives VMware through it (before the plugin, which breaks Vagrant for
    ///   every provider without it), P's plugin, and VMware's OVF Tool for ESXi;
    /// - Terraform: Terraform, and its target's: a cloud's CLI where it signs in with one, the
    ///   OVF Tool for ESXi.
    ///
    /// A provider or target the format doesn't use is ignored.
    #[must_use]
    pub fn requirements(&self) -> Vec<Requirement> {
        let mut needed = Vec::new();
        match self.format {
            Format::Compose => {
                needed.extend([
                    Requirement::Tool {
                        dependency: Dependency::Docker,
                    },
                    Requirement::ComposePlugin,
                    Requirement::EngineRunning,
                ]);
            }
            Format::Vagrant => {
                needed.push(Requirement::Tool {
                    dependency: Dependency::Vagrant,
                });
                if let Some(provider) = self.provider {
                    if !provider.is_remote() {
                        needed.push(Requirement::Hypervisor { provider });
                    }
                    if providers::needs_vmware_utility(provider) {
                        needed.push(Requirement::Tool {
                            dependency: Dependency::VmwareUtility,
                        });
                    }
                    if provider.plugin().is_some() {
                        needed.push(Requirement::VagrantPlugin { provider });
                    }
                    if provider == Provider::VmwareEsxi {
                        needed.push(Requirement::Ovftool);
                    }
                }
            }
            Format::Terraform => {
                needed.push(Requirement::Tool {
                    dependency: Dependency::Terraform,
                });
                match self.target {
                    Some(Target::Cloud { cloud }) => {
                        needed.extend(cloud_cli(cloud).map(|dependency| Requirement::Tool { dependency }));
                    }
                    Some(Target::Esxi) => needed.push(Requirement::Ovftool),
                    Some(Target::Proxmox) | None => {}
                }
            }
            // A format added to the runner needs nothing known here yet.
            _ => {}
        }
        needed
    }
}

/// The CLI a cloud's Terraform provider signs in with, where it uses one: the token and key
/// clouds use none.
#[must_use]
pub fn cloud_cli(cloud: Cloud) -> Option<Dependency> {
    match cloud {
        Cloud::Aws => Some(Dependency::Awscli),
        Cloud::Azure => Some(Dependency::Azurecli),
        Cloud::Gcp => Some(Dependency::Gcloud),
        _ => None,
    }
}

/// One thing a run needs on this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum Requirement {
    /// A tool a setup installs ([`Dependency`]), not a hypervisor (that is
    /// [`Requirement::Hypervisor`]).
    Tool {
        /// The tool.
        dependency: Dependency,
    },
    /// Docker's Compose plugin (`docker compose`), which comes with the engine's install.
    ComposePlugin,
    /// The container engine answering: started, not installed.
    EngineRunning,
    /// A hypervisor, installed and usable here as its provider's detection says.
    Hypervisor {
        /// Its provider.
        provider: Provider,
    },
    /// The Vagrant plugin a provider runs through ([`Provider::plugin`]), at a version that
    /// works.
    VagrantPlugin {
        /// The provider.
        provider: Provider,
    },
    /// VMware's OVF Tool, which ESXi's VMs are uploaded with ([`crate::tools::ovftool`]).
    Ovftool,
}

impl Requirement {
    /// What a tool that isn't on `PATH` is, by its program's name as the runner says it
    /// (`docker`, `VBoxManage`); none for a program no requirement installs.
    #[must_use]
    pub fn for_program(program: &str) -> Option<Requirement> {
        let tool = |dependency| Some(Requirement::Tool { dependency });
        let program = program.strip_suffix(".exe").unwrap_or(program);
        match program {
            "docker" => tool(Dependency::Docker),
            "vagrant" => tool(Dependency::Vagrant),
            "terraform" => tool(Dependency::Terraform),
            "aws" => tool(Dependency::Awscli),
            "az" => tool(Dependency::Azurecli),
            "gcloud" => tool(Dependency::Gcloud),
            "ovftool" => Some(Requirement::Ovftool),
            "VBoxManage" => Some(Requirement::Hypervisor {
                provider: Provider::Virtualbox,
            }),
            "virsh" => Some(Requirement::Hypervisor { provider: Provider::Libvirt }),
            "qemu-img" => Some(Requirement::Hypervisor { provider: Provider::Qemu }),
            "vmrun" => Some(Requirement::Hypervisor {
                provider: Provider::VmwareDesktop,
            }),
            "prlctl" => Some(Requirement::Hypervisor { provider: Provider::Parallels }),
            _ => None,
        }
    }

    /// The requirement a run's failure says is missing: a tool not on `PATH`
    /// ([`Error::ToolMissing`]), or the container engine not answering
    /// ([`Error::ToolUnanswered`] of `docker`); none for any other failure.
    #[must_use]
    pub fn of_error(error: &Error) -> Option<Requirement> {
        match error {
            Error::ToolMissing { tool, .. } => Requirement::for_program(tool),
            Error::ToolUnanswered { tool, .. } if tool == "docker" => Some(Requirement::EngineRunning),
            _ => None,
        }
    }

    /// The same, from a failure's detail as an event or a report carries it.
    #[must_use]
    pub fn of_detail(detail: &Detail) -> Option<Requirement> {
        match detail {
            Detail::ToolMissing { tool } => Requirement::for_program(tool),
            Detail::ToolUnanswered { tool } if tool == "docker" => Some(Requirement::EngineRunning),
            _ => None,
        }
    }
}

/// How a missing requirement is brought.
#[derive(Debug)]
#[non_exhaustive]
pub enum Plan {
    /// Installed by these steps ([`install::run_steps`]).
    Install(Vec<Step>),
    /// Started, not installed: the container engine ([`crate::docker::start_engine`]).
    Start,
    /// Installed, but KVM holds the CPU's virtualization ([`Unavailable::KvmHoldsVirtualization`],
    /// VirtualBox's): KVM set to share it ([`crate::setup::share_virtualization`]).
    ShareVirtualization,
    /// Its user gets it from its vendor (behind a sign-in or a licence): from `url`, when known.
    ByHand {
        /// Where.
        url: Option<&'static str>,
    },
    /// No plan here, for this reason.
    Refused(Refusal),
    /// Brought once `requirement` is met, by its own plan then: a tool that serves a hypervisor
    /// its user gets first ([`Dependency::serves`]: VMware's utility, before VMware); VMware's
    /// plugin, after VMware and its utility (without the utility it breaks Vagrant for every
    /// provider).
    After {
        /// What comes first.
        requirement: Requirement,
    },
}

impl Plan {
    /// The plan of an install: its steps, or why there are none.
    fn of(planned: Result<Vec<Step>, Refusal>) -> Plan {
        planned.map_or_else(Plan::Refused, Plan::Install)
    }
}

/// One requirement as asked of this computer.
#[derive(Debug)]
#[non_exhaustive]
pub struct Need {
    /// The requirement.
    pub requirement: Requirement,
    /// Whether it is met here; `None` when that can't be told (a Windows feature).
    pub present: Option<bool>,
    /// What brings it, when it isn't known to be met; for a hypervisor installed that KVM keeps
    /// from running ([`Unavailable::KvmHoldsVirtualization`]), what lets it run.
    pub plan: Option<Plan>,
    /// For a hypervisor, why its provider can't run VMs here yet, as its detection says
    /// ([`ProviderStatus::reason`]), whatever else it lacks: a hypervisor installed can still be
    /// stopped, or this account lack the right to use it. None when it can, or for another
    /// requirement.
    pub unavailable: Option<Unavailable>,
}

impl Need {
    /// `requirement`, `present` or not, brought by `plan`: what [`needs`] finds, made by hand
    /// (an embedder's test).
    #[must_use]
    pub fn new(requirement: Requirement, present: Option<bool>, plan: Option<Plan>) -> Need {
        Need {
            requirement,
            present,
            plan,
            unavailable: None,
        }
    }

    /// Whether it is known to be missing.
    #[must_use]
    pub fn missing(&self) -> bool {
        self.present == Some(false)
    }
}

/// How [`needs`] asks and plans: who an install is for, how its root steps are elevated, and
/// where the OVF Tool is looked for.
#[derive(Debug, Clone, Copy)]
pub struct Asking<'a> {
    /// The account an install is for (Linux joins it to the groups a tool's devices need).
    pub user: Option<&'a str>,
    /// How a root step is elevated (Linux).
    pub elevation: Elevation,
    /// The caller's own folder of the OVF Tool ([`crate::tools::ovftool::place`]), if it keeps
    /// one.
    pub ovftool_dir: Option<&'a Path>,
    /// The `PATH` the OVF Tool is looked for on.
    pub path: Option<&'a OsStr>,
}

impl Default for Asking<'_> {
    fn default() -> Self {
        Asking {
            user: None,
            elevation: Elevation::Prompt,
            ovftool_dir: None,
            path: None,
        }
    }
}

/// Each of `requirements` asked of the context's host: present or not, and, unless it is known
/// to be, the plan that brings it ([`install::plan`] for a tool, the hypervisor's own install or
/// its vendor's page, the plugin's `vagrant plugin install`, the engine's start); a hypervisor
/// installed that KVM keeps from running, KVM set to share ([`Plan::ShareVirtualization`]).
/// Asking only reads: nothing is installed, started or changed.
pub async fn needs(cx: &Context, requirements: &[Requirement], asking: &Asking<'_>) -> Vec<Need> {
    let mut statuses: HashMap<Provider, Option<ProviderStatus>> = HashMap::new();
    let mut found = Vec::new();
    for &requirement in requirements {
        let present = match requirement {
            Requirement::Tool { dependency } => install::present(cx, dependency).await,
            Requirement::ComposePlugin => Some(tools::version(cx, probes::COMPOSE).await.installed),
            Requirement::EngineRunning => Some(crate::docker::daemon_info(cx, "{{.ServerVersion}}", None).await.is_ok()),
            Requirement::Hypervisor { provider } => status(cx, &mut statuses, provider).await.and_then(|s| s.hypervisor),
            Requirement::VagrantPlugin { provider } => status(cx, &mut statuses, provider).await.map(|s| s.plugin_installed),
            Requirement::Ovftool => Some(crate::tools::ovftool::locate(cx, asking.ovftool_dir, asking.path).is_some()),
        };
        let first = comes_first(cx, &mut statuses, requirement).await;
        let unavailable = match requirement {
            Requirement::Hypervisor { provider } => status(cx, &mut statuses, provider).await.and_then(|s| s.reason.clone()),
            _ => None,
        };
        let plan = if present == Some(true) {
            (unavailable == Some(Unavailable::KvmHoldsVirtualization)).then_some(Plan::ShareVirtualization)
        } else {
            Some(match first {
                Some(hypervisor) => Plan::After { requirement: hypervisor },
                None => plan(cx, requirement, asking),
            })
        };
        found.push(Need {
            requirement,
            present,
            plan,
            unavailable,
        });
    }
    found
}

/// What `requirement` waits for, when it waits: a tool that serves a hypervisor not installed yet
/// waits for it (VMware's utility, for VMware); VMware's plugin waits for VMware, then for its
/// utility, so it is never installed without it.
async fn comes_first(cx: &Context, statuses: &mut HashMap<Provider, Option<ProviderStatus>>, requirement: Requirement) -> Option<Requirement> {
    let (provider, utility) = match requirement {
        Requirement::Tool { dependency } => (dependency.serves()?, false),
        Requirement::VagrantPlugin { provider } if providers::needs_vmware_utility(provider) => (provider, true),
        _ => return None,
    };
    let status = status(cx, statuses, provider).await;
    if status.and_then(|s| s.hypervisor) != Some(true) {
        return Some(Requirement::Hypervisor { provider });
    }
    (utility && status.and_then(|s| s.utility) != Some(crate::providers::UtilityState::Running)).then_some(Requirement::Tool {
        dependency: Dependency::VmwareUtility,
    })
}

/// `provider`'s status, asked once.
async fn status<'s>(cx: &Context, statuses: &'s mut HashMap<Provider, Option<ProviderStatus>>, provider: Provider) -> Option<&'s ProviderStatus> {
    if let std::collections::hash_map::Entry::Vacant(slot) = statuses.entry(provider) {
        slot.insert(providers::status(cx, provider).await);
    }
    statuses.get(&provider).and_then(Option::as_ref)
}

/// What brings `requirement` on the context's host.
#[must_use]
pub fn plan(cx: &Context, requirement: Requirement, asking: &Asking<'_>) -> Plan {
    let install = |dep| Plan::of(install::plan(dep, &cx.host, asking.user, asking.elevation));
    match requirement {
        Requirement::Tool { dependency } => install(dependency),
        // Docker's install brings its Compose plugin: Docker Desktop has it, Docker Engine's
        // install adds it.
        Requirement::ComposePlugin => install(Dependency::Docker),
        Requirement::EngineRunning => Plan::Start,
        Requirement::Hypervisor { provider } => match Dependency::of_hypervisor(provider) {
            Some(dep) => install(dep),
            None => Plan::ByHand {
                url: ProviderRow::of(provider).vendor,
            },
        },
        Requirement::VagrantPlugin { provider } => {
            let step = provider.plugin().and_then(install::vagrant_plugin);
            step.map_or(Plan::ByHand { url: None }, |s| Plan::Install(vec![s]))
        }
        Requirement::Ovftool => Plan::ByHand { url: Some(OVFTOOL_URL) },
    }
}
