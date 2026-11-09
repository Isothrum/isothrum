//! What the crate's tests share: the manifests shaped like Isoloom's fixtures, a run folder
//! holding one, and a runner configuration on fakes with a fixed clock.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use isoloom_manifest::{HostOs, Manifest};

use crate::clock::FixedClock;
use crate::net::FakePorts;
use crate::process::fake::{Call, FakeProcesses, Reply};
use crate::{HostPorts, ProcessRunner, RunnerConfig};

/// The `flag-portal` docker manifest's text.
pub(crate) const FLAG_PORTAL: &str = include_str!("../tests/fixtures/flag-portal.run.json");

/// `flag-portal` with an on-demand access box (a Compose `attacker` service in the `access`
/// profile): the hot-add op's docker fixture.
pub(crate) const FLAG_PORTAL_ACCESS: &str = include_str!("../tests/fixtures/flag-portal-access.run.json");

/// `flag-vm` with an on-demand access VM (a Vagrant `attacker` define): the hot-add op's VM fixture.
pub(crate) const FLAG_VM_ACCESS: &str = include_str!("../tests/fixtures/flag-vm-access.run.json");

/// Isoloom's `proxmox` fixture (the solo-web example): Terraform, ready files and checks over SSH.
pub(crate) const SOLO_WEB_PROXMOX: &str = include_str!("../tests/fixtures/solo-web.proxmox.run.json");

/// A cloud-vm manifest on AWS (the solo-web example), shaped as Isoloom writes them: its region
/// and profile as params, its credentials as connection slots, an expiry, a flag delivered to
/// the guest over SSH, its machine's address an output.
pub(crate) const SOLO_WEB_AWS: &str = include_str!("../tests/fixtures/solo-web.cloud-vm-aws.run.json");

/// The `flag-vm` vagrant manifest's text (Isoloom's Linux vagrant fixture): a Debian web VM
/// with its flag as a file, its page forwarded to the host, checked from the controller.
pub(crate) const FLAG_VM: &str = include_str!("../tests/fixtures/flag-vm.run.json");

/// Isoloom's `docker-vm` fixture (flag-portal): Compose inside a VM Vagrant makes.
pub(crate) const DOCKER_VM: &str = include_str!("../tests/fixtures/flag-portal.docker-vm.run.json");

/// Isoloom's `docker-vm` fixture on Proxmox: Compose inside a VM Terraform makes, over SSH.
pub(crate) const DOCKER_VM_PROXMOX: &str = include_str!("../tests/fixtures/flag-portal.docker-vm-proxmox.run.json");

/// When the tests' clock stands: 2026-10-15T12:00:00Z.
pub(crate) const TEST_TIME: u64 = 1_792_065_600;

/// The solo-web `proxmox` manifest.
pub(crate) fn proxmox_manifest() -> Manifest {
    isoloom_manifest::parse(SOLO_WEB_PROXMOX).unwrap()
}

/// The solo-web `cloud-vm` manifest on AWS.
pub(crate) fn solo_web_aws_manifest() -> Manifest {
    isoloom_manifest::parse(SOLO_WEB_AWS).unwrap()
}

/// The `flag-portal` docker manifest.
pub(crate) fn docker_manifest() -> Manifest {
    isoloom_manifest::parse(FLAG_PORTAL).unwrap()
}

/// The `flag-vm` vagrant manifest.
pub(crate) fn vagrant_manifest() -> Manifest {
    isoloom_manifest::parse(FLAG_VM).unwrap()
}

/// A run generated for one target, and a runner home beside it; both go when it is dropped.
pub(crate) struct LabFolder {
    /// The runner's home.
    pub(crate) home: tempfile::TempDir,
    /// The run's folder, kept while the test runs.
    _lab: tempfile::TempDir,
    /// Its run manifest, `.isoloom/<target>/run.json`.
    pub(crate) manifest: PathBuf,
}

impl LabFolder {
    /// `manifest` written for `target`.
    pub(crate) fn new(target: &str, manifest: &str) -> LabFolder {
        let (home, run) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let dir = run.path().join(".isoloom").join(target);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("run.json"), manifest).unwrap();
        LabFolder {
            manifest: dir.join("run.json"),
            home,
            _lab: run,
        }
    }

    /// The folder the manifest is in (the target's generated files).
    pub(crate) fn target_dir(&self) -> &Path {
        self.manifest.parent().unwrap()
    }
}

/// A test's model of an engine (a Compose project, a Vagrantfile's VMs), and fake processes that
/// `answer` from it: the test reads and changes the model, the runner's calls see it.
pub(crate) fn modelled<E: Default + Send + 'static>(answer: fn(&Mutex<E>, &Call) -> Reply) -> (Arc<Mutex<E>>, Arc<FakeProcesses>) {
    let engine = Arc::new(Mutex::new(E::default()));
    let fake = Arc::new(FakeProcesses::new({
        let engine = Arc::clone(&engine);
        move |call| answer(&engine, call)
    }));
    (engine, fake)
}

/// `fake`, as the port the runner starts tools through.
pub(crate) fn processes(fake: &Arc<FakeProcesses>) -> Arc<dyn ProcessRunner> {
    Arc::<FakeProcesses>::clone(fake)
}

/// A runner at `home` on `host`, its tools `processes`, its ports `ports`, its clock at
/// [`TEST_TIME`].
pub(crate) fn runner_config(home: &Path, processes: Arc<dyn ProcessRunner>, ports: Arc<dyn HostPorts>, host: HostOs) -> RunnerConfig {
    RunnerConfig {
        home: home.to_path_buf(),
        processes,
        ports,
        clock: Arc::new(FixedClock(UNIX_EPOCH + Duration::from_secs(TEST_TIME))),
        host,
        path: None,
    }
}

/// What a fake `ssh-keygen` does: writes the runner's key pair under `home`.
pub(crate) fn keygen(home: &Path) -> Reply {
    let key = crate::ssh::key_path(home);
    std::fs::write(&key, "private").unwrap();
    std::fs::write(key.with_extension("pub"), "ssh-ed25519 AAAAfake isothrum\n").unwrap();
    Reply::ok("")
}

/// No host port to give.
pub(crate) fn no_ports() -> Arc<dyn HostPorts> {
    Arc::new(FakePorts::new([]))
}

/// The options a VM run's tests start with: VirtualBox, `flag` given as the main flag.
pub(crate) fn virtualbox_options(flag: &str) -> crate::RunOptions {
    let mut secrets = crate::secrets::Secrets::new();
    secrets.insert("CTF_FLAG_MAIN", crate::secrets::SecretValue::new(flag));
    crate::RunOptions::new().with_provider("virtualbox").with_secrets(secrets)
}

/// A running `run` whose lab declares no access machine has none to attach: an attach, a detach
/// and the shell into one are each refused (`unsupported`, saying why), and nothing is run.
pub(crate) async fn refuses_a_box_it_has_none_of(run: &crate::Running<'_>, fake: &FakeProcesses) {
    let calls = fake.calls().len();
    let err = run.attach(None, None, Some("me/kali:2026"), &crate::Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::Unsupported);
    assert!(err.to_string().contains("this lab declares no access machine"), "{err}");
    assert_eq!(
        run.detach(None, &crate::Recorder::new()).await.unwrap_err().code(),
        crate::ErrorCode::Unsupported
    );
    assert_eq!(run.shell_command(None).unwrap_err().code(), crate::ErrorCode::Unsupported);
    assert_eq!(fake.calls().len(), calls, "nothing ran: {:?}", &fake.lines()[calls..]);
}
