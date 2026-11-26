mod booting;
mod commands;
mod contract;
mod needs;
mod observe;
mod passing;
mod resources;
mod steps;
mod vmnet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use super::*;
use crate::RunName;
use crate::process::fake::{CallView as _, FakeProcesses, Reply};
use crate::project::platform::Protocol;
use crate::project::{Format, Project as Folder};

const VAGRANTFILE: &str = "Vagrant.configure('2') do |c|\n  c.vm.define ENV.fetch('PROJECT_KEY', 'x') + '-web'\nend\n";

/// The warnings a test's driver gave.
type Warned = Arc<Mutex<Vec<(WarningCode, String)>>>;

/// Where a test's driver gives its warnings.
type Warn = Box<dyn Fn(WarningCode, &str) + Send + Sync>;

/// A Vagrant project and a state folder, each in a temporary folder.
struct Fixture {
    _dir: tempfile::TempDir,
    state: tempfile::TempDir,
    project: Folder,
    run: RunName,
    warned: Warned,
    warn: Warn,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().expect("a folder");
        std::fs::write(dir.path().join("Vagrantfile"), VAGRANTFILE).expect("a Vagrantfile");
        let project = Folder::open(dir.path(), Some(Format::Vagrant)).expect("a project");
        let warned: Warned = Arc::default();
        let into = Arc::clone(&warned);
        Fixture {
            _dir: dir,
            state: tempfile::tempdir().expect("a state folder"),
            project,
            run: RunName::new("stack").expect("a name"),
            warned,
            warn: Box::new(move |code, said| into.lock().unwrap().push((code, said.to_owned()))),
        }
    }

    /// The run's dotfile folder.
    fn dot(&self) -> Dotfile {
        Dotfile::in_state(self.state.path())
    }

    /// Records `machine` as made on `provider` with VM id `id`, as Vagrant does.
    fn made(&self, machine: &str, provider: &str, id: &str) {
        self.dot()
            .write_back(&Record {
                machine: machine.into(),
                provider: provider.into(),
                id: id.into(),
            })
            .expect("a record");
    }

    fn warnings(&self) -> Vec<WarningCode> {
        self.warned.lock().unwrap().iter().map(|(c, _)| *c).collect()
    }
}

fn quiet(_: crate::events::Stream, _: &str) {}

/// A context for run `stack` of the fixture's project on `fake`, on `provider`.
fn cx<'a>(f: &'a Fixture, fake: &'a FakeProcesses, provider: Option<Provider>) -> ToolCx<'a> {
    ToolCx {
        project: &f.project,
        run: &f.run,
        key: "k3y9ab",
        state_dir: f.state.path(),
        provider,
        env: vec![
            ("ISOTHRUM_RUN_KEY".to_owned(), "k3y9ab".to_owned()),
            ("PROJECT_KEY".to_owned(), "k3y9ab".to_owned()),
        ],
        host_env: &crate::host_env::EMPTY,
        processes: fake,
        ssh_key: std::path::Path::new("/nowhere/id_ed25519"),
        ports: &crate::formats::NoPorts,
        sink: &quiet,
        warn: &*f.warn,
        steps: crate::steps::Steps::silent(),
        made: crate::formats::NOTHING_MADE,
        fetched: None,
    }
}

/// Each call's program and verb (`vagrant up`, `VBoxManage discardstate`), in the order made.
fn verbs(fake: &FakeProcesses) -> Vec<String> {
    fake.calls()
        .iter()
        .map(|c| format!("{} {}", c.program, c.args.first().map_or("", String::as_str)))
        .collect()
}

const LIBVIRT: Option<Provider> = Some(Provider::Libvirt);

/// Vagrant whose `up` fails with `fixture` its first `failing` times, then succeeds: an answer
/// that changes over time, which a script doesn't give.
fn failing_first(failing: usize, fixture_name: &str) -> FakeProcesses {
    let said = fixture(fixture_name);
    let tries = std::sync::atomic::AtomicUsize::new(0);
    FakeProcesses::model(move |_| {
        if tries.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < failing {
            Reply::fail(1, &said)
        } else {
            Reply::ok("")
        }
    })
}
const VBOX: Option<Provider> = Some(Provider::Virtualbox);

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tool_output/fixtures").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[tokio::test]
async fn up_runs_on_the_provider_asked_with_the_dotfile_in_the_state_folder_and_the_key_in_the_environment() {
    let f = Fixture::new();
    let fake = FakeProcesses::script().expect_times("vagrant up --provider libvirt", 1, Reply::ok("")).build();
    Vagrant.up(&cx(&f, &fake, LIBVIRT)).await.expect("up");
    let [up] = fake.made("vagrant up").try_into().expect("one up");
    assert_eq!(up.args, ["up", "--provider", "libvirt"]);
    assert_eq!(up.cwd.as_deref(), Some(f.project.dir.as_path()));
    assert_eq!(up.env(dotfile::VAR), Some(f.dot().path.display().to_string().as_str()));
    assert_eq!(up.env("PROJECT_KEY"), Some("k3y9ab"), "the key reaches the Vagrantfile's ENV.fetch");
    let file = std::fs::read_to_string(f.project.dir.join("Vagrantfile")).unwrap();
    assert_eq!(file, VAGRANTFILE, "the Vagrantfile is never rewritten");
}

#[tokio::test]
async fn a_run_with_no_provider_is_refused_before_any_call() {
    let f = Fixture::new();
    let fake = FakeProcesses::script().build();
    let e = Vagrant.up(&cx(&f, &fake, None)).await.expect_err("no provider");
    assert!(matches!(e, Error::NoProvider { .. }), "{e}");
}

#[tokio::test]
async fn a_record_on_another_provider_is_destroyed_and_reset_before_up() {
    let f = Fixture::new();
    f.made("web", "virtualbox", "uuid-web");
    let fake = FakeProcesses::script()
        .expect_times("vagrant destroy -f", 1, Reply::ok(""))
        .expect_times("vagrant up --provider libvirt", 1, Reply::ok(""))
        .build();
    Vagrant.up(&cx(&f, &fake, LIBVIRT)).await.expect("up");
    assert_eq!(verbs(&fake), ["vagrant destroy", "vagrant up"], "cleared first");
    assert_eq!(f.dot().recorded().unwrap(), Vec::<Record>::new(), "the other provider's record is dropped");
    assert_eq!(f.warnings(), [WarningCode::OtherProvider]);
}

#[tokio::test]
async fn a_failed_clearing_destroy_on_another_provider_is_a_warning_and_the_start_goes_on() {
    let f = Fixture::new();
    f.made("web", "virtualbox", "uuid-web");
    let fake = FakeProcesses::script()
        .expect_times("vagrant destroy -f", 1, Reply::fail(1, "VBoxManage not found"))
        .expect_times("vagrant up --provider libvirt", 1, Reply::ok(""))
        .build();
    Vagrant.up(&cx(&f, &fake, LIBVIRT)).await.expect("up");
    assert_eq!(f.warnings(), [WarningCode::OtherProvider, WarningCode::CleanupFailed]);
    assert!(!f.dot().path.exists());
}

#[tokio::test]
async fn a_cut_off_saved_state_is_discarded_before_virtualbox_boots_the_vm() {
    let f = Fixture::new();
    f.made("web", "virtualbox", "uuid-web");
    f.made("db", "virtualbox", "uuid-db");
    let fake = FakeProcesses::script()
        .expect(
            "VBoxManage showvminfo uuid-web --machinereadable",
            Reply::ok(&fixture("vboxmanage/7.0-showvminfo-aborted-saved.txt")),
        )
        .expect(
            "VBoxManage showvminfo uuid-db --machinereadable",
            Reply::fail(1, &fixture("vboxmanage/7.0-showvminfo-not-found.txt")),
        )
        .expect_times("VBoxManage discardstate uuid-web", 1, Reply::ok(""))
        .expect_times("vagrant up --provider virtualbox", 1, Reply::ok(""))
        .build();
    Vagrant.up(&cx(&f, &fake, VBOX)).await.expect("up");
    assert_eq!(verbs(&fake).last().map(String::as_str), Some("vagrant up"), "discarded before the boot");
    assert_eq!(f.warnings(), [WarningCode::SavedStateDiscarded]);
}

#[tokio::test]
async fn an_up_refused_for_a_vm_name_taken_clears_what_the_run_made_and_is_tried_once_more_only() {
    let f = Fixture::new();
    f.made("web", "virtualbox", "uuid-web");
    let machine_folder = tempfile::tempdir().unwrap();
    std::fs::create_dir(machine_folder.path().join("stack-k3y9ab-web")).unwrap();
    std::fs::create_dir(machine_folder.path().join("stack-zz0000-web")).unwrap();
    let taken = Reply::fail(1, &fixture("vagrant/2.4.1-virtualbox-7.0-already-exists.txt"));
    let vms = "\"stack-k3y9ab-db\" {uuid-db}\n\"stack-zz0000-db\" {uuid-other}\n";
    let fake = FakeProcesses::script()
        .expect("VBoxManage showvminfo uuid-web --machinereadable", Reply::ok("VMState=\"poweroff\""))
        .expect_times("vagrant up --provider virtualbox", 2, taken)
        .expect_times("vagrant destroy -f", 1, Reply::ok(""))
        .expect("VBoxManage controlvm", Reply::ok(""))
        .expect("VBoxManage discardstate uuid-db", Reply::fail(1, "not saved"))
        .expect_times("VBoxManage unregistervm uuid-web --delete", 1, Reply::ok(""))
        .expect_times("VBoxManage unregistervm uuid-db --delete", 1, Reply::ok(""))
        .expect("VBoxManage list vms", Reply::ok(vms))
        .expect(
            "VBoxManage list systemproperties",
            Reply::ok(&format!("Default machine folder:          {}\n", machine_folder.path().display())),
        )
        .build();
    // The recovery's one try, never a loop: `expect_times(.. up .., 2`.
    let e = Vagrant.up(&cx(&f, &fake, VBOX)).await.expect_err("taken twice");
    assert_eq!(e.code(), crate::ErrorCode::ToolFailed, "{e}");
    isothrum_test_support::assert_every!(fake.calls(), |c: &crate::process::fake::Call| !c.has("uuid-other"));
    assert!(!machine_folder.path().join("stack-k3y9ab-web").exists());
    assert!(machine_folder.path().join("stack-zz0000-web").exists());
    assert_eq!(f.dot().recorded().unwrap(), Vec::<Record>::new());
}

#[tokio::test(start_paused = true)]
async fn an_up_another_process_lock_refused_runs_again_and_says_so() {
    let f2 = Fixture::new();
    // Vagrant whose lock another process holds at the first call only: an answer that changes
    // over time, which a script doesn't give.
    let fake = FakeProcesses::model({
        let tries = std::sync::atomic::AtomicUsize::new(0);
        move |_| {
            if tries.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                Reply {
                    code: 1,
                    ..Reply::ok(&fixture("vagrant/2.4.9-error-machine-locked.txt"))
                }
            } else {
                Reply::ok("")
            }
        }
    });
    Vagrant.up(&cx(&f2, &fake, LIBVIRT)).await.expect("through once the lock is released");
    assert_eq!(verbs(&fake), ["vagrant up", "vagrant up"]);
    assert_eq!(f2.warnings(), [WarningCode::Retrying]);
}

#[tokio::test]
async fn park_suspends_or_halts_and_esxi_refusing_a_vm_already_off_is_halted() {
    let f = Fixture::new();
    let fake = FakeProcesses::script()
        .expect_times("vagrant suspend", 1, Reply::ok(""))
        .expect_times("vagrant halt", 1, Reply::fail(1, &fixture("vagrant/vmware-esxi-2.5.5-halt-already-off.txt")))
        .build();
    Vagrant.park(&cx(&f, &fake, LIBVIRT), Park::Pause).await.expect("suspend");
    Vagrant.park(&cx(&f, &fake, LIBVIRT), Park::Shutdown).await.expect("already off is halted");
}

#[tokio::test]
async fn resume_boots_without_provisioning_and_update_and_provision_provision() {
    let f = Fixture::new();
    let fake = FakeProcesses::script()
        .expect_times("vagrant up --provider libvirt --no-provision", 1, Reply::ok(""))
        .expect_times("vagrant up --provider libvirt --provision", 3, Reply::ok(""))
        .build();
    let cx = cx(&f, &fake, LIBVIRT);
    Vagrant.resume(&cx).await.expect("resume");
    Vagrant.update(&cx).await.expect("update");
    Vagrant.provision(&cx, None, None).await.expect("provision all");
    Vagrant.provision(&cx, Some("web"), None).await.expect("provision one");
    let provisions = fake.made("vagrant up --provider libvirt --provision");
    let machines: Vec<&[String]> = provisions.iter().map(|c| &c.args[4..]).collect();
    assert_eq!(machines, [&[][..], &[][..], &["web".to_owned()][..]], "one machine named only when asked");
}

#[tokio::test]
async fn attach_boots_one_machine_and_retire_destroys_it() {
    let f = Fixture::new();
    let fake = FakeProcesses::script()
        .expect_times("vagrant up --provider libvirt attack", 1, Reply::ok(""))
        .expect_times("vagrant destroy -f attack", 1, Reply::ok(""))
        .build();
    let cx = cx(&f, &fake, LIBVIRT);
    Vagrant.attach(&cx, "attack").await.expect("attach");
    Vagrant.retire(&cx, "attack").await.expect("retire");
}

#[tokio::test]
async fn package_saves_the_machine_as_a_box_file() {
    let f = Fixture::new();
    let output = f.state.path().join("base.box");
    let call = format!("vagrant package base --output {}", output.display());
    let fake = FakeProcesses::script().expect_times(&call, 1, Reply::ok("")).build();
    Vagrant.package(&cx(&f, &fake, LIBVIRT), "base", &output).await.expect("package");
}

#[tokio::test]
async fn down_destroys_then_resets_the_dotfile_and_a_failed_destroy_keeps_it() {
    let f = Fixture::new();
    f.made("web", "libvirt", "uuid-web");
    let fake = FakeProcesses::script()
        .expect_times("vagrant destroy -f", 1, Reply::fail(1, "libvirt is down"))
        .build();
    let e = Vagrant.down(&cx(&f, &fake, LIBVIRT)).await.expect_err("a failed destroy");
    assert_eq!(e.code(), crate::ErrorCode::ToolFailed, "{e}");
    assert_eq!(f.dot().recorded().unwrap().len(), 1, "the ids stay for a removal by id");
    let through = FakeProcesses::script().expect_times("vagrant destroy -f", 1, Reply::ok("")).build();
    Vagrant.down(&cx(&f, &through, LIBVIRT)).await.expect("down");
    assert!(!f.dot().path.exists());
}
