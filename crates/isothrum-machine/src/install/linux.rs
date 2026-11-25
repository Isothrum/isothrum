//! Installs and removals on Linux: one root script each ([`RootScript`]), run as root as the
//! [`Elevation`] says, through apt and the vendors' apt repositories or dnf and the vendors' rpm
//! repositories ([`scripts`]).

use crate::host::{Arch, HostOs, LinuxPackages, Packages};
use crate::install::root::RootScript;
use crate::install::steps::Step;
use crate::install::{Dependency, Elevation, Item, Refusal};

pub mod scripts;

use scripts::{
    awscli_script, azurecli_script, docker_dnf_script, docker_script, gcloud_script, libvirt_dnf_script, libvirt_script, qemu_dnf_script, qemu_script,
    terraform_dnf_script, terraform_script, vagrant_dnf_script, vagrant_script, virtualbox_dnf_script, virtualbox_script, vmware_utility_script,
};

/// The proxy variables a root step is handed: apt and curl need them behind a corporate proxy, and
/// pkexec clears the environment.
pub const PROXY_VARS: [&str; 6] = ["HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "http_proxy", "https_proxy", "no_proxy"];

/// The proxy variables `var` reads that are set, as `NAME=value` (what a root step is handed).
pub fn proxy_assignments(var: impl Fn(&str) -> Option<String>) -> Vec<String> {
    PROXY_VARS
        .into_iter()
        .filter_map(|name| var(name).filter(|v| !v.is_empty()).map(|v| format!("{name}={v}")))
        .collect()
}

/// How root steps run here: elevated as `elevation` says, with the proxy variables `proxy`
/// (`NAME=value`, [`proxy_assignments`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    /// How a step is elevated.
    pub elevation: Elevation,
    /// The proxy variables handed to it.
    pub proxy: Vec<String>,
}

impl Root {
    /// Root steps run `elevation`'s way, handed this process's proxy variables.
    pub(crate) fn with_this_proxy(elevation: Elevation) -> Root {
        Root {
            elevation,
            proxy: proxy_assignments(|name| std::env::var(name).ok()),
        }
    }

    /// `script` run as root. The proxy variables go in an owner-only file in a private folder
    /// kept as long as the step, which the script reads right after its prelude: on pkexec's
    /// command line, which every local user can read (`ps`, `/proc/<pid>/cmdline`) while the
    /// prompt is up, a proxy's password would show.
    ///
    /// # Errors
    ///
    /// [`Refusal::PrivateFile`] when that file can't be written.
    pub fn step(&self, script: RootScript) -> Result<Step, Refusal> {
        let root = |text: String| {
            let (program, args) = self.elevation.as_root("env", &["sh".into(), "-c".into(), text]);
            Step {
                program,
                args,
                note: None,
                interactions: Vec::new(),
                private: None,
                not_served: None,
            }
        };
        // A value over two lines can't be one line of the file; no proxy has one.
        let proxy: Vec<&str> = self.proxy.iter().map(String::as_str).filter(|a| !a.contains(['\n', '\r'])).collect();
        if proxy.is_empty() {
            return Ok(root(script.into_string()));
        }
        let (dir, file) = private_file(&proxy).map_err(|e| Refusal::PrivateFile(e.to_string()))?;
        let mut step = root(script.with_after_prelude(&read_proxy(&file)));
        step.private = Some(dir);
        Ok(step)
    }
}

/// An owner-only folder (mode 0700 from its creation, under a name no one can take first) holding
/// the owner-only file `proxy` (0600), one variable a line.
fn private_file(proxy: &[&str]) -> std::io::Result<(tempfile::TempDir, std::path::PathBuf)> {
    let mut private = tempfile::Builder::new();
    private.prefix("isothrum-root-");
    #[cfg(unix)]
    private.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700));
    let dir = private.tempdir()?;
    let file = dir.path().join("proxy");
    let mut lines = proxy.join("\n");
    lines.push('\n');
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    // Owner-only from its creation.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(&file)?, lines.as_bytes())?;
    Ok((dir, file))
}

/// The shell that exports the proxy variables `file` lists (`NAME=value`, one a line), read as
/// data: a line naming anything but a proxy variable is skipped, and nothing in a value is run.
fn read_proxy(file: &std::path::Path) -> String {
    let names = PROXY_VARS.map(|name| format!("{name}=*")).join("|");
    let quoted = format!("'{}'", file.to_string_lossy().replace('\'', r"'\''"));
    format!(r#"while IFS= read -r v; do case "$v" in {names}) export "$v" ;; esac; done < {quoted}"#)
}

/// The Linux distribution's package manager `packages` names, or why nothing installs without one.
fn linux(dependency: Dependency, packages: Option<Packages>) -> Result<LinuxPackages, Refusal> {
    packages.and_then(Packages::linux).ok_or(Refusal::WithoutPackages { dependency })
}

/// The install of `dep` on Linux on a CPU `arch`, as root, for the user `user` (joined to the
/// groups a tool's devices need): apt and the vendors' apt repositories, or dnf and the vendors'
/// rpm repositories. A tool its vendor ships no build of for `arch` is refused
/// ([`Dependency::no_build`]), before the package manager is looked at.
///
/// # Errors
///
/// [`Refusal`]: why it can't be installed here.
pub fn install(dep: Dependency, packages: Option<Packages>, arch: Arch, user: Option<&str>, root: &Root) -> Result<Vec<Step>, Refusal> {
    dep.runs_here(HostOs::Linux)?;
    if let Some(why) = dep.no_build(HostOs::Linux, arch) {
        return Err(Refusal::NoBuild {
            dependency: dep,
            os: HostOs::Linux,
            arch,
            why,
        });
    }
    let packages = linux(dep, packages)?;
    let script = match (dep, packages) {
        (Dependency::Wsl | Dependency::Hyperv | Dependency::Utm, _) => {
            return Err(Refusal::NotHere {
                dependency: dep,
                os: HostOs::Linux,
            });
        }
        // From HashiCorp's apt repository, not the distribution's (older than projects need).
        (Dependency::Vagrant, LinuxPackages::Apt) => vagrant_script(),
        (Dependency::Vagrant, LinuxPackages::Dnf) => vagrant_dnf_script(),
        (Dependency::Docker, LinuxPackages::Apt) => docker_script(user),
        (Dependency::Docker, LinuxPackages::Dnf) => docker_dnf_script(user),
        (Dependency::Terraform, LinuxPackages::Apt) => terraform_script(),
        (Dependency::Terraform, LinuxPackages::Dnf) => terraform_dnf_script(),
        // Fedora: RPM Fusion's VirtualBox, the distribution's libvirt and QEMU.
        (Dependency::Virtualbox, LinuxPackages::Apt) => virtualbox_script(),
        (Dependency::Virtualbox, LinuxPackages::Dnf) => virtualbox_dnf_script(user),
        // The user joins /dev/kvm's group: VMs boot with KVM.
        (Dependency::Qemu, LinuxPackages::Apt) => qemu_script(user),
        (Dependency::Qemu, LinuxPackages::Dnf) => qemu_dnf_script(user),
        (Dependency::Libvirt, LinuxPackages::Apt) => libvirt_script(user),
        (Dependency::Libvirt, LinuxPackages::Dnf) => libvirt_dnf_script(user),
        // AWS CLI v2 from Amazon's signed bundle, in a private temporary directory: no package
        // manager's.
        (Dependency::Awscli, LinuxPackages::Apt | LinuxPackages::Dnf) => awscli_script(),
        // Azure CLI and gcloud from Microsoft's and Google's apt repositories, their keys pinned;
        // through apt only for now, as VMware's utility.
        (Dependency::Azurecli, LinuxPackages::Apt) => azurecli_script(),
        (Dependency::Gcloud, LinuxPackages::Apt) => gcloud_script(),
        (Dependency::VmwareUtility, LinuxPackages::Apt) => vmware_utility_script(),
        (Dependency::Azurecli | Dependency::Gcloud | Dependency::VmwareUtility, LinuxPackages::Dnf) => return Err(Refusal::AptOnly { dependency: dep }),
    };
    let step = root.step(script)?;
    // Docker's install stops, changing nothing, outside the distributions it serves.
    Ok(vec![if dep == Dependency::Docker {
        step.not_served(super::steps::NotServed::DockerDistribution)
    } else {
        step
    }])
}

/// The removal on Linux, as root, of what `added` lists of `dep` ([`Item`], as an install's
/// record has it), and of nothing else: those of its packages ([`super::added::packages`]: a name
/// on record that isn't one of them is never removed) through the package manager that installed
/// them (a tool installed through dnf removed with `apt-get remove` failed with `apt-get: not
/// found`); the AWS CLI's folder when the install added it. The package manager would take away
/// with them every package that needs one of them, whoever installed it: the removal stops,
/// removing nothing, when it would (`removal_script`).
///
/// # Errors
///
/// [`Refusal::NothingAdded`] when `added` lists nothing of `dep`'s, or why it can't be removed
/// here.
pub fn uninstall(dep: Dependency, packages: Option<Packages>, added: &[Item], root: &Root) -> Result<Vec<Step>, Refusal> {
    dep.runs_here(HostOs::Linux)?;
    // AWS CLI is a folder on every distribution.
    if dep == Dependency::Awscli {
        return if added.contains(&Item::Tool(dep)) {
            Ok(vec![root.step(RootScript::new(AWS_CLI_REMOVAL))?])
        } else {
            Err(Refusal::NothingAdded { dependency: dep })
        };
    }
    let manager = linux(dep, packages)?;
    let own = super::added::packages(dep, manager);
    let names: Vec<&str> = added
        .iter()
        .filter_map(|item| match item {
            Item::Package(name) if package_name(name) && own.iter().any(|p| super::added::matches(p, name)) => Some(name.as_str()),
            _ => None,
        })
        .collect();
    if names.is_empty() {
        return Err(Refusal::NothingAdded { dependency: dep });
    }
    Ok(vec![root.step(removal_script(manager, &names))?])
}

/// A package's name a script may carry unquoted: letters, digits and `+-._`, starting with a
/// letter or a digit (never an option).
fn package_name(name: &str) -> bool {
    name.len() <= 128
        && name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.' | '_'))
}

/// The root script that removes the packages `names` (each a [`package_name`]) through
/// `manager`, after checking it would remove nothing else: apt's simulated removal names every
/// package it would take away (`Remv`), rpm's test refuses a package another needs. Either way,
/// another package that needs one of them stops the removal before anything is removed.
fn removal_script(manager: LinuxPackages, names: &[&str]) -> RootScript {
    let list = names.join(" ");
    match manager {
        LinuxPackages::Apt => {
            let ours: String = names.iter().flat_map(|n| [" -e ", n]).collect();
            RootScript::new(format!(
                r#"extra="$(apt-get -s remove {list} | awk '/^Remv /{{print $2}}' | sed 's/:.*//' | grep -vxF{ours} || true)"; if [ -n "$extra" ]; then echo "Removing $(echo {list}) would also remove packages that need them: $(echo $extra). Nothing was removed." >&2; exit 1; fi; apt-get remove -y {list}"#
            ))
        }
        LinuxPackages::Dnf => RootScript::new(format!(
            r#"if ! rpm -e --test {list}; then echo "Other packages need $(echo {list}). Nothing was removed." >&2; exit 1; fi; dnf remove -y {list}"#
        )),
    }
}

/// The AWS CLI's removal: its folder and its two links.
const AWS_CLI_REMOVAL: &str = "rm -rf /usr/local/aws-cli /usr/local/bin/aws /usr/local/bin/aws_completer";

#[cfg(test)]
mod tests;
