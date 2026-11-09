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

/// The `web-portal` docker manifest's text.
pub(crate) const WEB_PORTAL: &str = include_str!("../tests/fixtures/web-portal.run.json");

/// `web-portal` with an on-demand access machine (a Compose `attacker` service in the `access`
/// profile): the hot-add op's docker fixture.
pub(crate) const WEB_PORTAL_ACCESS: &str = include_str!("../tests/fixtures/web-portal-access.run.json");

/// `web-vm` with an on-demand access VM (a Vagrant `attacker` define): the hot-add op's VM fixture.
pub(crate) const WEB_VM_ACCESS: &str = include_str!("../tests/fixtures/web-vm-access.run.json");

/// Isoloom's `proxmox` fixture (the solo-web example): Terraform, ready files and checks over SSH.
pub(crate) const SOLO_WEB_PROXMOX: &str = include_str!("../tests/fixtures/solo-web.proxmox.run.json");

/// A cloud-vm manifest on AWS (the solo-web example), shaped as Isoloom writes them: its region
/// and profile as params, its credentials as connection slots, an expiry, a secret delivered to
/// the guest over SSH, its machine's address an output.
pub(crate) const SOLO_WEB_AWS: &str = include_str!("../tests/fixtures/solo-web.cloud-vm-aws.run.json");

/// The `web-vm` vagrant manifest's text (Isoloom's Linux vagrant fixture): a Debian web VM
/// with its secret as a file, its page forwarded to the host, checked from the controller.
pub(crate) const WEB_VM: &str = include_str!("../tests/fixtures/web-vm.run.json");

/// Isoloom's `docker-vm` fixture (web-portal): Compose inside a VM Vagrant makes.
pub(crate) const DOCKER_VM: &str = include_str!("../tests/fixtures/web-portal.docker-vm.run.json");

/// Isoloom's `docker-vm` fixture on Proxmox: Compose inside a VM Terraform makes, over SSH.
pub(crate) const DOCKER_VM_PROXMOX: &str = include_str!("../tests/fixtures/web-portal.docker-vm-proxmox.run.json");

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
    crate::RunOptions::new().with_provider("virtualbox").with_secrets(secrets)
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

/// A running `run` whose lab declares no access machine has none to attach: an attach and a
/// detach are each refused (`unsupported`, saying why), and nothing is run.
pub(crate) async fn refuses_a_box_it_has_none_of(run: &crate::Running<'_>, fake: &FakeProcesses) {
    let calls = fake.calls().len();
    let err = attach(run, None, None, Some("me/kali:2026"), &crate::Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::Unsupported);
    assert!(err.to_string().contains("this run declares no access machine"), "{err}");
    assert_eq!(
        detach(run, None, &crate::Recorder::new()).await.unwrap_err().code(),
        crate::ErrorCode::Unsupported
    );
    assert_eq!(fake.calls().len(), calls, "nothing ran: {:?}", &fake.lines()[calls..]);
}

/// What `f` traces while it runs (every level, as text), with what it gives.
pub(crate) async fn traced<T>(f: impl Future<Output = T>) -> (T, String) {
    #[derive(Clone, Default)]
    struct Lines(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Lines {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let lines = Lines::default();
    let writer = lines.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    let given = {
        let _traced = tracing::subscriber::set_default(subscriber);
        f.await
    };
    let text = String::from_utf8_lossy(&lines.0.lock().unwrap()).into_owned();
    (given, text)
}

/// Every variant of the unit enum `T`, read back from the names its JSON schema lists (serde's
/// own): a list nobody keeps, so a variant added is in it.
fn every_variant<T: schemars::JsonSchema + serde::de::DeserializeOwned>() -> Vec<T> {
    fn names(schema: &serde_json::Value, out: &mut Vec<String>) {
        match schema {
            serde_json::Value::Object(fields) => {
                if let Some(serde_json::Value::String(name)) = fields.get("const") {
                    out.push(name.clone());
                }
                if let Some(serde_json::Value::Array(listed)) = fields.get("enum") {
                    out.extend(listed.iter().filter_map(|v| v.as_str().map(str::to_owned)));
                }
                fields.values().for_each(|v| names(v, out));
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| names(v, out)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    names(&serde_json::to_value(schemars::schema_for!(T)).unwrap(), &mut found);
    found.iter().map(|n| serde_json::from_value(serde_json::json!(n)).unwrap()).collect()
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
