use std::path::Path;
use std::sync::Arc;

use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::testing::linux_host;

/// `text` written to `path`, its folders made.
fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A Linux host under `root` whose KVM is a module (`kvm_intel` over `kvm`) holding the CPU's
/// virtualization, `vms` of its own VMs running.
fn held(root: &Path, vms: usize) -> Host {
    let host = linux_host(root);
    let d = &host.devices;
    write(&d.proc_modules, "kvm_intel 413696 0 - Live 0x0\nkvm 1404928 1 kvm_intel, Live 0x0\n");
    write(&d.kvm_virt_at_load, "Y\n");
    write(&d.sys_module.join("kvm/refcnt"), "1\n");
    write(&d.sys_module.join("kvm/holders/kvm_intel"), "");
    write(&d.sys_module.join("kvm_intel/refcnt"), &format!("{vms}\n"));
    std::fs::create_dir_all(d.sys_module.join("kvm_intel/holders")).unwrap();
    host
}

#[test]
fn kvm_is_in_use_when_more_hold_it_than_the_modules_built_on_it() {
    let dir = tempfile::tempdir().unwrap();
    let host = held(dir.path(), 0);
    assert!(kvm::loadable(&host));
    assert_eq!(kvm::cpu_module(&host), Some("kvm_intel"));
    assert!(!kvm::in_use(&host), "kvm held by kvm_intel alone, kvm_intel by nothing");
    write(&host.devices.sys_module.join("kvm_intel/refcnt"), "2\n");
    assert!(kvm::in_use(&host), "two VMs running");
    write(&host.devices.sys_module.join("kvm_intel/refcnt"), "0\n");
    write(&host.devices.sys_module.join("kvm/refcnt"), "2\n");
    assert!(kvm::in_use(&host), "/dev/kvm held open");
    std::fs::remove_file(host.devices.sys_module.join("kvm/refcnt")).unwrap();
    assert!(kvm::in_use(&host), "unreadable: in use, nothing unloaded on a guess");
    assert!(!kvm::loadable(&host), "no refcnt: built into the kernel");
}

#[test]
fn the_option_is_read_from_modprobes_conf_files() {
    let dir = tempfile::tempdir().unwrap();
    let host = linux_host(dir.path());
    assert!(!kvm::shares_from_next_load(&host), "no /etc/modprobe.d");
    let d = &host.devices.modprobe_d;
    write(&d.join("other.conf"), "# options kvm enable_virt_at_load=0\noptions kvm_intel nested=1\n");
    write(&d.join("kvm.conf.dpkg-old"), "options kvm enable_virt_at_load=0\n");
    assert!(!kvm::shares_from_next_load(&host), "a comment, another module, a file modprobe doesn't read");
    write(&d.join("local.conf"), "options kvm  ignore_msrs=1 enable_virt_at_load=N\n");
    assert!(kvm::shares_from_next_load(&host));
}

#[test]
fn the_script_writes_the_option_and_loads_kvm_again_only_when_asked() {
    let dir = Path::new("/etc/modprobe.d");
    let write_only = share_script(dir, None);
    assert!(
        write_only.contains("> '/etc/modprobe.d/isothrum-kvm.conf'") && write_only.contains(OPTION),
        "{write_only}"
    );
    assert!(!write_only.contains("modprobe -r"), "{write_only}");
    let reload = share_script(dir, Some("kvm_amd"));
    assert!(
        reload.contains("if modprobe -r kvm_amd kvm; then") && reload.ends_with("; modprobe kvm_amd"),
        "{reload}"
    );
    for script in [write_only, reload] {
        assert!(crate::testing::sh_parses(&script), "not valid sh: {script}");
    }
}

#[test]
fn off_linux_or_with_kvm_built_in_there_is_no_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = held(dir.path(), 0);
    std::fs::remove_file(host.devices.sys_module.join("kvm/refcnt")).unwrap();
    assert_eq!(share_step(&host, Elevation::Prompt).unwrap_err(), ShareFailed::BuiltIn);
    host.os = HostOs::Macos;
    assert_eq!(share_step(&host, Elevation::Prompt).unwrap_err(), ShareFailed::NotHere);
}

#[test]
fn the_step_runs_as_root_the_installs_way() {
    let dir = tempfile::tempdir().unwrap();
    let host = held(dir.path(), 0);
    let prompt = share_step(&host, Elevation::Prompt).unwrap();
    assert_eq!(
        (prompt.program.as_str(), &prompt.args[..3]),
        ("pkexec", &["env".to_owned(), "sh".into(), "-c".into()][..])
    );
    let sudo = share_step(&host, Elevation::Unattended { root: false }).unwrap();
    assert_eq!((sudo.program.as_str(), sudo.args[0].as_str()), ("sudo", "-n"));
    let root = share_step(&host, Elevation::Unattended { root: true }).unwrap();
    assert_eq!(root.program, "env");
}

#[tokio::test]
async fn nothing_runs_where_kvm_doesnt_hold_the_cpus_virtualization() {
    let dir = tempfile::tempdir().unwrap();
    let host = held(dir.path(), 0);
    write(&host.devices.kvm_virt_at_load, "N\n");
    let cx = Context::new(Arc::new(FakeProcesses::script().build()), host);
    assert_eq!(share_virtualization(&cx, Elevation::Prompt, &mut |_| {}).await, Ok(Shared::Now));
    let none = Context::new(Arc::new(FakeProcesses::script().build()), linux_host(dir.path().join("none").as_path()));
    assert_eq!(
        share_virtualization(&none, Elevation::Prompt, &mut |_| {}).await,
        Ok(Shared::Now),
        "no KVM loaded"
    );
}

#[tokio::test]
async fn a_refused_root_step_is_the_failure() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeProcesses::script()
        .expect("sudo -n env sh -c *", Reply::fail(1, "sudo: a password is required"))
        .build();
    let cx = Context::new(Arc::new(fake), held(dir.path(), 0));
    let failed = share_virtualization(&cx, Elevation::Unattended { root: false }, &mut |_| {}).await;
    assert_eq!(failed, Err(ShareFailed::Step(StepFailed::SudoRefused)));
}

/// The root step's script run for real by `sh` against the test's folders, `modprobe` stood in
/// for: an unload refused (`refuse`: `kvm_intel` in use), a load after an unload setting KVM's
/// parameter from the option file as modprobe would (a load of a module loaded already does
/// nothing).
#[cfg(unix)]
fn modprobe_model(host: &Host, refuse: bool) -> FakeProcesses {
    let param = host.devices.kvm_virt_at_load.display().to_string();
    let option = host.devices.modprobe_d.join(OPTION_FILE).display().to_string();
    let stub = format!(
        r#"unloaded=; modprobe() {{ if [ "$1" = -r ]; then {unload}; elif [ -n "$unloaded" ] && grep -q enable_virt_at_load=0 '{option}'; then echo N > '{param}'; fi; }}"#,
        unload = if refuse {
            "echo 'modprobe: FATAL: Module kvm_intel is in use.' >&2; return 1"
        } else {
            "unloaded=1"
        },
    );
    FakeProcesses::model(move |call| {
        assert_eq!(call.program, "env", "{}", call.line());
        let out = crate::testing::sh(&format!("{stub}; {}", call.args[2]), &[]);
        let said = String::from_utf8_lossy(&out.stdout).into_owned();
        match out.status.code {
            Some(0) => Reply::ok(&said),
            code => Reply::fail(code.unwrap_or(1), &String::from_utf8_lossy(&out.stderr)),
        }
    })
}

#[cfg(unix)]
#[tokio::test]
async fn kvm_in_use_by_nobody_is_loaded_again_and_shares_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let host = held(dir.path(), 0);
    let fake = Arc::new(modprobe_model(&host, false));
    let cx = Context::new(Arc::<FakeProcesses>::clone(&fake), host.clone());
    let mut said = Vec::new();
    let shared = share_virtualization(&cx, Elevation::Unattended { root: true }, &mut |l| said.push(l.to_owned())).await;
    assert_eq!(shared, Ok(Shared::Now), "{said:#?}");
    let written = std::fs::read_to_string(host.devices.modprobe_d.join(OPTION_FILE)).unwrap();
    assert!(written.ends_with(&format!("{OPTION}\n")) && written.starts_with("# "), "{written}");
    assert!(said.iter().any(|l| l.contains("shares the CPU's virtualization now")), "{said:#?}");
    assert!(!kvm::holds_virtualization(&host) && sharing(&host) == Some(Shared::Now));
}

#[cfg(unix)]
#[tokio::test]
async fn a_kvm_vm_running_leaves_kvm_loaded_and_the_option_for_the_next_restart() {
    let dir = tempfile::tempdir().unwrap();
    let host = held(dir.path(), 1);
    let fake = Arc::new(modprobe_model(&host, false));
    let cx = Context::new(Arc::<FakeProcesses>::clone(&fake), host.clone());
    let shared = share_virtualization(&cx, Elevation::Unattended { root: true }, &mut |_| {}).await;
    assert_eq!(shared, Ok(Shared::AfterRestart));
    assert!(!fake.calls()[0].args[2].contains("modprobe -r"), "nothing unloaded under a running VM");
    assert!(kvm::shares_from_next_load(&host));
}

#[cfg(unix)]
#[tokio::test]
async fn an_unload_modprobe_refuses_leaves_the_option_for_the_next_restart() {
    let dir = tempfile::tempdir().unwrap();
    let host = held(dir.path(), 0);
    let cx = Context::new(Arc::new(modprobe_model(&host, true)), host.clone());
    let mut said = Vec::new();
    let shared = share_virtualization(&cx, Elevation::Unattended { root: true }, &mut |l| said.push(l.to_owned())).await;
    assert_eq!(shared, Ok(Shared::AfterRestart), "{said:#?}");
    assert!(said.iter().any(|l| l.contains("from the next restart")), "{said:#?}");
}
