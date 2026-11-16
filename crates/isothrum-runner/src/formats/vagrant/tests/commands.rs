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
    assert_eq!(
        call.args,
        ["ssh", "web", "-c", "cat '/etc/a b'", "--", "-o", "BatchMode=yes", "-o", "ConnectTimeout=20"],
        "never a prompt, and a guest that doesn't answer given up on"
    );
}

/// A check whose guest never answers (a VM's SSH that hangs) fails as a timeout at its deadline,
/// with its last line, and never holds the operation.
#[tokio::test(start_paused = true)]
async fn a_check_that_hangs_is_a_timeout_at_its_deadline_with_its_last_line() {
    let f = Fixture::new();
    let hanging = Reply::ok("running check 3: web answers\nnever printed").paced(std::time::Duration::from_secs(200));
    let fake = FakeProcesses::script().expect("vagrant ssh web -c", hanging).build();
    let started = tokio::time::Instant::now();
    let e = Vagrant
        .exec(&cx(&f, &fake, LIBVIRT), "web", &["./checks.sh".into()])
        .await
        .expect_err("past its deadline");
    assert_eq!(e.code(), crate::ErrorCode::Timeout, "{e}");
    assert_eq!(
        e.timed_out(),
        Some(&crate::error::TimedOut::Ceiling {
            after_s: 300,
            last_line: Some("running check 3: web answers".to_owned())
        })
    );
    assert_eq!(
        started.elapsed(),
        std::time::Duration::from_secs(300),
        "at its deadline, not the 400 s it would take"
    );
}

#[tokio::test]
async fn a_console_is_vagrant_ssh_with_the_dotfile_and_a_user_is_checked() {
    let f = Fixture::new();
    let fake = FakeProcesses::script().build();
    let cx = cx(&f, &fake, LIBVIRT);
    let plain = Vagrant.console(&cx, "web", None).await.expect("a console");
    assert!(plain.sh_line().contains("vagrant ssh web"), "{}", plain.sh_line());
    let as_user = Vagrant.console(&cx, "web", Some("alice")).await.expect("a console as alice");
    assert!(as_user.sh_line().contains("sudo -iu alice"), "{}", as_user.sh_line());
    assert_eq!(
        Vagrant.console(&cx, "web", Some("a; rm -rf /")).await.map(drop).map_err(|e| e.code()),
        Err(crate::ErrorCode::Unsupported)
    );
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

/// Regression: the libvirt connection was read from the process's environment mid-operation, a
/// value that isn't UTF-8 as unset. It is the runner's own environment's: vagrant-libvirt's
/// default connection named, unless `LIBVIRT_DEFAULT_URI` names one (`virsh` reads it itself).
#[tokio::test]
async fn a_libvirt_screen_asks_virsh_on_the_connection_the_runners_environment_names() {
    let f = Fixture::new();
    f.made("web", "libvirt", "dom-web");
    let session = crate::host_env::HostEnv::of([(crate::host_env::LIBVIRT_DEFAULT_URI, "qemu:///session")]);
    for (env, call) in [
        (&crate::host_env::EMPTY, "virsh -c qemu:///system domdisplay dom-web"),
        (&session, "virsh domdisplay dom-web"),
    ] {
        let fake = FakeProcesses::script().expect(call, Reply::ok("vnc://127.0.0.1:0\n")).build();
        let console = super::screen::reach(
            &ToolCx {
                host_env: env,
                ..cx(&f, &fake, LIBVIRT)
            },
            "web",
        )
        .await
        .expect(call);
        assert_eq!(
            console.endpoint,
            crate::graphical::Endpoint::Loopback("127.0.0.1:5900".parse().unwrap()),
            "{call}"
        );
    }
}
