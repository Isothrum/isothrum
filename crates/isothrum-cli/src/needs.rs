//! What a run needs on this computer, and what of it is missing (isothrum-machine's
//! [`requirements`]): `check` says it for a project, and a failed `up` says it when its failure
//! was a missing requirement (a tool not on `PATH`, an engine that doesn't answer). Asking
//! installs nothing.

use std::sync::Arc;

use isothrum_machine::Context;
use isothrum_machine::install::{self, Dependency, Elevation, Interaction, Prompt};
use isothrum_machine::requirements::{self, Asking, Need, Plan, Requirement, Uses};
use isothrum_runner::{Project, SystemProcesses};

use crate::args::{CheckArgs, Cli, Command, Lifecycle, RunnerCommand, UpArgs};
use crate::error::Result;
use crate::exit::Verdict;
use crate::term::{Term, Trail};

/// The longest command line a plan shows; a longer one (a root script) is cut, ending `…`.
const SHOWN: usize = 100;

/// `check`: the project's needs here, one a line (a JSON document with `json`). Fails when one is
/// missing.
pub(crate) async fn check(args: &CheckArgs, json: bool, term: &Term) -> Result<Verdict> {
    let project = Project::open(&args.path, args.format.map(Into::into))?;
    let mut uses = Uses::new(project.format);
    if let Some(provider) = args.provider {
        uses = uses.on(provider);
    }
    if let Some(target) = args.target {
        uses = uses.at(target);
    }
    let found = asked(&uses).await;
    if json {
        let needs: Vec<serde_json::Value> = found.iter().map(need_json).collect();
        term.out(&serde_json::to_string(&serde_json::json!({ "needs": needs }))?);
    } else {
        term.out(&format!("This {} run needs:", project.format));
        for need in &found {
            term.out(&need_line(need));
        }
    }
    Ok(Verdict::of(!found.iter().any(Need::missing)))
}

/// After an `up` that failed for a missing requirement, what the run needs here, on standard
/// error: what is missing, and what brings it.
pub(crate) async fn after_failed_up(cli: &Cli, trail: &Trail, term: &Term) {
    let Command::Runner(command) = &cli.command else { return };
    let RunnerCommand::Lifecycle(Lifecycle::Up(UpArgs { op, .. })) = &**command else {
        return;
    };
    let Some(detail) = trail.error_detail() else { return };
    if Requirement::of_detail(&detail).is_none() {
        return;
    }
    let Ok(project) = Project::open(&op.run.path, op.run.format.map(Into::into)) else {
        return;
    };
    let uses = match op.provider {
        Some(provider) => Uses::new(project.format).on(provider),
        None => Uses::new(project.format),
    };
    let missing: Vec<Need> = asked(&uses).await.into_iter().filter(|n| n.present != Some(true)).collect();
    if missing.is_empty() {
        return;
    }
    term.err("This computer is missing what the run needs:");
    for need in &missing {
        term.err(&need_line(need));
    }
}

/// `uses`' requirements asked of this computer.
async fn asked(uses: &Uses) -> Vec<Need> {
    let cx = Context::here(Arc::new(SystemProcesses::default())).await;
    let path = std::env::var_os("PATH");
    let asking = Asking {
        elevation: Elevation::Prompt,
        path: path.as_deref(),
        ..Asking::default()
    };
    requirements::needs(&cx, &uses.requirements(), &asking).await
}

/// A need as a line: whether it is met, what it is, and what brings it when it isn't.
fn need_line(need: &Need) -> String {
    let state = match need.present {
        Some(true) => "ok",
        Some(false) => "missing",
        None => "unknown",
    };
    let name = name(need.requirement);
    match &need.plan {
        Some(plan) => format!("  {state:<8} {name}: {}", plan_text(plan)),
        None => format!("  {state:<8} {name}"),
    }
}

/// A requirement as a person reads it.
pub(crate) fn name(requirement: Requirement) -> String {
    match requirement {
        Requirement::Tool { dependency } => tool_name(dependency).to_owned(),
        Requirement::ComposePlugin => "Docker Compose".to_owned(),
        Requirement::EngineRunning => "the container engine, running".to_owned(),
        Requirement::Hypervisor { provider } => format!("the {} hypervisor", provider.id()),
        Requirement::VagrantPlugin { provider } => format!("the Vagrant plugin {}", provider.plugin().unwrap_or(provider.id())),
        Requirement::Ovftool => "VMware's OVF Tool".to_owned(),
        _ => format!("{requirement:?}"),
    }
}

/// A tool as a person reads it.
fn tool_name(dependency: Dependency) -> &'static str {
    match dependency {
        Dependency::Docker => "Docker",
        Dependency::Vagrant => "Vagrant",
        Dependency::Terraform => "Terraform",
        Dependency::Virtualbox => "VirtualBox",
        Dependency::Qemu => "QEMU",
        Dependency::Utm => "UTM",
        Dependency::Libvirt => "libvirt",
        Dependency::Awscli => "the AWS CLI",
        Dependency::Azurecli => "the Azure CLI",
        Dependency::Gcloud => "the Google Cloud CLI",
        Dependency::Wsl => "WSL 2",
        Dependency::VmwareUtility => "the Vagrant VMware Utility",
        Dependency::Hyperv => "Hyper-V",
    }
}

/// What brings a missing requirement, in a few words.
pub(crate) fn plan_text(plan: &Plan) -> String {
    match plan {
        Plan::Install(steps) => {
            let lines: Vec<String> = steps.iter().map(|s| shortened(&install::command_line(s))).collect();
            let asks: Vec<String> = steps.iter().flat_map(|s| s.interactions.iter()).map(interaction_text).collect();
            let asks = if asks.is_empty() { String::new() } else { format!(" ({})", asks.join("; ")) };
            format!("install with {}{asks}", lines.join(", then "))
        }
        Plan::Start => "start it".to_owned(),
        Plan::ByHand { page: Some(page) } => format!("download it from {} ({}), install it, then check again", page.name, page.url),
        Plan::ByHand { page: None } => "install it yourself".to_owned(),
        Plan::Refused(why) => format!("no install here: {why}"),
        Plan::After { requirement } => format!("install it once {} is there", name(*requirement)),
        Plan::ShareVirtualization => "let KVM share the CPU's virtualization with it".to_owned(),
        _ => "no plan known".to_owned(),
    }
}

/// What an install asks of the person at the computer, in a few words.
fn interaction_text(interaction: &Interaction) -> String {
    match interaction {
        Interaction::AdminPassword { via: Prompt::Mac } => "macOS asks for your password".to_owned(),
        Interaction::AdminPassword { via: Prompt::Uac } => "Windows asks for an administrator's approval".to_owned(),
        Interaction::AdminPassword { via: Prompt::Pkexec } => "asks for your password".to_owned(),
        Interaction::Licence { name, url } => format!("accepts the {name} for you: {url}"),
        Interaction::VendorWindow { app } => format!("{app} opens a window to finish in"),
        Interaction::AllowSystemExtension { vendor } => format!("allow {vendor}'s system extension in System Settings"),
        Interaction::Restart => "a restart follows".to_owned(),
        Interaction::Relogin => "sign out and back in after".to_owned(),
        Interaction::LongBuild { minutes } => format!("builds from source: about {minutes} minutes"),
        Interaction::DownloadByHand { vendor, url } => format!("download it from {vendor}: {url}"),
        _ => format!("{interaction:?}"),
    }
}

/// `line` without its `$ ` prompt, on one line (a script's lines and indents as single spaces),
/// cut to [`SHOWN`] characters.
fn shortened(line: &str) -> String {
    let line = line.strip_prefix("$ ").unwrap_or(line);
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= SHOWN {
        format!("`{line}`")
    } else {
        format!("`{}…`", line.chars().take(SHOWN).collect::<String>())
    }
}

/// A need as `check --json` gives it.
fn need_json(need: &Need) -> serde_json::Value {
    let plan = need.plan.as_ref().map(|plan| match plan {
        Plan::Install(steps) => serde_json::json!({
            "kind": "install",
            "steps": steps.iter().map(|s| install::command_line(s).trim_start_matches("$ ").to_owned()).collect::<Vec<_>>(),
            "interactions": steps.iter().flat_map(|s| s.interactions.iter()).collect::<Vec<_>>(),
        }),
        Plan::Start => serde_json::json!({ "kind": "start" }),
        Plan::ByHand { page } => serde_json::json!({
            "kind": "byHand",
            "url": page.map(|p| p.url),
            "vendor": page.map(|p| p.name),
            "interactions": page.map(|p| vec![p.download()]).unwrap_or_default(),
        }),
        Plan::Refused(why) => serde_json::json!({ "kind": "refused", "reason": why.to_string() }),
        Plan::After { requirement } => serde_json::json!({ "kind": "after", "requirement": requirement, "name": name(*requirement) }),
        Plan::ShareVirtualization => serde_json::json!({ "kind": "shareVirtualization" }),
        _ => serde_json::json!({ "kind": "unknown" }),
    });
    serde_json::json!({
        "requirement": need.requirement,
        "name": name(need.requirement),
        "present": need.present,
        "plan": plan,
    })
}

#[cfg(test)]
mod tests {
    use isothrum_machine::install::Refusal;
    use isothrum_machine::providers::Provider;

    use super::*;

    #[test]
    fn a_plan_is_said_in_a_few_words_a_long_script_cut() {
        assert_eq!(plan_text(&Plan::Start), "start it");
        assert_eq!(
            plan_text(&Plan::ByHand {
                page: Some(install::OVFTOOL_PAGE)
            }),
            format!("download it from Broadcom ({}), install it, then check again", requirements::OVFTOOL_URL)
        );
        let plugin = install::vagrant_plugin("vagrant-libvirt").unwrap();
        assert_eq!(plan_text(&Plan::Install(vec![plugin])), "install with `vagrant plugin install vagrant-libvirt`");
        let long = install::step("sh", &["-c", &format!("set -e\n  {}", "x".repeat(500))]);
        let said = plan_text(&Plan::Install(vec![long]));
        assert!(said.ends_with("…`") && said.chars().count() < 130, "{said}");
        assert!(!said.contains('\n') && !said.contains("  "), "one line: {said}");
        let mut docker = install::step("winget", &["install", "Docker.DockerDesktop"]);
        docker.interactions = install::interactions(Dependency::Docker, &windows(), Elevation::Prompt, None);
        assert_eq!(
            plan_text(&Plan::Install(vec![docker])),
            "install with `winget install Docker.DockerDesktop` (Windows asks for an administrator's approval; accepts the Docker Subscription Service Agreement for you: https://www.docker.com/legal/docker-subscription-service-agreement/)"
        );
        let refused = plan_text(&Plan::Refused(Refusal::NoHomebrew));
        assert_eq!(refused, "no install here: Homebrew isn't installed");
    }

    #[test]
    fn each_need_reads_as_its_state_its_name_and_its_plan() {
        let need = |requirement, present, plan| Need::new(requirement, present, plan);
        let ok = need(Requirement::ComposePlugin, Some(true), None);
        assert_eq!(need_line(&ok), "  ok       Docker Compose");
        let missing = need(Requirement::EngineRunning, Some(false), Some(Plan::Start));
        assert_eq!(need_line(&missing), "  missing  the container engine, running: start it");
        let plugin = Requirement::VagrantPlugin {
            provider: Provider::VmwareEsxi,
        };
        assert_eq!(name(plugin), "the Vagrant plugin vagrant-vmware-esxi");
        let json = need_json(&need(Requirement::Ovftool, Some(false), Some(Plan::ByHand { page: None })));
        assert_eq!(json["requirement"], serde_json::json!({"kind": "ovftool"}));
        assert_eq!(
            json["plan"],
            serde_json::json!({"kind": "byHand", "url": null, "vendor": null, "interactions": []})
        );
        let ovftool = need_json(&need(
            Requirement::Ovftool,
            Some(false),
            Some(Plan::ByHand {
                page: Some(install::OVFTOOL_PAGE),
            }),
        ));
        assert_eq!(
            ovftool["plan"]["interactions"],
            serde_json::json!([{"kind": "downloadByHand", "vendor": "Broadcom", "url": requirements::OVFTOOL_URL}])
        );
        assert_eq!(json["present"], false);
        let mut wsl = install::step("powershell", &[]);
        wsl.interactions = install::interactions(Dependency::Wsl, &windows(), Elevation::Prompt, None);
        let installs = need_json(&need(
            Requirement::Tool { dependency: Dependency::Wsl },
            Some(false),
            Some(Plan::Install(vec![wsl])),
        ));
        assert_eq!(
            installs["plan"]["interactions"],
            serde_json::json!([{"kind": "adminPassword", "via": "uac"}, {"kind": "restart"}])
        );
    }

    /// A Windows computer on x86-64.
    fn windows() -> isothrum_machine::Host {
        isothrum_machine::Host {
            os: isothrum_machine::HostOs::Windows,
            arch: isothrum_machine::Arch::Amd64,
            packages: None,
            devices: isothrum_machine::host::Devices::under(std::path::Path::new("/nonexistent")),
        }
    }
}
