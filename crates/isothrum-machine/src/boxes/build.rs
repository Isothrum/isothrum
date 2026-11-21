//! A box made on this computer: a base box booted on a hypervisor, a script run in it as root,
//! the VM then saved as a box file ([`isothrum_runner::Running::package`]) and added to Vagrant
//! under a name of the caller's. A later deployment boots that box with its set-up done, in the
//! time a boot takes. The build runs through a runner of its own, its home and project in the
//! caller's folder, so on Hyper-V the start, the save and the removal share one elevation; its
//! VM is always taken down, and the box file removed once added.

use std::path::Path;
use std::sync::Arc;

use isothrum_runner::{DownloadToAdopt, Event, EventKind, ProcessSpec, Provider, RunName, RunOptions, Runner, RunnerConfig};

use super::{add_box, adopted, held};
use crate::Context;

/// The build's run, in its own runner.
const RUN: &str = "box-build";
/// The build's machine: Vagrant's default.
const MACHINE: &str = "default";
/// The saved VM, in the build's folder until it is added.
const BOX_FILE: &str = "built.box";
/// The script, beside the build's Vagrantfile.
const SCRIPT: &str = "build.sh";

/// What to build.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Build<'a> {
    /// The box's name in Vagrant once added (`vagrant box add --name`).
    pub name: &'a str,
    /// The public box it starts from, added first when Vagrant doesn't hold it.
    pub base: &'a str,
    /// What runs in it, as root, once: a shell script.
    pub script: &'a str,
    /// The hypervisor it is built for, and runs on.
    pub provider: Provider,
    /// The build VM's memory, in MiB.
    pub memory_mb: u32,
    /// The build VM's CPUs.
    pub cpus: u32,
}

impl<'a> Build<'a> {
    /// Box `name` built from `base` on `provider` by `script`, in a VM of 2 GiB and 2 CPUs.
    #[must_use]
    pub fn new(name: &'a str, base: &'a str, script: &'a str, provider: Provider) -> Build<'a> {
        Build {
            name,
            base,
            script,
            provider,
            memory_mb: 2048,
            cpus: 2,
        }
    }

    /// The same, in a VM of `memory_mb` MiB and `cpus` CPUs.
    #[must_use]
    pub fn sized(self, memory_mb: u32, cpus: u32) -> Build<'a> {
        Build { memory_mb, cpus, ..self }
    }
}

/// Builds `b` in `dir` (the build's own folder: its runner's home and its project), each line the
/// tools print said to `line`. The box as added, for a caller that keeps a record of what it added
/// (none when its version can't be read back).
///
/// # Errors
///
/// A hypervisor no box is built on here (a server's), the base box not added, the VM not started,
/// the script failing, the VM not saved or the box not added: what the tool or the runner said.
/// The build's VM is taken down whatever failed.
pub async fn build(cx: &Context, dir: &Path, b: &Build<'_>, line: &(dyn Fn(&str) + Send + Sync)) -> Result<Option<DownloadToAdopt>, String> {
    if b.provider == Provider::VmwareEsxi {
        return Err(format!("a box is built on this computer's hypervisor, not on {}", b.provider.id()));
    }
    let list = cx.read_ok("vagrant", &["box", "list"]).await.unwrap_or_default();
    if held(&list, b.base, b.provider, cx.host.arch).is_none() {
        add_box(cx, b.provider, b.base, &mut |l| line(l)).await?;
    }
    let project = dir.join("project");
    let written = std::fs::create_dir_all(&project)
        .and_then(|()| std::fs::write(project.join("Vagrantfile"), vagrantfile(b)))
        .and_then(|()| std::fs::write(project.join(SCRIPT), b.script));
    written.map_err(|e| format!("{} couldn't be written: {e}", project.display()))?;
    let mut config = RunnerConfig::new(dir.join("home"));
    config.processes = Arc::clone(&cx.runner);
    let runner = Runner::new(config).map_err(|e| e.to_string())?;
    let name = RunName::new(RUN).map_err(|e| e.to_string())?;
    let run = runner.open(&project, Some(name)).map_err(|e| e.to_string())?.sharing_elevation();
    let opts = RunOptions::new().with_provider(b.provider);
    let said = |e: &Event| {
        if let EventKind::Log { line: l, .. } = &e.kind {
            line(l);
        }
    };
    let file = dir.join(BOX_FILE);
    // A file an earlier build left would be added as this one.
    drop(std::fs::remove_file(&file));
    let saved = match run.up(&opts, &said).await {
        Ok(_) => run.package(MACHINE, &file, &opts, &said).await.map_err(|e| e.report().to_string()),
        Err(e) => Err(e.report().to_string()),
    };
    let down = run.down(&opts, &said).await;
    saved?;
    let added = add_file(cx, b, &file, line).await;
    drop(std::fs::remove_file(&file));
    if let Err(e) = down {
        line(&format!("the build's VM wasn't taken down: {}", e.report()));
    }
    added
}

/// Adds the box file `file` as box `b.name` (`vagrant box add --name … --force`).
async fn add_file(cx: &Context, b: &Build<'_>, file: &Path, line: &(dyn Fn(&str) + Send + Sync)) -> Result<Option<DownloadToAdopt>, String> {
    let file = file.display().to_string();
    let spec = ProcessSpec::new("vagrant", ["box", "add", "--name", b.name, file.as_str(), "--force"]);
    let exit = cx.runner.run(&spec, &mut |_, l: &str| line(l)).await.map_err(|e| e.to_string())?;
    if !exit.success() {
        return Err(format!("vagrant box add --name {} failed ({exit})", b.name));
    }
    let list = cx.read_ok("vagrant", &["box", "list"]).await.unwrap_or_default();
    Ok(held(&list, b.name, b.provider, cx.host.arch).map(|version| adopted(b.name, b.provider, version)))
}

/// The build's Vagrantfile: `b.base` on `b.provider`, sized as `b` says, its script run once as
/// root. On Hyper-V it joins Windows' Default Switch (Vagrant would otherwise ask which switch,
/// with no one to answer).
fn vagrantfile(b: &Build<'_>) -> String {
    let (memory, cpus) = (b.memory_mb, b.cpus);
    let mut v = vec![
        "Vagrant.configure(\"2\") do |config|".to_owned(),
        format!("  config.vm.box = {}", ruby_quote(b.base)),
        "  config.vm.boot_timeout = 1800".to_owned(),
        "  config.vm.synced_folder \".\", \"/vagrant\", disabled: true".to_owned(),
    ];
    if b.provider == Provider::Hyperv {
        let switch = crate::selftest::hypervisor::HYPERV_SWITCH;
        v.push(format!("  config.vm.network \"public_network\", bridge: \"{switch}\""));
    }
    let settings = match b.provider {
        Provider::Qemu => format!("    h.memory = \"{memory}M\"\n    h.smp = \"{cpus}\"\n"),
        Provider::VmwareDesktop => format!("    h.vmx[\"memsize\"] = \"{memory}\"\n    h.vmx[\"numvcpus\"] = \"{cpus}\"\n"),
        _ => format!("    h.memory = {memory}\n    h.cpus = {cpus}\n"),
    };
    v.push(format!("  config.vm.provider \"{}\" do |h|\n{settings}  end", b.provider.id()));
    v.push(format!("  config.vm.provision \"shell\", path: \"{SCRIPT}\""));
    v.push("end\n".to_owned());
    v.join("\n")
}

/// `s` as a Ruby single-quoted string.
fn ruby_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_boots_the_base_box_sized_and_runs_the_script_once() {
        let b = Build::new("local/tools", "base/os", "echo hi", Provider::Virtualbox).sized(4096, 4);
        let file = vagrantfile(&b);
        assert!(file.contains("config.vm.box = 'base/os'"), "{file}");
        assert!(
            file.contains("config.vm.provider \"virtualbox\" do |h|\n    h.memory = 4096\n    h.cpus = 4\n  end"),
            "{file}"
        );
        assert!(file.contains("config.vm.provision \"shell\", path: \"build.sh\""), "{file}");
        assert!(!file.contains("public_network"), "{file}");
    }

    #[test]
    fn each_hypervisor_gets_its_own_sizes_and_hyper_v_its_switch() {
        let qemu = vagrantfile(&Build::new("n", "b", "", Provider::Qemu));
        assert!(qemu.contains("h.memory = \"2048M\"\n    h.smp = \"2\""), "{qemu}");
        let vmware = vagrantfile(&Build::new("n", "b", "", Provider::VmwareDesktop));
        assert!(vmware.contains("h.vmx[\"memsize\"] = \"2048\""), "{vmware}");
        let hyperv = vagrantfile(&Build::new("n", "b", "", Provider::Hyperv));
        assert!(hyperv.contains("config.vm.network \"public_network\", bridge: \"Default Switch\""), "{hyperv}");
        assert!(
            hyperv.contains("config.vm.provider \"hyperv\" do |h|\n    h.memory = 2048\n    h.cpus = 2"),
            "{hyperv}"
        );
    }

    #[test]
    fn a_base_box_name_is_quoted_for_ruby() {
        assert_eq!(ruby_quote("it's"), "'it\\'s'");
    }
}
