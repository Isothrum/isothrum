use std::path::Path;
use std::sync::{Arc, Mutex};

use super::*;
use crate::RunName;
use crate::process::fake::{FakeProcesses, Reply};
use crate::project::{Format, Project as Folder};

const VAGRANTFILE: &str = "Vagrant.configure('2') do |c|\n  c.vm.define ENV.fetch('ISOLOOM_KEY', 'x') + '-web'\nend\n";

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
            run: RunName::new("lab").expect("a name"),
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

/// A context for run `lab` of the fixture's project on `fake`, on `provider`.
fn cx<'a>(f: &'a Fixture, fake: &'a FakeProcesses, provider: Option<Provider>) -> ToolCx<'a> {
    ToolCx {
        project: &f.project,
        run: &f.run,
        key: "k3y9ab",
        state_dir: f.state.path(),
        provider,
        env: vec![
            ("ISOTHRUM_RUN_KEY".to_owned(), "k3y9ab".to_owned()),
            ("ISOLOOM_KEY".to_owned(), "k3y9ab".to_owned()),
        ],
        processes: fake,
        sink: &quiet,
        warn: &*f.warn,
    }
}

/// Every call made, as `program arg…`.
fn made(fake: &FakeProcesses) -> Vec<String> {
    fake.calls()
        .iter()
        .map(|c| std::iter::once(c.program.clone()).chain(c.args.clone()).collect::<Vec<_>>().join(" "))
        .collect()
}

const LIBVIRT: Option<Provider> = Some(Provider::Libvirt);
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
    assert_eq!(made(&fake), ["vagrant up --provider libvirt"]);
    let up = fake.last("up").expect("an up");
    assert_eq!(up.cwd.as_deref(), Some(f.project.dir.as_path()));
    assert_eq!(up.env(dotfile::VAR), Some(f.dot().path.display().to_string().as_str()));
    assert_eq!(up.env("ISOLOOM_KEY"), Some("k3y9ab"), "the key reaches the Vagrantfile's ENV.fetch");
    let file = std::fs::read_to_string(f.project.dir.join("Vagrantfile")).unwrap();
    assert_eq!(file, VAGRANTFILE, "the Vagrantfile is never rewritten");
}

#[tokio::test]
async fn a_run_with_no_provider_is_refused_before_any_call() {
    let f = Fixture::new();
    let fake = FakeProcesses::script().build();
    let e = Vagrant.up(&cx(&f, &fake, None)).await.expect_err("no provider");
    assert!(matches!(e, Error::NoProvider { .. }), "{e}");
    assert_eq!(made(&fake), Vec::<String>::new());
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
    assert_eq!(made(&fake), ["vagrant destroy -f", "vagrant up --provider libvirt"]);
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
    assert_eq!(made(&fake).last().map(String::as_str), Some("vagrant up --provider virtualbox"));
    assert_eq!(f.warnings(), [WarningCode::SavedStateDiscarded]);
}

#[tokio::test]
async fn an_up_over_an_earlier_vm_clears_what_the_run_made_and_starts_once_more() {
    let f = Fixture::new();
    f.made("web", "virtualbox", "uuid-web");
    let machine_folder = tempfile::tempdir().unwrap();
    std::fs::create_dir(machine_folder.path().join("lab-k3y9ab-web")).unwrap();
    std::fs::create_dir(machine_folder.path().join("lab-zz0000-web")).unwrap();
    let taken = Reply::fail(1, &fixture("vagrant/2.4.1-virtualbox-7.0-already-exists.txt"));
    let vms = "\"lab-k3y9ab-db\" {uuid-db}\n\"lab-zz0000-db\" {uuid-other}\n";
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
    let e = Vagrant.up(&cx(&f, &fake, VBOX)).await.expect_err("taken twice");
    assert_eq!(e.code(), crate::ErrorCode::ToolFailed, "{e}");
    let ups = made(&fake).iter().filter(|l| l.starts_with("vagrant up")).count();
    assert_eq!(ups, 2, "the recovery's one try: never a loop");
    assert!(!made(&fake).iter().any(|l| l.contains("uuid-other")), "another copy's VM is never touched");
    assert!(!machine_folder.path().join("lab-k3y9ab-web").exists());
    assert!(machine_folder.path().join("lab-zz0000-web").exists());
    assert_eq!(f.dot().recorded().unwrap(), Vec::<Record>::new());
}

#[tokio::test(start_paused = true)]
async fn an_up_another_process_lock_refused_runs_again_and_says_so() {
    let f2 = Fixture::new();
    let fake = FakeProcesses::new({
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
    assert_eq!(made(&fake), ["vagrant up --provider libvirt", "vagrant up --provider libvirt"]);
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
    assert_eq!(made(&fake), ["vagrant suspend", "vagrant halt"]);
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
    Vagrant.provision(&cx, None).await.expect("provision all");
    Vagrant.provision(&cx, Some("web")).await.expect("provision one");
    assert_eq!(
        made(&fake),
        [
            "vagrant up --provider libvirt --no-provision",
            "vagrant up --provider libvirt --provision",
            "vagrant up --provider libvirt --provision",
            "vagrant up --provider libvirt --provision web",
        ]
    );
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
    assert_eq!(made(&fake), ["vagrant up --provider libvirt attack", "vagrant destroy -f attack"]);
}

#[tokio::test]
async fn down_destroys_then_resets_the_dotfile_and_a_failed_destroy_keeps_it() {
    let f = Fixture::new();
    f.made("web", "libvirt", "uuid-web");
    let fake = FakeProcesses::script()
        .expect_times("vagrant destroy -f", 1, Reply::fail(1, "libvirt is down"))
        .build();
    Vagrant.down(&cx(&f, &fake, LIBVIRT)).await.expect_err("a failed destroy");
    assert_eq!(f.dot().recorded().unwrap().len(), 1, "the ids stay for a removal by id");
    let through = FakeProcesses::script().expect_times("vagrant destroy -f", 1, Reply::ok("")).build();
    Vagrant.down(&cx(&f, &through, LIBVIRT)).await.expect("down");
    assert!(!f.dot().path.exists());
}

#[tokio::test]
async fn observe_reads_each_machine_and_the_ports_of_those_running_without_guessing_a_protocol() {
    let f = Fixture::new();
    let fake = FakeProcesses::script()
        .expect("vagrant status --machine-readable", Reply::ok(&fixture("vagrant/2.4.1-status-virtualbox.txt")))
        .expect_times("vagrant port web --machine-readable", 1, Reply::ok(&fixture("vagrant/2.4.1-port.txt")))
        .build();
    let (seen, state) = Vagrant.observe(&cx(&f, &fake, VBOX)).await.expect("observe");
    let web = seen.machines.iter().find(|m| m.name == "web").expect("web");
    assert_eq!(web.state, MachineState::Running);
    assert_eq!(
        web.published,
        [
            SeenPort {
                guest: crate::port::GuestPort::new(22),
                host: crate::port::HostPort::new(2222),
                protocol: None
            },
            SeenPort {
                guest: crate::port::GuestPort::new(80),
                host: crate::port::HostPort::new(8080),
                protocol: None
            }
        ]
    );
    let db = seen.machines.iter().find(|m| m.name == "db").expect("db");
    assert_eq!((db.state.clone(), db.published.len()), (MachineState::Stopped, 0));
    assert_eq!(state, RunState::Failed, "one made and stopped among running ones");
}

#[tokio::test]
async fn a_provider_that_cant_list_forwards_has_no_port_proven() {
    let f = Fixture::new();
    let fake = FakeProcesses::script()
        .expect("vagrant status --machine-readable", Reply::ok("1,web,state,running\n"))
        .expect(
            "vagrant port web",
            Reply {
                code: 1,
                ..Reply::ok(&fixture("vagrant/2.4.9-libvirt-port-unsupported.txt"))
            },
        )
        .build();
    let (seen, state) = Vagrant.observe(&cx(&f, &fake, LIBVIRT)).await.expect("observe");
    assert_eq!(seen.machines[0].published, Vec::<SeenPort>::new());
    assert_eq!(state, RunState::Ready);
}

#[tokio::test]
async fn nothing_made_is_absent_as_vagrant_prints_it() {
    let f = Fixture::new();
    let fake = FakeProcesses::script()
        .expect(
            "vagrant status --machine-readable",
            Reply::ok(&fixture("vagrant/2.4.9-libvirt-status-not-created.txt")),
        )
        .build();
    let (seen, state) = Vagrant.observe(&cx(&f, &fake, LIBVIRT)).await.expect("observe");
    assert_eq!(seen.machines.len(), 2);
    assert_eq!(state, RunState::Absent);
    let names = Vagrant.machines(&cx(&f, &fake, LIBVIRT)).await.expect("machines");
    assert_eq!(names, ["attack", "web"], "every machine the Vagrantfile defines, made or not");
}

#[test]
fn the_run_state_rule() {
    let m = |name: &str, state: MachineState| SeenMachine {
        name: name.into(),
        state,
        published: Vec::new(),
    };
    assert_eq!(run_state(&[]), RunState::Absent);
    assert_eq!(run_state(&[m("a", MachineState::Absent)]), RunState::Absent);
    assert_eq!(run_state(&[m("a", MachineState::Stopped), m("b", MachineState::Stopped)]), RunState::Parked);
    assert_eq!(
        run_state(&[m("a", MachineState::Running), m("attack", MachineState::Absent)]),
        RunState::Ready,
        "a machine left down at up (autostart: false) isn't a failure"
    );
    assert_eq!(run_state(&[m("a", MachineState::Running), m("b", MachineState::Stopped)]), RunState::Failed);
    assert_eq!(
        run_state(&[m("a", MachineState::Running), m("b", MachineState::Unknown("shut".into()))]),
        RunState::Failed
    );
}

#[tokio::test]
async fn found_is_the_dotfiles_machine_ids_by_machine() {
    let f = Fixture::new();
    f.made("web", "libvirt", "uuid-web");
    f.made("old", "virtualbox", "uuid-old");
    let fake = FakeProcesses::script().build();
    let found = Vagrant.found(&cx(&f, &fake, LIBVIRT)).await.expect("found");
    assert_eq!(
        found,
        [
            Found {
                key: "vm:old@virtualbox".into(),
                kind: ResourceKind::Vm,
                native_id: "uuid-old".into(),
                lifetime: Lifetime::Machines,
            },
            Found {
                key: "vm:web".into(),
                kind: ResourceKind::Vm,
                native_id: "uuid-web".into(),
                lifetime: Lifetime::Machines,
            },
        ]
    );
    assert_eq!(made(&fake), Vec::<String>::new(), "read from the dotfile, no call");
}

#[tokio::test]
async fn destroy_by_id_destroys_a_recorded_machine_through_vagrant() {
    let f = Fixture::new();
    f.made("web", "libvirt", "uuid-web");
    let fake = FakeProcesses::script().expect_times("vagrant destroy -f web", 1, Reply::ok("")).build();
    Vagrant
        .destroy_by_id(&cx(&f, &fake, LIBVIRT), "vm:web", ResourceKind::Vm, "uuid-web")
        .await
        .expect("destroyed");
    assert_eq!(f.dot().recorded().unwrap(), Vec::<Record>::new());
}

#[tokio::test]
async fn destroy_by_id_writes_a_lost_record_back_for_vagrant_to_destroy_by() {
    let f = Fixture::new();
    let fake = FakeProcesses::script().expect_times("vagrant destroy -f web", 1, Reply::ok("")).build();
    let dot = f.dot();
    Vagrant
        .destroy_by_id(&cx(&f, &fake, LIBVIRT), "vm:web", ResourceKind::Vm, "uuid-web")
        .await
        .expect("destroyed");
    let call = fake.last("destroy").unwrap();
    assert_eq!(call.env(dotfile::VAR), Some(dot.path.display().to_string().as_str()));
    assert_eq!(dot.recorded().unwrap(), Vec::<Record>::new(), "the written-back record goes with its VM");
}

#[tokio::test]
async fn destroy_by_id_of_a_machine_no_longer_defined_goes_by_id_at_virtualbox_and_gone_is_fine() {
    let f = Fixture::new();
    f.made("old", "virtualbox", "uuid-old");
    let not_found = Reply {
        code: 1,
        ..Reply::ok(&fixture("vagrant/2.4.9-machine-not-found.txt"))
    };
    let fake = FakeProcesses::script()
        .expect_times("vagrant destroy -f old", 1, not_found)
        .expect("VBoxManage controlvm uuid-old poweroff", Reply::fail(1, "not running"))
        .expect_times(
            "VBoxManage unregistervm uuid-old --delete",
            1,
            Reply::fail(1, &fixture("vboxmanage/7.0-showvminfo-not-found.txt")),
        )
        .build();
    Vagrant
        .destroy_by_id(&cx(&f, &fake, VBOX), "vm:old", ResourceKind::Vm, "uuid-old")
        .await
        .expect("already gone is fine");
    assert_eq!(f.dot().recorded().unwrap(), Vec::<Record>::new());
}

#[tokio::test]
async fn destroy_by_id_refuses_what_it_has_no_way_to_remove_naming_it() {
    let f = Fixture::new();
    f.made("old", "libvirt", "uuid-old");
    let not_found = Reply {
        code: 1,
        ..Reply::ok(&fixture("vagrant/2.4.9-machine-not-found.txt"))
    };
    let fake = FakeProcesses::script().expect("vagrant destroy -f old", not_found).build();
    let e = Vagrant
        .destroy_by_id(&cx(&f, &fake, LIBVIRT), "vm:old", ResourceKind::Vm, "uuid-old")
        .await
        .expect_err("no libvirt adapter");
    assert!(
        matches!(&e, Error::Unsupported { what } if what.contains("libvirt") && what.contains("uuid-old")),
        "{e}"
    );
    let container = Vagrant
        .destroy_by_id(&cx(&f, &fake, LIBVIRT), "container:x", ResourceKind::Container, "abc")
        .await
        .expect_err("no container in a Vagrant run");
    assert!(matches!(container, Error::Unsupported { .. }));
}

#[tokio::test]
async fn reapply_adds_removes_and_provisions_machine_by_machine() {
    let f = Fixture::new();
    f.made("gone", "virtualbox", "uuid-gone");
    let fake = FakeProcesses::script()
        .expect_times("vagrant up --provider virtualbox attack", 1, Reply::ok(""))
        .expect(
            "vagrant status --machine-readable",
            Reply::ok("1,web,state,running\n1,old,state,running\n1,attack,state,running\n"),
        )
        .expect_times("vagrant destroy -f old", 1, Reply::ok(""))
        .expect("VBoxManage controlvm uuid-gone poweroff", Reply::ok(""))
        .expect_times("VBoxManage unregistervm uuid-gone --delete", 1, Reply::ok(""))
        .expect_times("vagrant provision web", 1, Reply::ok(""))
        .build();
    let change = Change {
        added: vec!["attack".into()],
        removed: vec!["old".into(), "gone".into()],
        changed: vec!["web".into()],
    };
    Vagrant.reapply(&cx(&f, &fake, VBOX), &change).await.expect("reapply");
    let calls = made(&fake);
    let at = |l: &str| calls.iter().position(|c| c == l).unwrap_or_else(|| panic!("{l} in {calls:?}"));
    assert!(at("vagrant up --provider virtualbox attack") < at("vagrant destroy -f old"));
    assert!(at("vagrant destroy -f old") < at("vagrant provision web"));
    assert_eq!(
        f.dot().recorded().unwrap(),
        Vec::<Record>::new(),
        "the machine the Vagrantfile dropped went by its id"
    );
}

#[tokio::test]
async fn exec_runs_the_command_over_ssh_quoted_and_a_failing_command_is_its_result() {
    let f = Fixture::new();
    let fake = FakeProcesses::script().expect("vagrant ssh web -c", Reply::fail(3, "no such file")).build();
    let ran = Vagrant
        .exec(&cx(&f, &fake, LIBVIRT), "web", &["cat".into(), "/etc/a b".into()])
        .await
        .expect("ran");
    assert_eq!((ran.exit, ran.stderr.as_str()), (3, "no such file"));
    let call = fake.last("ssh").unwrap();
    assert_eq!(call.args, ["ssh", "web", "-c", "cat '/etc/a b'"]);
}

#[test]
fn a_console_is_vagrant_ssh_with_the_dotfile_and_a_user_is_checked() {
    let f = Fixture::new();
    let fake = FakeProcesses::script().build();
    let cx = cx(&f, &fake, LIBVIRT);
    let plain = Vagrant.console(&cx, "web", None).expect("a console");
    assert!(plain.sh_line().contains("vagrant ssh web"), "{}", plain.sh_line());
    let as_user = Vagrant.console(&cx, "web", Some("alice")).expect("a console as alice");
    assert!(as_user.sh_line().contains("sudo -iu alice"), "{}", as_user.sh_line());
    assert!(Vagrant.console(&cx, "web", Some("a; rm -rf /")).is_err());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn probe_refuses_a_provider_this_host_doesnt_run_a_missing_plugin_and_an_old_vagrant() {
    let f = Fixture::new();
    let none = FakeProcesses::script().build();
    let host = Vagrant.probe(&cx(&f, &none, Some(Provider::Hyperv))).await.expect_err("Hyper-V on Linux");
    assert!(matches!(host, Error::Provider { .. }), "{host}");
    let no_plugin = FakeProcesses::script()
        .expect("vagrant --version", Reply::ok("Vagrant 2.4.9"))
        .expect("vagrant plugin list --machine-readable", Reply::ok("1,,plugin-name,vagrant-qemu\n"))
        .build();
    let plugin = Vagrant.probe(&cx(&f, &no_plugin, LIBVIRT)).await.expect_err("no vagrant-libvirt");
    assert!(plugin.to_string().contains("vagrant plugin install vagrant-libvirt"), "{plugin}");
    let fine = FakeProcesses::script()
        .expect("vagrant --version", Reply::ok("Vagrant 2.4.9"))
        .expect("vagrant plugin list --machine-readable", Reply::ok(&fixture("vagrant/2.4.1-plugin-list.txt")))
        .build();
    Vagrant.probe(&cx(&f, &fine, LIBVIRT)).await.expect("libvirt with its plugin");
    let old = FakeProcesses::script().expect("vagrant --version", Reply::ok("Vagrant 2.3.8")).build();
    let too_old = Vagrant.probe(&cx(&f, &old, VBOX)).await.expect_err("too old");
    assert!(matches!(too_old, Error::ToolTooOld { .. }), "{too_old}");
}

#[test]
fn hyper_v_takes_its_commands_elevated() {
    assert!(Vagrant.elevates(Some(Provider::Hyperv)));
    assert!(!Vagrant.elevates(LIBVIRT) && !Vagrant.elevates(None));
}

#[test]
fn a_vm_is_this_runs_by_its_key_as_a_word() {
    assert!(ours("lab-k3x9a2 - dc01", "k3x9a2") && ours("k3x9a2", "k3x9a2"));
    for theirs in ["lab-zz0000 - dc01", "lab-k3x9a2x - dc01", "labk3x9a2 - dc01"] {
        assert!(!ours(theirs, "k3x9a2"), "{theirs}");
    }
    assert!(!ours("lab", ""), "no key matches nothing");
    assert_eq!(of_key("vm:web", LIBVIRT), Some(("web".into(), Some("libvirt".into()))));
    assert_eq!(of_key("vm:web@virtualbox", LIBVIRT), Some(("web".into(), Some("virtualbox".into()))));
    assert_eq!(of_key("network:x", LIBVIRT), None);
}
