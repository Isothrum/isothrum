//! What the crate's tests share: the run manifests Isoloom writes, a run folder holding one, and
//! a runner configuration on fakes with a fixed clock.
//!
//! The manifests in `tests/fixtures/` are `isoloom generate --target <target>` output, copied
//! as written, for specs Isoloom's own tests use: web-portal is the run-manifest design's spec
//! (`isoloom-targets`' `tests/targets/run_manifest.rs`), web-vm the Linux Vagrant one
//! (`vagrant_manifest.rs`), solo-web Isoloom's example. An `-access` variant adds
//! `user: { access: true, os: debian@12, networks: { lab: 99 } }` (Isoloom refuses an access
//! machine with no `os` on every target but `external`, unless its image table gives one); the
//! AWS one gives solo-web `inputs: [DEMO_SECRET]` and `web`
//! the file `/srv/secret.txt` from it. To take them again, generate each from Isoloom's checkout
//! and copy `.isoloom/<target>/run.json` over its fixture; the docker, docker-vm and proxmox ones
//! are then the same as `isoloom-manifest`'s fixtures.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use isoloom_manifest::{HostOs, Manifest};

use crate::clock::FixedClock;
use crate::net::FakePorts;
use crate::process::fake::{Call, FakeProcesses, Reply};
use crate::{HostPorts, ProcessRunner, RunnerConfig};

/// The `web-portal` docker manifest's text.
pub(crate) const WEB_PORTAL: &str = include_str!("../tests/fixtures/web-portal.run.json");

/// `web-portal` with an on-demand access machine (a Compose `user` service in the `access`
/// profile, and Isoloom's stand-in for it while the checks run): the hot-add op's docker fixture.
pub(crate) const WEB_PORTAL_ACCESS: &str = include_str!("../tests/fixtures/web-portal-access.run.json");

/// `web-vm` with an on-demand access VM (a Vagrant `user` define): the hot-add op's VM fixture.
pub(crate) const WEB_VM_ACCESS: &str = include_str!("../tests/fixtures/web-vm-access.run.json");

/// Isoloom's `proxmox` fixture (the solo-web example): Terraform, ready files and checks over SSH.
pub(crate) const SOLO_WEB_PROXMOX: &str = include_str!("../tests/fixtures/solo-web.proxmox.run.json");

/// The solo-web example's cloud-vm manifest on AWS, with a secret input: its region and profile
/// as params, its credentials as connection slots, an expiry, the project copied and the secret
/// delivered into the guest over SSH, its machine's address and login outputs.
pub(crate) const SOLO_WEB_AWS: &str = include_str!("../tests/fixtures/solo-web.cloud-vm-aws.run.json");

/// The `web-vm` vagrant manifest's text (Isoloom's Linux vagrant fixture): a Debian web VM
/// with its secret as a file, its page forwarded to a host port the runner picks, checked from
/// the controller.
pub(crate) const WEB_VM: &str = include_str!("../tests/fixtures/web-vm.run.json");

/// Isoloom's `docker-vm` fixture (web-portal): Compose inside a VM Vagrant makes.
pub(crate) const DOCKER_VM: &str = include_str!("../tests/fixtures/web-portal.docker-vm.run.json");

/// Isoloom's `docker-vm` fixture on Proxmox: Compose inside a VM Terraform makes, over SSH.
pub(crate) const DOCKER_VM_PROXMOX: &str = include_str!("../tests/fixtures/web-portal.docker-vm-proxmox.run.json");

/// The Terraform state a Proxmox host's reset left of a cut-short apply: its IP set, gone from the
/// server, as a real run kept it.
pub(crate) const IPSET_LEFT: &str = r#"{"version":4,"terraform_version":"1.16.7","serial":24,"outputs":{},"resources":[{"mode":"managed","type":"proxmox_virtual_environment_firewall_ipset","name":"test","provider":"provider[\"registry.terraform.io/bpg/proxmox\"]","instances":[{"schema_version":0,"attributes":{"cidr":[{"comment":"","name":"10.254.250.0/24","nomatch":false}],"comment":"network test","container_id":null,"id":"iso1n0","name":"iso1n0","node_name":null,"vm_id":null},"sensitive_attributes":[]}]}]}"#;

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

/// The `web-portal` docker manifest.
pub(crate) fn docker_manifest() -> Manifest {
    isoloom_manifest::parse(WEB_PORTAL).unwrap()
}

/// `web-portal` with an output whose format can't be filled: it names a sensitive output (the
/// published port again, `token`), which is never bound for a template.
pub(crate) fn unfillable_format() -> String {
    let mut m: serde_json::Value = serde_json::from_str(WEB_PORTAL).unwrap();
    let mut token = m["outputs"][0].clone();
    token["name"] = "token".into();
    token["sensitive"] = true.into();
    token.as_object_mut().unwrap().remove("format");
    m["outputs"][0]["format"] = "http://127.0.0.1:${value}/?token=${outputs.token}".into();
    m["outputs"].as_array_mut().unwrap().push(token);
    m.to_string()
}

/// The `web-vm` vagrant manifest.
pub(crate) fn vagrant_manifest() -> Manifest {
    isoloom_manifest::parse(WEB_VM).unwrap()
}

/// `manifest` (JSON) with machine `machine`'s `native.boot_digest` set to `digest`, or taken out
/// (a manifest written before Isoloom wrote one): a regenerated Vagrantfile whose VM boots
/// otherwise.
pub(crate) fn with_boot_digest(manifest: &str, machine: &str, digest: Option<&str>) -> String {
    let mut m: serde_json::Value = serde_json::from_str(manifest).unwrap();
    let native = m["machines"].as_array_mut().unwrap().iter_mut().find(|x| x["name"] == machine).unwrap()["native"]
        .as_object_mut()
        .unwrap();
    match digest {
        Some(d) => native.insert("boot_digest".into(), d.into()),
        None => native.remove("boot_digest"),
    };
    m.to_string()
}

/// A run generated for one target, and a runner home beside it; both go when it is dropped.
pub(crate) struct RunFolder {
    /// The runner's home.
    pub(crate) home: tempfile::TempDir,
    /// The run's folder, kept while the test runs.
    _run: tempfile::TempDir,
    /// Its run manifest, `.isoloom/<target>/run.json`.
    pub(crate) manifest: PathBuf,
}

impl RunFolder {
    /// `manifest` written for `target`.
    pub(crate) fn new(target: &str, manifest: &str) -> RunFolder {
        let (home, run) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let dir = run.path().join(".isoloom").join(target);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("run.json"), manifest).unwrap();
        RunFolder {
            manifest: dir.join("run.json"),
            home,
            _run: run,
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
        tf_plugin_cache: None,
        host_env: crate::host_env::HostEnv::default(),
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

/// The options a VM run's tests start with: VirtualBox, `secret` given as the main secret.
pub(crate) fn virtualbox_options(secret: &str) -> crate::RunOptions {
    let mut secrets = crate::secrets::Secrets::new();
    secrets.insert("DEMO_SECRET", crate::secrets::SecretValue::new(secret));
    crate::RunOptions::new()
        .with_provider(isoloom_manifest::Provider::Virtualbox)
        .with_secrets(secrets)
}

/// `run`'s access machine attached, as [`crate::Running::attach`] picks it, with no options.
pub(crate) async fn attach(
    run: &crate::Running<'_>,
    machine: Option<&str>,
    network: Option<&str>,
    image: Option<&str>,
    events: &dyn crate::Events,
) -> crate::Result<crate::AttachReport> {
    let pick = crate::AccessPick { machine, network, image };
    run.attach(pick, &crate::RunOptions::new(), events).await
}

/// `run`'s access machine `machine` detached, with no options.
pub(crate) async fn detach(run: &crate::Running<'_>, machine: Option<&str>, events: &dyn crate::Events) -> crate::Result<()> {
    run.detach(machine, &crate::RunOptions::new(), events).await
}

/// A running `run` that declares no access machine has none to attach: an attach and a
/// detach are each refused (`unsupported`, saying why), and nothing is run.
pub(crate) async fn refuses_a_box_it_has_none_of(run: &crate::Running<'_>, fake: &FakeProcesses) {
    let calls = fake.calls().len();
    let err = attach(run, None, None, Some("me/toolbox:2026"), &crate::Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::Unsupported);
    assert!(err.to_string().contains("this run declares no access machine"), "{err}");
    assert_eq!(
        detach(run, None, &crate::Recorder::new()).await.unwrap_err().code(),
        crate::ErrorCode::Unsupported
    );
    assert_eq!(fake.calls().len(), calls, "nothing ran: {:?}", &fake.lines()[calls..]);
}

/// What `f` traces while it runs (every level, as text), with what it gives. One global test
/// subscriber writes each event to the buffer of the thread that is capturing, if any: a
/// subscriber set per test (`set_default`) races `tracing`'s global interest cache with the
/// tests running beside it, and a test then sometimes captured nothing at all. `f` runs on this
/// thread (a test's own current-thread runtime), so what it traces lands here.
pub(crate) async fn traced<T>(f: impl Future<Output = T>) -> (T, String) {
    use std::cell::RefCell;
    use std::sync::OnceLock;

    thread_local! {
        static CAPTURE: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
    }
    /// Writes to this thread's capture, or nowhere.
    struct ThisThread;
    impl std::io::Write for ThisThread {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            CAPTURE.with(|c| {
                if let Some(buf) = c.borrow_mut().as_mut() {
                    buf.extend_from_slice(bytes);
                }
            });
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(|| ThisThread)
            .finish();
        // Another test binary's subscriber can't be here: this is the only global one in tests.
        tracing::subscriber::set_global_default(subscriber).expect("the test subscriber is set once");
    });
    CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
    let given = f.await;
    let bytes = CAPTURE.with(|c| c.borrow_mut().take()).unwrap_or_default();
    (given, String::from_utf8_lossy(&bytes).into_owned())
}

/// Every variant of the unit enum `T`, read back from the names its JSON schema lists (serde's
/// own): a list nobody keeps, so a variant added is in it.
pub(crate) fn every_variant<T: schemars::JsonSchema + serde::de::DeserializeOwned>() -> Vec<T> {
    crate::every_name::<T>().iter().map(|n| crate::from_wire_name(n).unwrap()).collect()
}

/// Every variant of `T` is written as `name` writes it (as serde does), reads back as itself, and
/// no two share a name.
pub(crate) fn assert_named_as_serde<T>(name: fn(T) -> &'static str)
where
    T: Copy + PartialEq + std::fmt::Debug + serde::Serialize + serde::de::DeserializeOwned + schemars::JsonSchema,
{
    let all: Vec<T> = every_variant();
    assert!(!all.is_empty(), "no variant found in the schema");
    for &value in &all {
        let json = serde_json::to_value(value).unwrap();
        assert_eq!(json, serde_json::json!(name(value)), "the name disagrees with serde for {value:?}");
        let back: T = serde_json::from_value(json).unwrap();
        assert_eq!(back, value, "{value:?} didn't read back as itself");
    }
    let mut names: Vec<&str> = all.iter().map(|&v| name(v)).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), all.len(), "two variants share a name");
}

#[test]
fn every_variant_is_read_from_the_schema_in_order() {
    use crate::{ErrorCode, Park};
    assert_eq!(every_variant::<Park>(), [Park::Pause, Park::Shutdown]);
    let codes = every_variant::<ErrorCode>();
    assert_eq!(codes.first(), Some(&ErrorCode::ManifestInvalid));
    assert!(codes.contains(&ErrorCode::NotSealed), "the last variant too: {codes:?}");
}

#[tokio::test]
async fn a_capture_holds_its_own_threads_events_and_none_of_another_threads() {
    // A callsite first hit on another thread, with nothing capturing there, still traces here.
    let elsewhere = || std::thread::spawn(|| tracing::warn!(what = "elsewhere", "a trace")).join().unwrap();
    elsewhere();
    let ((), here) = traced(async {
        tracing::warn!(what = "here", "a trace");
        elsewhere();
    })
    .await;
    assert!(here.contains("what=\"here\"") || here.contains("what=here"), "{here}");
    assert!(!here.contains("elsewhere"), "another thread's trace isn't this capture's: {here}");
}
