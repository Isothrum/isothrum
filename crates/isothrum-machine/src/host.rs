//! This computer as machine setup reads it: its OS, its CPU and its package manager, each a closed
//! set, and the files detection reads (KVM's device, UTM's bundle, the kernel's module list...).
//! It is worked out once ([`Host::here`]) and handed to every rule that depends on it. A rule
//! matches on these enums (the OS and CPU are the runner's [`HostOs`] and [`Arch`]), never on
//! `std::env::consts` strings: an OS, CPU or package manager added is a compile error at every
//! rule that doesn't say what it does there yet.
//!
//! A test builds its own (`Host { os, arch, packages, devices: Devices::under(dir) }`), so each
//! OS's rules run on any machine.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use isothrum_runner::{ProcessRunner, this_arch, this_host};

use crate::exec;
use crate::providers::virtiofsd::{VAGRANT_QEMU_FALLBACKS, VHOST_USER_DIRS, vagrant_qemu_candidates};

/// The OS and the CPU machine setup runs on: the runner's ([`isothrum_runner::this_host`],
/// [`isothrum_runner::this_arch`]), written as the runner writes them (`macos`, `amd64`).
pub use isothrum_runner::{Arch, HostOs};

pub mod tool_path;

/// Every OS.
pub const ALL_OSES: [HostOs; 3] = [HostOs::Macos, HostOs::Windows, HostOs::Linux];

/// Every CPU.
pub const ALL_ARCHES: [Arch; 2] = [Arch::Amd64, Arch::Arm64];

/// `os`'s name as people write it (`macOS`).
#[must_use]
pub fn os_name(os: HostOs) -> &'static str {
    match os {
        HostOs::Macos => "macOS",
        HostOs::Windows => "Windows",
        HostOs::Linux => "Linux",
    }
}

/// The package manager setup installs through on `os` when the machine has it: the one to name
/// when it is missing (Linux's first, apt).
#[must_use]
pub fn usual_packages(os: HostOs) -> &'static str {
    match os {
        HostOs::Macos => "Homebrew",
        HostOs::Windows => "winget",
        HostOs::Linux => "apt",
    }
}

/// QEMU's system emulator for a CPU `arch` (`qemu-system-x86_64`).
#[must_use]
pub fn qemu_system(arch: Arch) -> &'static str {
    match arch {
        Arch::Amd64 => "qemu-system-x86_64",
        Arch::Arm64 => "qemu-system-aarch64",
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
    /// The loaded modules' folders (`/sys/module`): each loadable module's `refcnt` (how many
    /// hold it) and `holders` (the modules among them).
    pub sys_module: PathBuf,
    /// The folder of the options modprobe gives a module as it loads it (`/etc/modprobe.d`, its
    /// `*.conf` files).
    pub modprobe_d: PathBuf,
    /// The Applications folder macOS apps install into (`/Applications`): a tool's app bundle
    /// ([`crate::install::ToolRow::mac_app`]) is looked for there, and Docker Desktop's own
    /// `docker` CLI inside its bundle.
    pub applications: PathBuf,
    /// Docker Desktop's socket (`~/.docker/run/docker.sock`, macOS), which listens once its
    /// backend runs: never, while Docker Desktop waits for its user in its own window.
    pub docker_desktop_socket: PathBuf,
    /// UTM's app bundle (macOS): in `/Applications`, else in the user's own `~/Applications` (an
    /// install without an administrator's password); `/Applications/UTM.app` when in neither.
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
    /// Where VMware's apps install `vmrun`, looked at when it isn't on `PATH`: Fusion's
    /// `Contents/Public/vmrun` in `/Applications`, then in `~/Applications` (macOS); Workstation's
    /// folder under `%ProgramFiles(x86)%`, then `%ProgramFiles%` (Windows).
    pub vmrun: Vec<PathBuf>,
    /// Where Parallels Desktop installs `prlctl`, looked at when it isn't on `PATH`: the app's
    /// `Contents/MacOS/prlctl` in `/Applications`, then in `~/Applications`.
    pub prlctl: Vec<PathBuf>,
    /// The folders libvirt reads QEMU's vhost-user descriptors from, in its order, a later
    /// folder's file taking the place of an earlier one's of the same name: the distribution's
    /// (`/usr/share/qemu/vhost-user`), then an administrator's (`/etc/qemu/vhost-user`) (Linux).
    pub vhost_user: Vec<PathBuf>,
    /// Where vagrant-qemu looks for the virtiofsd it shares folders through, in its order:
    /// `virtiofsd` in each folder of `PATH`, then `/usr/libexec/virtiofsd`, then
    /// `/usr/lib/qemu/virtiofsd` (Linux).
    pub virtiofsd: Vec<PathBuf>,
    /// Where VMware's hypervisors keep their own OVF Tool (`ovftool`), in the order to look:
    /// Fusion's `Contents/Library/VMware OVF Tool` in `/Applications`, then in `~/Applications`
    /// (macOS); Workstation's `OVFTool` folder under `%ProgramFiles(x86)%`, then `%ProgramFiles%`
    /// (Windows). Workstation for Linux puts it in `/usr/bin`, on `PATH`.
    pub ovftool: Vec<PathBuf>,
    /// Rosetta 2's runtime (`/Library/Apple/usr/libexec/oah/libRosettaRuntime`), there once it is
    /// installed (macOS on Apple silicon).
    pub rosetta_runtime: PathBuf,
}

/// The port the Vagrant VMware Utility's service listens on.
pub const VMWARE_UTILITY_PORT: u16 = 9922;

/// Fusion's `vmrun` and Parallels Desktop's `prlctl` in the app bundles under each of `bases`
/// (`/`, then the user's home), each base's `Applications` folder.
fn mac_apps(bases: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let apps = |bundle: &str, inside: &str| bases.iter().map(|b| b.join("Applications").join(bundle).join(inside)).collect();
    (
        apps("VMware Fusion.app", "Contents/Public/vmrun"),
        apps("Parallels Desktop.app", "Contents/MacOS/prlctl"),
    )
}

/// UTM's app bundle in the `Applications` folder of the first of `bases` (`/`, then the user's
/// home) that has it; the first base's when none has.
fn utm_app(bases: &[PathBuf]) -> PathBuf {
    let in_each = bases.iter().map(|b| b.join("Applications").join("UTM.app"));
    let first = in_each.clone().next().unwrap_or_else(|| PathBuf::from("/Applications/UTM.app"));
    in_each.into_iter().find(|app| app.exists()).unwrap_or(first)
}

/// Fusion's OVF Tool in the app bundles under each of `bases` (`/`, then the user's home).
fn fusion_ovftool(bases: &[PathBuf]) -> Vec<PathBuf> {
    bases
        .iter()
        .map(|b| b.join("Applications/VMware Fusion.app/Contents/Library/VMware OVF Tool/ovftool"))
        .collect()
}

/// Workstation's OVF Tool under each of the install folders `bases`.
fn workstation_ovftool(bases: &[PathBuf]) -> Vec<PathBuf> {
    bases.iter().map(|b| crate::tools::ovftool::workstation_dir(b).join("ovftool.exe")).collect()
}

/// Rosetta 2's runtime under `root`.
const ROSETTA_RUNTIME: &str = "Library/Apple/usr/libexec/oah/libRosettaRuntime";

/// Workstation's `vmrun.exe` under each of the install folders `bases`.
fn workstation(bases: &[PathBuf]) -> Vec<PathBuf> {
    bases.iter().map(|b| b.join("VMware").join("VMware Workstation").join("vmrun.exe")).collect()
}

impl Devices {
    /// This computer's, on `os`: where each OS keeps them.
    #[must_use]
    pub fn here(os: HostOs) -> Devices {
        let windows_folder = |var: &str, default: &str| std::env::var_os(var).map_or_else(|| PathBuf::from(default), PathBuf::from);
        let mut mac_bases = vec![PathBuf::from("/")];
        mac_bases.extend(std::env::var_os("HOME").map(PathBuf::from));
        let program_files: Vec<PathBuf> = ["ProgramFiles(x86)", "ProgramFiles"]
            .iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
            .collect();
        let (vmrun, prlctl) = match os {
            HostOs::Macos => mac_apps(&mac_bases),
            HostOs::Windows => (workstation(&program_files), Vec::new()),
            HostOs::Linux => (Vec::new(), Vec::new()),
        };
        let ovftool = match os {
            HostOs::Macos => fusion_ovftool(&mac_bases),
            HostOs::Windows => workstation_ovftool(&program_files),
            HostOs::Linux => Vec::new(),
        };
        let vmware_utility = match os {
            HostOs::Windows => windows_folder("ProgramData", "C:\\ProgramData")
                .join("HashiCorp")
                .join("vagrant-vmware-desktop")
                .join("certificates"),
            HostOs::Macos | HostOs::Linux => PathBuf::from("/opt/vagrant-vmware-desktop/bin/vagrant-vmware-utility"),
        };
        let (vhost_user, virtiofsd) = match os {
            HostOs::Linux => (
                VHOST_USER_DIRS.iter().map(PathBuf::from).collect(),
                vagrant_qemu_candidates(std::env::var_os("PATH").as_deref(), &VAGRANT_QEMU_FALLBACKS),
            ),
            HostOs::Macos | HostOs::Windows => (Vec::new(), Vec::new()),
        };
        Devices {
            kvm: PathBuf::from("/dev/kvm"),
            etc_group: PathBuf::from("/etc/group"),
            proc_modules: PathBuf::from("/proc/modules"),
            kernel_release: PathBuf::from("/proc/sys/kernel/osrelease"),
            lib_modules: PathBuf::from("/lib/modules"),
            kvm_virt_at_load: PathBuf::from("/sys/module/kvm/parameters/enable_virt_at_load"),
            sys_module: PathBuf::from("/sys/module"),
            modprobe_d: PathBuf::from("/etc/modprobe.d"),
            applications: PathBuf::from("/Applications"),
            docker_desktop_socket: std::env::var_os("HOME")
                .map_or_else(|| PathBuf::from("/nonexistent"), PathBuf::from)
                .join(".docker/run/docker.sock"),
            utm_app: utm_app(&mac_bases),
            hyperv_service: windows_folder("SystemRoot", "C:\\Windows").join("System32").join("vmms.exe"),
            vmware_utility,
            vmware_utility_service: SocketAddr::from((Ipv4Addr::LOCALHOST, VMWARE_UTILITY_PORT)),
            vmrun,
            prlctl,
            vhost_user,
            virtiofsd,
            ovftool,
            rosetta_runtime: Path::new("/").join(ROSETTA_RUNTIME),
        }
    }

    /// Each one under the folder `root`, where a test makes the ones it is about (`root/dev/kvm`,
    /// `root/proc/modules`, `root/Applications/UTM.app`...), none of which exist until it does;
    /// the VMware Utility's service at port 0 of this machine, which nothing answers. The apps'
    /// tools (the hypervisors' OVF Tools too) are under `root` and `root/home` (macOS's), and
    /// `root/Program Files (x86)` and `root/Program Files` (Workstation's); Rosetta's runtime
    /// under `root`; libvirt's descriptor folders and vagrant-qemu's
    /// virtiofsd fallbacks under `root`, with no `PATH` folder before them.
    #[must_use]
    pub fn under(root: &Path) -> Devices {
        let (fusion, prlctl) = mac_apps(&[root.to_path_buf(), root.join("home")]);
        let mut vmrun = fusion;
        let program_files = [root.join("Program Files (x86)"), root.join("Program Files")];
        vmrun.extend(workstation(&program_files));
        let mut ovftool = fusion_ovftool(&[root.to_path_buf(), root.join("home")]);
        ovftool.extend(workstation_ovftool(&program_files));
        Devices {
            kvm: root.join("dev/kvm"),
            etc_group: root.join("etc/group"),
            proc_modules: root.join("proc/modules"),
            kernel_release: root.join("proc/sys/kernel/osrelease"),
            lib_modules: root.join("lib/modules"),
            kvm_virt_at_load: root.join("sys/module/kvm/parameters/enable_virt_at_load"),
            sys_module: root.join("sys/module"),
            modprobe_d: root.join("etc/modprobe.d"),
            applications: root.join("Applications"),
            docker_desktop_socket: root.join("home/.docker/run/docker.sock"),
            utm_app: root.join("Applications/UTM.app"),
            hyperv_service: root.join("Windows/System32/vmms.exe"),
            vmware_utility: root.join("opt/vagrant-vmware-desktop/bin/vagrant-vmware-utility"),
            vmware_utility_service: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            vmrun,
            prlctl,
            vhost_user: VHOST_USER_DIRS.iter().map(|d| root.join(d.trim_start_matches('/'))).collect(),
            virtiofsd: VAGRANT_QEMU_FALLBACKS.iter().map(|f| root.join(f.trim_start_matches('/'))).collect(),
            ovftool,
            rosetta_runtime: root.join(ROSETTA_RUNTIME),
        }
    }
}

impl Devices {
    /// Where an app installs `program` off `PATH` ([`Devices::vmrun`], [`Devices::prlctl`]), in
    /// the order to look; none for a tool no app here installs.
    #[must_use]
    pub fn app_tool(&self, program: &str) -> Vec<&Path> {
        let at = match program {
            "vmrun" => &self.vmrun,
            "prlctl" => &self.prlctl,
            _ => return Vec::new(),
        };
        at.iter().map(PathBuf::as_path).collect()
    }
}

/// This computer: its OS, its CPU, the package manager setup goes through here (`None`: a Mac
/// without Homebrew, a Linux distribution with neither apt nor dnf), and the files detection
/// reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// The OS.
    pub os: HostOs,
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
        let os = this_host();
        let packages = match os {
            HostOs::Macos => HOMEBREW.into_iter().find(|brew| Path::new(brew).exists()).map(Packages::Homebrew),
            HostOs::Windows => Some(Packages::Winget),
            HostOs::Linux => linux_packages(runner).await.map(Packages::Linux),
        };
        Host {
            os,
            arch: this_arch(),
            packages,
            devices: Devices::here(os),
        }
    }

    /// Whether it is a PC with Windows on Arm.
    #[must_use]
    pub fn is_windows_on_arm(&self) -> bool {
        self.os == HostOs::Windows && self.arch == Arch::Arm64
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

    /// Every OS once, in the order a UI lists them, with its name and usual package manager.
    #[test]
    fn each_os_is_listed_once_with_its_name_and_its_usual_package_manager() {
        let named: Vec<_> = ALL_OSES.iter().map(|os| (*os, os_name(*os), usual_packages(*os))).collect();
        assert_eq!(
            named,
            [
                (HostOs::Macos, "macOS", "Homebrew"),
                (HostOs::Windows, "Windows", "winget"),
                (HostOs::Linux, "Linux", "apt")
            ]
        );
    }

    /// Every CPU once, with QEMU's system emulator for it.
    #[test]
    fn each_cpu_is_listed_once_with_its_qemu_system() {
        let named: Vec<_> = ALL_ARCHES.iter().map(|arch| (*arch, qemu_system(*arch))).collect();
        assert_eq!(named, [(Arch::Amd64, "qemu-system-x86_64"), (Arch::Arm64, "qemu-system-aarch64")]);
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
        assert!(on(HostOs::Windows, Arch::Arm64).is_windows_on_arm());
        assert!(!on(HostOs::Windows, Arch::Amd64).is_windows_on_arm());
        assert!(!on(HostOs::Macos, Arch::Arm64).is_windows_on_arm());
    }

    /// A test's host reads nothing of the machine running it: every path under its folder, the
    /// apps' tools included, and no service that answers.
    #[test]
    fn a_tests_devices_are_all_under_its_folder() {
        let root = Path::new("/the/test");
        let devices = Devices::under(root);
        let fixed = [
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
        let paths = fixed
            .into_iter()
            .chain(&devices.vmrun)
            .chain(&devices.prlctl)
            .chain(&devices.vhost_user)
            .chain(&devices.virtiofsd);
        isothrum_test_support::assert_every!(paths, |path: &&PathBuf| path.starts_with(root));
        assert_eq!(devices.vmware_utility_service.port(), 0);
        assert!(devices.vmware_utility_service.ip().is_loopback());
    }

    #[test]
    fn utm_is_found_in_the_users_own_applications_folder_too() {
        // An install without an administrator's password goes in ~/Applications: only
        // /Applications/UTM.app counted, so that UTM read as not installed.
        let (system, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let bases = [system.path().to_path_buf(), home.path().to_path_buf()];
        let in_system = system.path().join("Applications/UTM.app");
        assert_eq!(utm_app(&bases), in_system, "in neither: the system's, which isn't there");
        std::fs::create_dir_all(home.path().join("Applications/UTM.app")).unwrap();
        assert_eq!(utm_app(&bases), home.path().join("Applications/UTM.app"));
        std::fs::create_dir_all(&in_system).unwrap();
        assert_eq!(utm_app(&bases), in_system, "the system's first");
        assert_eq!(utm_app(&[]), Path::new("/Applications/UTM.app"));
    }

    #[test]
    fn each_oss_devices_are_where_it_keeps_them() {
        let linux = Devices::here(HostOs::Linux);
        assert_eq!(linux.kvm, Path::new("/dev/kvm"));
        assert_eq!(linux.vmware_utility, Path::new("/opt/vagrant-vmware-desktop/bin/vagrant-vmware-utility"));
        assert_eq!(linux.vmware_utility_service, SocketAddr::from((Ipv4Addr::LOCALHOST, 9922)));
        assert_eq!(linux.vhost_user, [Path::new("/usr/share/qemu/vhost-user"), Path::new("/etc/qemu/vhost-user")]);
        assert!(
            linux
                .virtiofsd
                .ends_with(&[PathBuf::from("/usr/libexec/virtiofsd"), PathBuf::from("/usr/lib/qemu/virtiofsd")]),
            "the PATH's, then vagrant-qemu's fallbacks: {:?}",
            linux.virtiofsd
        );
        assert!(Devices::here(HostOs::Macos).virtiofsd.is_empty() && Devices::here(HostOs::Windows).vhost_user.is_empty());
        let windows = Devices::here(HostOs::Windows);
        assert!(
            windows.vmware_utility.ends_with("HashiCorp/vagrant-vmware-desktop/certificates"),
            "{:?}",
            windows.vmware_utility
        );
        assert!(windows.hyperv_service.ends_with("System32/vmms.exe"), "{:?}", windows.hyperv_service);
        assert!(Devices::here(HostOs::Macos).utm_app.ends_with("Applications/UTM.app"));
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
        assert_eq!((host.os, host.arch), (this_host(), this_arch()));
        assert_eq!(host.devices, Devices::here(this_host()));
        match (host.os, host.packages) {
            (HostOs::Macos, None | Some(Packages::Homebrew(_)))
            | (HostOs::Windows, Some(Packages::Winget))
            | (HostOs::Linux, Some(Packages::Linux(LinuxPackages::Apt))) => {}
            other => panic!("{other:?}"),
        }
    }
}
