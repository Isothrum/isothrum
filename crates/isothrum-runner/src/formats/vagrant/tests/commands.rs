//! A command in a machine, its console, the probe, and the provider's elevation.

use super::*;

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
