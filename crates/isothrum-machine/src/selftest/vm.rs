//! The VM self-test: a real throwaway VM on a local hypervisor.
//!
//! Without an elevation hook, `vagrant up` and `vagrant ssh` run as two separate runner calls (on
//! Hyper-V each may prompt on its own). With one — an [`ElevatedBoot`] a caller supplies — both run
//! as one elevated batch (one prompt). The elevation itself is the `unsafe`
//! bit this crate drops: it is the caller's to implement, never computed here.

use std::path::Path;
use std::time::Duration;

use isothrum_runner::ProcessRunner;

use super::downloads::ensure_box;
use super::elevate::ElevatedBoot;
use super::{Reporter, arm};
use crate::error::{Error, Result};
use crate::exec;
use crate::providers::{self, Provider};

/// The test VM's address on its private lab network (VirtualBox's default host-only range): the
/// first of these that nothing answers on yet. One that answers already (another VM on it) would
/// make the ping check pass whatever the test VM does.
const VM_IPS: [&str; 4] = [
    "192.168.56.250",
    "192.168.56.249",
    "192.168.56.248",
    "192.168.56.247",
];
/// Booting (the box imported, the VM up, the command inside) and deleting the test VM: past these
/// the hypervisor or Vagrant is stuck, and the test says so instead of hanging.
const BOOT_LIMIT: Duration = Duration::from_secs(1200);
const DESTROY_LIMIT: Duration = Duration::from_secs(300);
/// How the test VM shows up in the hypervisor's own app while it runs.
const VM_NAME: &str = "CyberCTF test VM";
/// Pings (1 s apart) for the VM to answer on the lab network.
const PING_TRIES: u32 = 5;

/// A test box to make sure is present: which provider and CPU it is for.
pub(crate) struct TestBox {
    /// The hypervisor the box is for.
    pub provider: Provider,
    /// The box name.
    pub name: &'static str,
    /// Whether the host CPU is ARM.
    pub arm: bool,
}

/// What the boot step needs: the chosen provider, its box, and the private-network address when it
/// gets one.
struct Boot {
    provider: Provider,
    bx: &'static str,
    ip: Option<&'static str>,
}

/// The local hypervisors the VM self-test can use.
pub(crate) const TEST_PROVIDERS: [Provider; 7] = [
    Provider::Virtualbox,
    Provider::VmwareDesktop,
    Provider::Parallels,
    Provider::Utm,
    Provider::Libvirt,
    Provider::Qemu,
    Provider::Hyperv,
];

/// The local provider the VM test uses: the preferred one when it's ready, else the first one whose
/// hypervisor and plugin are ready.
pub(super) async fn test_provider(
    runner: &dyn ProcessRunner,
    preferred: Option<Provider>,
) -> Option<Provider> {
    let ready: Vec<Provider> = providers::detect(runner, true, None)
        .await
        .iter()
        .filter(|s| !s.remote && s.available && s.hypervisor != Some(false))
        .map(|s| s.provider)
        .collect();
    preferred
        .filter(|p| ready.contains(p))
        .or_else(|| ready.first().copied())
}

/// A read-only command that only succeeds when the hypervisor's service is up and usable by this
/// user (not just installed).
fn responds(p: Provider) -> Option<(&'static str, &'static [&'static str])> {
    match p {
        Provider::Virtualbox => Some(("VBoxManage", &["list", "hostinfo"])),
        Provider::VmwareDesktop => Some(("vmrun", &["list"])),
        Provider::Parallels => Some(("prlctl", &["list", "--all"])),
        Provider::Libvirt => Some(("virsh", &["-c", "qemu:///system", "list", "--all"])),
        Provider::Utm => Some(("/Applications/UTM.app/Contents/MacOS/utmctl", &["list"])),
        _ => None,
    }
}

/// Small public boxes that exist for this provider and CPU, smallest first.
pub(crate) fn candidate_boxes(p: Provider, arm: bool) -> &'static [&'static str] {
    match (p, arm) {
        (Provider::Virtualbox | Provider::VmwareDesktop | Provider::Parallels, false) => {
            &["generic/alpine319", "bento/debian-12"]
        }
        (Provider::Virtualbox | Provider::VmwareDesktop | Provider::Parallels, true) => {
            &["bento/debian-12"]
        }
        (Provider::Utm, _) => &["utm/bookworm"],
        _ => &["generic/alpine319"],
    }
}

/// The Vagrant box provider a box is added and listed under for `p`. QEMU is the exception: the
/// vagrant-qemu plugin boots a libvirt-format box's qcow2 directly, so its box is stored as
/// `libvirt`, never `qemu`. This mirrors the Isoloom engine, which runs its QEMU labs from libvirt
/// boxes too.
pub(crate) fn box_provider(p: Provider) -> &'static str {
    match p {
        Provider::Qemu => "libvirt",
        _ => p.id(),
    }
}

/// Providers whose Vagrant plugin can put the VM on a static private network.
fn has_private_network(p: Provider) -> bool {
    matches!(
        p,
        Provider::Virtualbox | Provider::VmwareDesktop | Provider::Parallels | Provider::Libvirt
    )
}

/// Picks a candidate box already downloaded for this provider, else the first candidate. Returns
/// (box, already downloaded).
pub(super) async fn pick_box(
    runner: &dyn ProcessRunner,
    p: Provider,
    arm: bool,
) -> (&'static str, bool) {
    let candidates = candidate_boxes(p, arm);
    let list = exec::run(runner, "vagrant", &["box", "list"], None)
        .await
        .unwrap_or_default();
    if let Some(c) = candidates
        .iter()
        .copied()
        .find(|c| box_listed(&list, c, box_provider(p), arm))
    {
        return (c, true);
    }
    (
        candidates.first().copied().unwrap_or("generic/alpine319"),
        false,
    )
}

/// Whether `vagrant box list` has `name` for `provider` and this CPU (a box with no architecture in
/// the listing counts for any).
fn box_listed(list: &str, name: &str, provider: &str, arm: bool) -> bool {
    let arch = if arm { "arm64" } else { "amd64" };
    // Lines look like: `bento/debian-12   (virtualbox, 202510.26.0, (amd64))`
    list.lines().any(|l| {
        let mut parts = l.splitn(2, char::is_whitespace);
        let listed = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("");
        listed == name
            && rest.contains(&format!("({provider},"))
            && (!rest.contains("(amd64)") && !rest.contains("(arm64)")
                || rest.contains(&format!("({arch})")))
    })
}

/// `no_kvm`: a Linux host without a usable `/dev/kvm`. vagrant-qemu asks for `accel=kvm` with
/// `cpu=host` there, which QEMU refuses; the test VM falls back to software emulation (slow, but it
/// boots and the check says something true). `ip`: the VM's address on a private network, when it
/// gets one.
fn vagrantfile(p: Provider, bx: &str, no_kvm: bool, ip: Option<&str>) -> String {
    let network = match ip.filter(|_| has_private_network(p)) {
        Some(ip) => format!("  config.vm.network \"private_network\", ip: \"{ip}\"\n"),
        None => String::new(),
    };
    // With several virtual switches and none named, Vagrant would stop to ask which one.
    let hyperv = if p == Provider::Hyperv {
        "  config.vm.network \"public_network\", bridge: ENV[\"ISOLOOM_HYPERV_SWITCH\"]\n"
    } else {
        ""
    };
    // A readable name in the hypervisor's VM list instead of Vagrant's `<dir>_default_<timestamp>`.
    let settings = match p {
        Provider::Virtualbox | Provider::Parallels => {
            format!("    h.name = \"{VM_NAME}\"\n    h.memory = 512\n")
        }
        Provider::VmwareDesktop => {
            format!("    h.vmx[\"displayName\"] = \"{VM_NAME}\"\n    h.memory = 512\n")
        }
        Provider::Libvirt => {
            "    h.default_prefix = \"cyberctf-\"\n    h.memory = 512\n".to_owned()
        }
        Provider::Hyperv => format!("    h.vmname = \"{VM_NAME}\"\n"),
        Provider::Utm => format!("    h.name = \"{VM_NAME}\"\n"),
        // vagrant-qemu's default is 4G, much more than a test VM needs.
        Provider::Qemu if no_kvm => {
            "    h.memory = \"1G\"\n    h.machine = \"q35,accel=tcg\"\n    h.cpu = \"max\"\n"
                .to_owned()
        }
        Provider::Qemu => "    h.memory = \"1G\"\n".to_owned(),
        _ => String::new(),
    };
    let provider_block = if settings.is_empty() {
        String::new()
    } else {
        format!(
            "  config.vm.provider \"{}\" do |h|\n{settings}  end\n",
            p.id()
        )
    };
    format!(
        "Vagrant.configure(\"2\") do |config|\n  config.vm.box = \"{bx}\"\n  config.vm.hostname = \"cyberctf-selftest\"\n  config.vm.boot_timeout = 600\n  config.vm.synced_folder \".\", \"/vagrant\", disabled: true\n{network}{hyperv}{provider_block}end\n"
    )
}

/// One ping with a 3 s wait, in this OS's `ping` flags.
fn ping_args(ip: &str) -> [&str; 5] {
    if cfg!(windows) {
        ["-n", "1", "-w", "3000", ip]
    } else if cfg!(target_os = "macos") {
        ["-c", "1", "-t", "3", ip]
    } else {
        ["-c", "1", "-W", "3", ip]
    }
}

async fn destroy(runner: &dyn ProcessRunner, dir: &Path) -> Result<String> {
    exec::within(
        "vagrant destroy -f",
        DESTROY_LIMIT,
        exec::run(runner, "vagrant", &["destroy", "-f"], Some(dir)),
    )
    .await
}

/// The first test address nothing answers on yet (see [`VM_IPS`]).
async fn free_ip(runner: &dyn ProcessRunner) -> Option<&'static str> {
    for ip in VM_IPS {
        if exec::run(runner, "ping", &ping_args(ip), None)
            .await
            .is_err()
        {
            return Some(ip);
        }
    }
    None
}

/// What the command inside the test VM prints its answer after, so it is told from anything else in
/// the output (stderr shares it: warnings, "Connection to … closed.").
const UNAME_TAG: &str = "cyberctf-uname:";

/// The command run inside the test VM.
fn uname_command() -> String {
    format!("echo {UNAME_TAG}$(uname -srm)")
}

/// What `uname -srm` printed, out of the command's output.
fn uname_line(out: &str) -> String {
    out.lines()
        .find_map(|l| l.trim().strip_prefix(UNAME_TAG))
        .unwrap_or("")
        .trim()
        .to_owned()
}

pub(super) async fn run_test(
    runner: &dyn ProcessRunner,
    dir: &Path,
    preferred: Option<Provider>,
    elevate: Option<&dyn ElevatedBoot>,
    r: &Reporter,
) -> Result<()> {
    // A test interrupted last time (window closed) may have left its VM behind.
    if dir.join(".vagrant").exists() {
        let _discard = destroy(runner, dir).await;
    }
    let res = steps(runner, dir, preferred, elevate, r).await;
    // Destroy the VM whatever happened; the box stays cached for the next test.
    if dir.join("Vagrantfile").exists() {
        let down = destroy(runner, dir).await;
        r.cleanup(
            "Delete the test VM",
            &res,
            down,
            Some("test box kept for next time".to_owned()),
        );
    }
    res
}

async fn steps(
    runner: &dyn ProcessRunner,
    dir: &Path,
    preferred: Option<Provider>,
    elevate: Option<&dyn ElevatedBoot>,
    r: &Reporter,
) -> Result<()> {
    let provider = host_provider(runner, preferred, r).await?;
    let arm = arm();
    let (bx, _cached) = pick_box(runner, provider, arm).await;
    let test_box = TestBox {
        provider,
        name: bx,
        arm,
    };
    r.step("box", "Get a small test VM image", async {
        let had = ensure_box(runner, dir, &test_box, |l| {
            r.progress("box", "Get a small test VM image", &l);
        })
        .await?;
        Ok((
            (),
            Some(if had {
                format!("{bx} (ready)")
            } else {
                bx.to_owned()
            }),
        ))
    })
    .await?;

    // An address for the ping check that nothing answers on before the VM exists.
    let ip = if has_private_network(provider) {
        free_ip(runner).await
    } else {
        None
    };
    boot_and_exec(runner, dir, &Boot { provider, bx, ip }, elevate, r).await?;
    network(runner, provider, ip, r).await
}

/// Vagrant and a hypervisor that answers: the provider the test runs on.
async fn host_provider(
    runner: &dyn ProcessRunner,
    preferred: Option<Provider>,
    r: &Reporter,
) -> Result<Provider> {
    r.step("vagrant", "Vagrant is installed", async {
        let v = exec::run(runner, "vagrant", &["--version"], None).await?;
        Ok(((), Some(v.trim().to_owned())))
    })
    .await?;

    let provider = r
        .step("provider", "A hypervisor is ready", async {
            let p = test_provider(runner, preferred).await.ok_or_else(|| {
                Error::Invalid(
                    "no local hypervisor with its Vagrant plugin is installed".to_owned(),
                )
            })?;
            Ok((p, Some(p.id().to_owned())))
        })
        .await?;

    r.step("hypervisor", "Hypervisor responds", async {
        match responds(provider) {
            Some((program, args)) => {
                exec::run(runner, program, args, None).await?;
                Ok(((), None))
            }
            None => Ok(((), Some("nothing to probe for this provider".to_owned()))),
        }
    })
    .await?;

    Ok(provider)
}

/// Boots the test VM, then runs a command inside it. With `elevate`, both run as one elevated
/// batch (one Hyper-V prompt) and the probe's output is stashed for the exec step; without it, as
/// two plain runner calls (today's path, one prompt each on Hyper-V).
async fn boot_and_exec(
    runner: &dyn ProcessRunner,
    dir: &Path,
    boot: &Boot,
    elevate: Option<&dyn ElevatedBoot>,
    r: &Reporter,
) -> Result<()> {
    let command = uname_command();
    // The probe's outcome from the elevated batch, read by the exec step; `None` is the plain path.
    let mut probe: Option<Result<String>> = None;
    r.step("boot", "Boot the test VM", async {
        let no_kvm = cfg!(target_os = "linux") && providers::kvm_problem().is_some();
        std::fs::write(
            dir.join("Vagrantfile"),
            vagrantfile(boot.provider, boot.bx, no_kvm, boot.ip),
        )?;
        match elevate {
            Some(e) => probe = Some(elevated_boot(e, dir, boot.provider, &command, r).await?),
            None => plain_boot(runner, dir, boot.provider, r).await?,
        }
        Ok(((), Some(format!("{} on {}", boot.bx, boot.provider.id()))))
    })
    .await?;

    r.step("exec", "Run a command inside the VM", async {
        let out = match probe.take() {
            Some(result) => result?,
            None => {
                exec::within(
                    "vagrant ssh",
                    BOOT_LIMIT,
                    exec::run(runner, "vagrant", &["ssh", "-c", &command], Some(dir)),
                )
                .await?
            }
        };
        Ok(((), Some(uname_line(&out))))
    })
    .await?;

    Ok(())
}

/// Boots the test VM with a plain `vagrant up`, streaming its progress (the unelevated path).
async fn plain_boot(
    runner: &dyn ProcessRunner,
    dir: &Path,
    provider: Provider,
    r: &Reporter,
) -> Result<()> {
    let args = ["up", "--provider", provider.id()];
    let up = exec::stream(runner, "vagrant", &args, Some(dir), |l| {
        r.progress("boot", "Boot the test VM", l);
    });
    exec::within("vagrant up", BOOT_LIMIT, up).await
}

/// Boots and probes through the caller's elevation hook as one batch. Returns the probe's outcome
/// (for the exec step); the outer `Result` carries a boot failure or an elevation that couldn't run.
async fn elevated_boot(
    e: &dyn ElevatedBoot,
    dir: &Path,
    provider: Provider,
    command: &str,
    r: &Reporter,
) -> Result<Result<String>> {
    let up = ["up", "--provider", provider.id()];
    let ssh = ["ssh", "-c", command];
    let mut on_line = |l: &str| r.progress("boot", "Boot the test VM", l);
    let batch = e.boot_and_probe(dir, &up, &ssh, &mut on_line);
    let bp = exec::within("vagrant up / vagrant ssh", BOOT_LIMIT, batch).await?;
    bp.boot?;
    Ok(bp
        .probe
        .unwrap_or_else(|| Err(Error::Invalid("the command did not run".to_owned()))))
}

/// The VM answers ping on its private lab network (skipped without one, or a free address).
async fn network(
    runner: &dyn ProcessRunner,
    provider: Provider,
    ip: Option<&'static str>,
    r: &Reporter,
) -> Result<()> {
    if let Some(ip) = ip {
        r.step("network", "VM reachable on a lab network", async {
            let mut last = String::new();
            for _ in 0..PING_TRIES {
                match exec::run(runner, "ping", &ping_args(ip), None).await {
                    Ok(_) => return Ok(((), Some(format!("this machine → {ip}")))),
                    Err(e) => last = e.to_string(),
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(Error::Invalid(format!("{ip} did not answer ping: {last}")))
        })
        .await?;
    } else {
        let detail = if has_private_network(provider) {
            let first = VM_IPS.first().copied().unwrap_or_default();
            format!("every test address ({first}…) is taken on this machine")
        } else {
            format!("{} has no private networks", provider.id())
        };
        r.send(
            "network",
            "VM reachable on a lab network",
            "skip",
            Some(detail),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_output_is_told_from_vagrants_own_lines() {
        assert_eq!(
            uname_line(
                "warning: something\ncyberctf-uname:Linux 6.6 x86_64\nConnection to 172.20.0.5 closed.\n"
            ),
            "Linux 6.6 x86_64"
        );
        assert_eq!(uname_line("Connection to x closed.\n"), "");
        assert_eq!(uname_command(), "echo cyberctf-uname:$(uname -srm)");
    }

    #[test]
    fn a_box_counts_only_for_its_provider_and_cpu() {
        let list = "bento/debian-12   (virtualbox, 202510.26.0, (amd64))\ngeneric/alpine319 (libvirt, 4.3.12)\n";
        assert!(box_listed(list, "bento/debian-12", "virtualbox", false));
        assert!(!box_listed(list, "bento/debian-12", "virtualbox", true));
        assert!(!box_listed(list, "bento/debian-12", "parallels", false));
        assert!(
            box_listed(list, "generic/alpine319", "libvirt", true),
            "no architecture listed: any CPU"
        );
        assert!(!box_listed(list, "generic/alpine", "libvirt", true));
        assert!(!box_listed(
            "There are no installed boxes!\n",
            "generic/alpine319",
            "qemu",
            false
        ));
    }

    #[test]
    fn the_vagrantfile_carries_the_network_and_per_provider_settings() {
        let v = vagrantfile(Provider::Virtualbox, "box/x", false, Some(VM_IPS[0]));
        assert!(
            v.contains("ip: \"192.168.56.250\"") && v.contains("h.name = \"CyberCTF test VM\""),
            "{v}"
        );
        assert!(v.starts_with("Vagrant.configure") && v.ends_with("end\n"));
        assert!(
            !vagrantfile(Provider::Utm, "x", false, Some(VM_IPS[0])).contains(VM_IPS[0]),
            "UTM has no private network"
        );
        let hv = vagrantfile(Provider::Hyperv, "x", false, Some(VM_IPS[0]));
        assert!(
            hv.contains(
                "config.vm.network \"public_network\", bridge: ENV[\"ISOLOOM_HYPERV_SWITCH\"]"
            ),
            "{hv}"
        );
        assert!(!v.contains("public_network"));
    }

    #[test]
    fn qemu_test_vm_falls_back_to_software_emulation_without_kvm() {
        let with_kvm = vagrantfile(Provider::Qemu, "generic/alpine319", false, Some(VM_IPS[0]));
        assert!(
            with_kvm.contains("h.memory = \"1G\"") && !with_kvm.contains("accel=tcg"),
            "{with_kvm}"
        );
        let without = vagrantfile(Provider::Qemu, "generic/alpine319", true, Some(VM_IPS[0]));
        assert!(
            without.contains("h.machine = \"q35,accel=tcg\"")
                && without.contains("h.cpu = \"max\""),
            "{without}"
        );
        assert!(!vagrantfile(Provider::Virtualbox, "x", true, Some(VM_IPS[0])).contains("accel"));
    }

    #[test]
    fn probes_boxes_and_providers_are_per_provider() {
        assert_eq!(
            responds(Provider::Virtualbox).map(|r| r.0),
            Some("VBoxManage")
        );
        assert_eq!(responds(Provider::Libvirt).map(|r| r.0), Some("virsh"));
        assert!(responds(Provider::Utm).is_some_and(|r| r.0.ends_with("utmctl")));
        assert!(responds(Provider::Qemu).is_none());
        assert_eq!(
            candidate_boxes(Provider::Virtualbox, false),
            ["generic/alpine319", "bento/debian-12"]
        );
        assert_eq!(candidate_boxes(Provider::Utm, true), ["utm/bookworm"]);
        assert_eq!(candidate_boxes(Provider::Qemu, true), ["generic/alpine319"]);
        assert!(
            has_private_network(Provider::Libvirt)
                && !has_private_network(Provider::Qemu)
                && !has_private_network(Provider::Hyperv)
        );
        assert_eq!(box_provider(Provider::Qemu), "libvirt");
        assert_eq!(box_provider(Provider::Virtualbox), "virtualbox");
    }

    #[tokio::test]
    async fn the_test_vm_takes_an_address_nothing_answers_on_yet() {
        use isothrum_runner::process::fake::{FakeProcesses, Reply};
        let fake = FakeProcesses::new(|call| {
            match (call.program.as_str(), call.args.last().map(String::as_str)) {
                ("ping", Some("192.168.56.250")) => Reply::ok("64 bytes from 192.168.56.250"),
                ("ping", _) => Reply::fail(1, "Request timeout"),
                _ => Reply::ok(""),
            }
        });
        assert_eq!(free_ip(&fake).await, Some("192.168.56.249"));
        let v = vagrantfile(Provider::Virtualbox, "x", false, free_ip(&fake).await);
        assert!(
            v.contains("ip: \"192.168.56.249\"") && !v.contains(".250"),
            "{v}"
        );
        let all = FakeProcesses::rules(&[]);
        assert_eq!(free_ip(&all).await, None, "every address answers");
    }
}
