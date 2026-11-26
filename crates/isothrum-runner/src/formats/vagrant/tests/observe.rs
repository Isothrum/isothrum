//! What a Vagrant run reads of its machines: each one's state, its forwarded ports and its address.

use super::*;

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
    assert_eq!(state, RunState::Ready, "one made and stopped among running ones reads stopped, the run ready");
}

/// `vagrant port` doesn't say a forward's protocol: the Vagrantfile's `forwarded_port` line does.
#[tokio::test]
async fn observe_takes_a_forwards_protocol_from_its_vagrantfile_line() {
    let f = Fixture::new();
    let vagrantfile = "Vagrant.configure('2') do |c|\n  c.vm.define 'dns' do |m|\n    m.vm.network 'forwarded_port', guest: 53, host: ENV.fetch('HOST_PORT_DNS_53_UDP', 10053).to_i, protocol: 'udp'\n    m.vm.network 'forwarded_port', guest: 80, host: 8080\n  end\nend\n";
    std::fs::write(f.project.dir.join("Vagrantfile"), vagrantfile).expect("its Vagrantfile");
    let fake = FakeProcesses::script()
        .expect("vagrant status --machine-readable", Reply::ok("1,dns,state,running\n"))
        .expect(
            "vagrant port dns --machine-readable",
            Reply::ok("1,dns,forwarded_port,22,2222\n1,dns,forwarded_port,53,10053\n1,dns,forwarded_port,80,8080\n"),
        )
        .build();
    let (seen, _) = Vagrant.observe(&cx(&f, &fake, VBOX)).await.expect("observe");
    let protocols: Vec<_> = seen.machines[0].published.iter().map(|p| (p.guest.get(), p.protocol)).collect();
    assert_eq!(
        protocols,
        [(22, None), (53, Some(Protocol::Udp)), (80, Some(Protocol::Tcp))],
        "Vagrant's own SSH forward isn't in the file: unknown"
    );
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

/// A vagrant-qemu VM whose QEMU monitor is out of reach (nothing recorded, no Vagrant home):
/// its ports are unknown, warned, and the run stands.
#[tokio::test]
async fn a_qemu_vm_whose_monitor_cant_say_its_ports_has_none_warned_and_runs_on() {
    let f = Fixture::new();
    f.made("web", "qemu", "id-web");
    let fake = FakeProcesses::script()
        .expect("vagrant status --machine-readable", Reply::ok("1,web,state,running\n"))
        .expect(
            "vagrant port web",
            Reply {
                code: 1,
                ..Reply::ok(&fixture("vagrant/2.4.1-qemu-port-unsupported.txt"))
            },
        )
        .build();
    let (seen, state) = Vagrant.observe(&cx(&f, &fake, Some(Provider::Qemu))).await.expect("observe");
    assert_eq!((seen.machines[0].published.len(), state), (0, RunState::Ready));
    assert_eq!(f.warnings(), [WarningCode::PortsUnknown]);
}

/// A VM reached at an address of its own (libvirt's, ESXi's) has the one `ssh-config` gives in
/// what is seen; one reached through a port forwarded on the loopback (VirtualBox's) isn't asked
/// for one. Regression: on ESXi, a running VM whose address couldn't be read vanished from
/// `status` with its ports; it is still read, without one.
#[tokio::test]
async fn a_vm_on_a_provider_that_gives_its_own_address_has_it_and_one_unreadable_is_still_seen() {
    let not_ready = Reply::fail(
        1,
        "The provider for this Vagrant-managed machine is reporting that it is not yet ready for SSH.",
    );
    let cases = [
        (
            LIBVIRT,
            "libvirt",
            Some(Reply::ok("Host web\n  HostName 192.168.121.10\n  User vagrant\n")),
            Some("192.168.121.10"),
        ),
        (LIBVIRT, "libvirt", Some(not_ready), None),
        (VBOX, "virtualbox", None, None),
    ];
    for (provider, kind, ssh_config, address) in cases {
        let f = Fixture::new();
        f.made("web", kind, "uuid-web");
        let mut script = FakeProcesses::script()
            .expect("vagrant status --machine-readable", Reply::ok("1,web,state,running\n"))
            .expect("vagrant port web --machine-readable", Reply::ok(""));
        if let Some(reply) = ssh_config {
            script = script.expect("vagrant ssh-config web", reply);
        }
        let fake = script.build();
        let (seen, _) = Vagrant.observe(&cx(&f, &fake, provider)).await.expect("observe");
        let web: Vec<_> = seen
            .machines
            .iter()
            .map(|m| (m.name.as_str(), m.state.clone(), m.kind.as_deref(), m.address.as_deref()))
            .collect();
        assert_eq!(web, [("web", MachineState::Running, Some(kind), address)], "{kind}, {address:?}");
    }
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
