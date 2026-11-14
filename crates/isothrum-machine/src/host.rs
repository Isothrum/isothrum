//! This computer as machine setup reads it: its OS, its CPU and its package manager, each a closed
//! set, and the files detection reads (KVM's device, UTM's bundle, the kernel's module list...).
//! It is worked out once ([`Host::here`]) and handed to every rule that depends on it. A rule
//! matches on these enums, never on `std::env::consts` strings: an OS, CPU or package manager added
//! here is a compile error at every rule that doesn't say what it does there yet.
//!
//! A test builds its own (`Host { os, arch, packages, devices: Devices::under(dir) }`), so each
//! OS's rules run on any machine.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use isothrum_runner::ProcessRunner;
use serde::{Deserialize, Serialize};

use crate::exec;

/// An OS machine setup runs on, by the id Rust gives it (`std::env::consts::OS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    /// macOS.
    Macos,
    /// Windows.
    Windows,
    /// Linux.
    Linux,
}

impl Os {
    /// Every OS.
    pub const ALL: [Os; 3] = [Os::Macos, Os::Windows, Os::Linux];

    /// The OS this build runs on: Windows, macOS, else Linux (the runner's rule for any other
    /// Unix).
    pub const HERE: Os = if cfg!(windows) {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Macos
    } else {
        Os::Linux
    };

    /// Its id, `std::env::consts::OS`'s (`macos`), as its serde form writes it.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Os::Macos => "macos",
            Os::Windows => "windows",
            Os::Linux => "linux",
        }
    }

    /// Its name as people write it (`macOS`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Os::Macos => "macOS",
            Os::Windows => "Windows",
            Os::Linux => "Linux",
        }
    }

    /// The package manager setup installs through on this OS when the machine has it: the one to
    /// name when it is missing (Linux's first, apt).
    #[must_use]
    pub fn usual_packages(self) -> &'static str {
        match self {
            Os::Macos => "Homebrew",
            Os::Windows => "winget",
            Os::Linux => "apt",
        }
    }
}

/// A CPU machine setup runs on, by the name Rust gives it (`std::env::consts::ARCH`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Arch {
    /// 64-bit x86 (Intel, AMD).
    #[serde(rename = "x86_64")]
    X86_64,
    /// 64-bit Arm (Apple silicon, Windows on Arm, Arm Linux).
    #[serde(rename = "aarch64")]
    Aarch64,
}

impl Arch {
    /// Every CPU.
    pub const ALL: [Arch; 2] = [Arch::X86_64, Arch::Aarch64];

    /// The CPU this build runs on: Arm for an `aarch64` build, else x86-64 (the runner's rule).
    pub const HERE: Arch = if cfg!(target_arch = "aarch64") { Arch::Aarch64 } else { Arch::X86_64 };

    /// Its id, `std::env::consts::ARCH`'s (`aarch64`), as its serde form writes it.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        }
    }

    /// Its name for Docker images, Vagrant boxes and vendors' downloads (`amd64`, `arm64`).
    #[must_use]
    pub fn docker_name(self) -> &'static str {
        match self {
            Arch::X86_64 => "amd64",
            Arch::Aarch64 => "arm64",
        }
    }

    /// QEMU's system emulator for it (`qemu-system-x86_64`).
    #[must_use]
    pub fn qemu_system(self) -> &'static str {
        match self {
            Arch::X86_64 => "qemu-system-x86_64",
            Arch::Aarch64 => "qemu-system-aarch64",
        }
    }
}

/// The package manager setup installs and removes tools through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packages {
    /// Homebrew (macOS), at this path.
    Homebrew(&'static str),
    /// winget (Windows).
    Winget,
    /// A Linux distribution's.
    Linux(LinuxPackages),
}

/// A Linux distribution's package manager setup goes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxPackages {
    /// apt-get: Debian, Ubuntu and the distributions built on them.
    Apt,
    /// dnf: Fedora and the distributions built on it.
    Dnf,
}

impl Packages {
    /// Its name (`Homebrew`, `winget`, `apt`, `dnf`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Packages::Homebrew(_) => "Homebrew",
            Packages::Winget => "winget",
            Packages::Linux(LinuxPackages::Apt) => "apt",
            Packages::Linux(LinuxPackages::Dnf) => "dnf",
        }
    }

    /// The program that answers its `--version`.
    #[must_use]
    pub fn program(self) -> &'static str {
        match self {
            Packages::Homebrew(brew) => brew,
            Packages::Winget => "winget",
            Packages::Linux(LinuxPackages::Apt) => "apt-get",
            Packages::Linux(LinuxPackages::Dnf) => "dnf",
        }
    }

    /// The Linux distribution's, when it is one.
    #[must_use]
    pub fn linux(self) -> Option<LinuxPackages> {
        match self {
            Packages::Linux(packages) => Some(packages),
            Packages::Homebrew(_) | Packages::Winget => None,
        }
    }
}

/// Where Homebrew's installer puts it: Apple silicon's, then Intel's.
const HOMEBREW: [&str; 2] = ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"];

/// The files and the one local service detection reads instead of running a tool: each a path a
/// test points into a folder of its own ([`Devices::under`]), so no test reads the machine running
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Devices {
    /// KVM's device (`/dev/kvm`), which libvirt's and QEMU's guests run on (Linux).
    pub kvm: PathBuf,
    /// The group database (`/etc/group`): the name of the group `/dev/kvm` belongs to.
    pub etc_group: PathBuf,
    /// The kernel modules loaded (`/proc/modules`): VirtualBox's driver, KVM's CPU module.
    pub proc_modules: PathBuf,
    /// The running kernel's release (`/proc/sys/kernel/osrelease`, what `uname -r` says).
    pub kernel_release: PathBuf,
    /// The kernels' module folders (`/lib/modules`), each with a `build` link once its headers
    /// are installed.
    pub lib_modules: PathBuf,
    /// KVM's `enable_virt_at_load` parameter (`/sys/module/kvm/parameters/enable_virt_at_load`,
    /// Linux 6.12 and later).
    pub kvm_virt_at_load: PathBuf,
    /// UTM's app bundle (`/Applications/UTM.app`, macOS).
    pub utm_app: PathBuf,
    /// Hyper-V's Virtual Machine Management service (`%SystemRoot%\System32\vmms.exe`), there
    /// once the Windows feature is on.
    pub hyperv_service: PathBuf,
    /// What proves HashiCorp's Vagrant VMware Utility is installed: its program on macOS and
    /// Linux, the certificates its installer generates (under `%ProgramData%`) on Windows.
    pub vmware_utility: PathBuf,
    /// Where the Vagrant VMware Utility's service answers when it runs: this machine's port 9922,
    /// the plugin's default (`utility_port`).
    pub vmware_utility_service: SocketAddr,
}

/// The port the Vagrant VMware Utility's service listens on.
pub const VMWARE_UTILITY_PORT: u16 = 9922;

impl Devices {
    /// This computer's, on `os`: where each OS keeps them.
    #[must_use]
    pub fn here(os: Os) -> Devices {
        let windows_folder = |var: &str, default: &str| std::env::var_os(var).map_or_else(|| PathBuf::from(default), PathBuf::from);
        let vmware_utility = match os {
            Os::Windows => windows_folder("ProgramData", "C:\\ProgramData")
                .join("HashiCorp")
                .join("vagrant-vmware-desktop")
                .join("certificates"),
            Os::Macos | Os::Linux => PathBuf::from("/opt/vagrant-vmware-desktop/bin/vagrant-vmware-utility"),
        };
        Devices {
            kvm: PathBuf::from("/dev/kvm"),
            etc_group: PathBuf::from("/etc/group"),
            proc_modules: PathBuf::from("/proc/modules"),
            kernel_release: PathBuf::from("/proc/sys/kernel/osrelease"),
            lib_modules: PathBuf::from("/lib/modules"),
            kvm_virt_at_load: PathBuf::from("/sys/module/kvm/parameters/enable_virt_at_load"),
            utm_app: PathBuf::from("/Applications/UTM.app"),
            hyperv_service: windows_folder("SystemRoot", "C:\\Windows").join("System32").join("vmms.exe"),
            vmware_utility,
            vmware_utility_service: SocketAddr::from((Ipv4Addr::LOCALHOST, VMWARE_UTILITY_PORT)),
        }
    }

    /// Each one under the folder `root`, where a test makes the ones it is about (`root/dev/kvm`,
    /// `root/proc/modules`, `root/Applications/UTM.app`...), none of which exist until it does;
    /// the VMware Utility's service at port 0 of this machine, which nothing answers.
    #[must_use]
    pub fn under(root: &Path) -> Devices {
        Devices {
            kvm: root.join("dev/kvm"),
            etc_group: root.join("etc/group"),
            proc_modules: root.join("proc/modules"),
            kernel_release: root.join("proc/sys/kernel/osrelease"),
            lib_modules: root.join("lib/modules"),
            kvm_virt_at_load: root.join("sys/module/kvm/parameters/enable_virt_at_load"),
            utm_app: root.join("Applications/UTM.app"),
            hyperv_service: root.join("Windows/System32/vmms.exe"),
            vmware_utility: root.join("opt/vagrant-vmware-desktop/bin/vagrant-vmware-utility"),
            vmware_utility_service: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        }
    }
}

/// This computer: its OS, its CPU, the package manager setup goes through here (`None`: a Mac
/// without Homebrew, a Linux distribution with neither apt nor dnf), and the files detection
/// reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// The OS.
    pub os: Os,
    /// The CPU.
    pub arch: Arch,
    /// The package manager, when the machine has one setup goes through.
    pub packages: Option<Packages>,
    /// The files and the service detection reads.
    pub devices: Devices,
}

impl Host {
    /// This computer, its package manager looked up now: Homebrew where its installer puts it;
    /// `apt-get`, else `dnf`, when it answers `--version` (through `runner`).
    pub async fn here(runner: &dyn ProcessRunner) -> Host {
        let packages = match Os::HERE {
            Os::Macos => HOMEBREW.into_iter().find(|brew| Path::new(brew).exists()).map(Packages::Homebrew),
            Os::Windows => Some(Packages::Winget),
            Os::Linux => linux_packages(runner).await.map(Packages::Linux),
        };
        Host {
            os: Os::HERE,
            arch: Arch::HERE,
            packages,
            devices: Devices::here(Os::HERE),
        }
    }

    /// Whether it is a PC with Windows on Arm.
    #[must_use]
    pub fn is_windows_on_arm(&self) -> bool {
        self.os == Os::Windows && self.arch == Arch::Aarch64
    }
}

/// The Linux distribution's package manager `runner` finds: apt-get, else dnf.
async fn linux_packages(runner: &dyn ProcessRunner) -> Option<LinuxPackages> {
    for (packages, program) in [(LinuxPackages::Apt, "apt-get"), (LinuxPackages::Dnf, "dnf")] {
        if exec::read_ok(runner, crate::context::PROBE_TIMEOUT, program, &["--version"]).await.is_ok() {
            return Some(packages);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use isothrum_runner::fake::{FakeProcesses, Reply};

    use super::*;

    #[test]
    fn the_os_and_cpu_here_are_rusts() {
        assert_eq!(Os::HERE.id(), std::env::consts::OS);
        assert_eq!(Arch::HERE.id(), std::env::consts::ARCH);
    }

    #[test]
    fn each_os_and_cpu_is_written_by_its_id() {
        for os in Os::ALL {
            assert_eq!(serde_json::to_value(os).unwrap(), os.id());
            assert_eq!(serde_json::from_value::<Os>(os.id().into()).unwrap(), os);
        }
        for arch in Arch::ALL {
            assert_eq!(serde_json::to_value(arch).unwrap(), arch.id());
            assert_eq!(serde_json::from_value::<Arch>(arch.id().into()).unwrap(), arch);
        }
        assert_eq!(Os::Macos.name(), "macOS");
    }

    #[test]
    fn a_cpu_has_rusts_name_and_dockers() {
        assert_eq!((Arch::X86_64.id(), Arch::X86_64.docker_name()), ("x86_64", "amd64"));
        assert_eq!((Arch::Aarch64.id(), Arch::Aarch64.docker_name()), ("aarch64", "arm64"));
        assert_eq!(Arch::Aarch64.qemu_system(), "qemu-system-aarch64");
        assert_eq!(Arch::X86_64.qemu_system(), "qemu-system-x86_64");
    }

    #[test]
    fn each_package_manager_names_its_program() {
        let all = [
            Packages::Homebrew("/opt/homebrew/bin/brew"),
            Packages::Winget,
            Packages::Linux(LinuxPackages::Apt),
            Packages::Linux(LinuxPackages::Dnf),
        ];
        let named: Vec<_> = all.iter().map(|p| (p.name(), p.program(), p.linux())).collect();
        assert_eq!(
            named,
            [
                ("Homebrew", "/opt/homebrew/bin/brew", None),
                ("winget", "winget", None),
                ("apt", "apt-get", Some(LinuxPackages::Apt)),
                ("dnf", "dnf", Some(LinuxPackages::Dnf)),
            ]
        );
    }

    #[test]
    fn only_a_windows_pc_on_arm_is_windows_on_arm() {
        let dir = Path::new("unused");
        let on = |os, arch| Host {
            os,
            arch,
            packages: None,
            devices: Devices::under(dir),
        };
        assert!(on(Os::Windows, Arch::Aarch64).is_windows_on_arm());
        assert!(!on(Os::Windows, Arch::X86_64).is_windows_on_arm());
        assert!(!on(Os::Macos, Arch::Aarch64).is_windows_on_arm());
    }

    #[test]
    fn a_tests_devices_are_all_under_its_folder() {
        let root = Path::new("/the/test");
        let devices = Devices::under(root);
        let paths = [
            &devices.kvm,
            &devices.etc_group,
            &devices.proc_modules,
            &devices.kernel_release,
            &devices.lib_modules,
            &devices.kvm_virt_at_load,
            &devices.utm_app,
            &devices.hyperv_service,
            &devices.vmware_utility,
        ];
        for path in paths {
            assert!(path.starts_with(root), "{path:?}");
        }
        assert_eq!(devices.vmware_utility_service.port(), 0);
        assert!(devices.vmware_utility_service.ip().is_loopback());
    }

    #[test]
    fn each_oss_devices_are_where_it_keeps_them() {
        let linux = Devices::here(Os::Linux);
        assert_eq!(linux.kvm, Path::new("/dev/kvm"));
        assert_eq!(linux.vmware_utility, Path::new("/opt/vagrant-vmware-desktop/bin/vagrant-vmware-utility"));
        assert_eq!(linux.vmware_utility_service, SocketAddr::from((Ipv4Addr::LOCALHOST, 9922)));
        let windows = Devices::here(Os::Windows);
        assert!(
            windows.vmware_utility.ends_with("HashiCorp/vagrant-vmware-desktop/certificates"),
            "{:?}",
            windows.vmware_utility
        );
        assert!(windows.hyperv_service.ends_with("System32/vmms.exe"), "{:?}", windows.hyperv_service);
        assert_eq!(Devices::here(Os::Macos).utm_app, Path::new("/Applications/UTM.app"));
    }

    #[tokio::test(start_paused = true)]
    async fn linux_goes_through_apt_else_dnf_else_none() {
        let apt = FakeProcesses::script().expect("apt-get --version", Reply::ok("apt 2.9.8 (amd64)")).build();
        assert_eq!(linux_packages(&apt).await, Some(LinuxPackages::Apt));
        let dnf = FakeProcesses::script()
            .expect("apt-get --version", Reply::missing())
            .expect("dnf --version", Reply::ok("4.21.1"))
            .build();
        assert_eq!(linux_packages(&dnf).await, Some(LinuxPackages::Dnf));
        let neither = FakeProcesses::script()
            .expect("apt-get --version", Reply::missing())
            .expect("dnf --version", Reply::fail(127, "dnf: command not found"))
            .build();
        assert_eq!(linux_packages(&neither).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn this_computer_is_the_builds_os_and_cpu_with_its_oss_package_manager() {
        let tools = FakeProcesses::script()
            .allow("apt-get --version", Reply::ok("apt 2.9.8"))
            .allow("dnf --version", Reply::missing())
            .build();
        let host = Host::here(&tools).await;
        assert_eq!((host.os, host.arch), (Os::HERE, Arch::HERE));
        assert_eq!(host.devices, Devices::here(Os::HERE));
        match (host.os, host.packages) {
            (Os::Macos, None | Some(Packages::Homebrew(_)))
            | (Os::Windows, Some(Packages::Winget))
            | (Os::Linux, Some(Packages::Linux(LinuxPackages::Apt))) => {}
            other => panic!("{other:?}"),
        }
    }
}
