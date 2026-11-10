//! Published ports: ESXi's at the VM's address, a local VM's pinned host port, fixed ports.

use super::*;

/// ESXi forwards no port to this computer: a published port is shown at its VM's own address
/// (what `vagrant ssh-config` reports), with the VM's own port; locally, at the loopback with the
/// forwarded one.
#[tokio::test]
async fn on_esxi_a_published_port_is_shown_at_its_vms_address() {
    let f = fixture(HostOs::Linux);
    f.engine().plugins.push("vagrant-vmware-esxi".into());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let opts = on_esxi(esxi_secrets());
    let started = run.up(&opts, &Recorder::new()).await.unwrap();
    assert_eq!(started.outputs[0].value.as_deref(), Some("http://172.16.1.53:80/"));
    let status = run.status().await.unwrap();
    assert_eq!(status.outputs[0].value.as_deref(), Some("http://172.16.1.53:80/"));
    let web = status.machines.iter().find(|m| m.name == "web").unwrap();
    assert_eq!(web.published.iter().map(|p| (p.port, p.host_port)).collect::<Vec<_>>(), [(80, 80)]);
    assert!(f.fake.lines().contains(&"vagrant ssh-config web".to_owned()), "{:?}", f.fake.lines());
    assert!(
        !f.fake.lines().iter().any(|l| l == "vagrant ssh-config isoloom-controller"),
        "the controller publishes nothing"
    );
}

/// Regression: on ESXi, a VM whose address couldn't be read vanished from `status` with its
/// ports, and nothing said why.
#[tokio::test]
async fn on_esxi_an_address_that_cant_be_read_is_left_out_and_traced() {
    let f = fixture(HostOs::Linux);
    f.engine().plugins.push("vagrant-vmware-esxi".into());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&on_esxi(esxi_secrets()), &Recorder::new()).await.unwrap();
    f.engine().ssh_config_fails = true;
    let (status, traced) = crate::testing::traced(run.status()).await;
    let web = status.unwrap().machines.into_iter().find(|m| m.name == "web").unwrap();
    assert!(web.published.is_empty(), "left out: {web:?}");
    assert!(
        traced.contains("WARN") && traced.contains("address couldn't be read") && traced.contains("web"),
        "{traced}"
    );
}

/// A fresh local start pins a free host port for an ephemeral published port, forwards the VM to
/// it (the Vagrantfile reads `ISOLOOM_HOSTPORTS`), and the run answers on it — not on its own 80.
#[tokio::test]
async fn a_fresh_local_start_pins_a_free_host_port_and_answers_on_it() {
    let f = fixture(HostOs::Linux);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let started = run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(
        f.fake.last("vagrant up").unwrap().env("ISOLOOM_HOSTPORTS"),
        Some(r#"{"web/web":41000}"#),
        "the pinned port reaches the Vagrantfile"
    );
    assert_eq!(started.outputs[0].value.as_deref(), Some("http://127.0.0.1:41000/"));
    let status = run.status().await.unwrap();
    let web = status.machines.iter().find(|m| m.name == "web").unwrap();
    assert_eq!(web.published.iter().map(|p| (p.port, p.host_port)).collect::<Vec<_>>(), [(80, 41000)]);
}

/// Regression: a VM forwarding to this computer answered at the loopback whatever address the
/// manifest publishes its ports on (`${host.<machine>}`).
#[tokio::test]
async fn a_local_vm_answers_at_the_address_its_ports_are_published_on() {
    let mut m: serde_json::Value = serde_json::from_str(&each_publish(|publish| {
        publish.insert("address".to_owned(), "192.0.2.9".into());
    }))
    .unwrap();
    m["outputs"][0]["format"] = "http://${host.web}:${value}/".into();
    let f = fixture_from(HostOs::Linux, &m.to_string());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let started = run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(started.outputs[0].value.as_deref(), Some("http://192.0.2.9:41000/"));
}

/// A published port with no `preferred` number (an ephemeral port Isoloom emits without a
/// `publish:` number) was dropped from `status` entirely, so `${published…}`/`${host…}` could not
/// resolve. Its host port is the runner's pick, so it is reported like any other published port.
#[tokio::test]
async fn a_published_port_with_no_preferred_number_is_still_reported() {
    let f = fixture_from(HostOs::Linux, &ephemeral_no_preferred_manifest());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let status = run.status().await.unwrap();
    let web = status.machines.iter().find(|m| m.name == "web").unwrap();
    assert_eq!(web.published.iter().map(|p| (p.port, p.host_port)).collect::<Vec<_>>(), [(80, 41000)]);
}

/// A resume forwards the same pinned port (kept in the record), so the VM's URL doesn't change —
/// no port is picked again (the fake has none left to hand out).
#[tokio::test]
async fn a_resume_forwards_the_same_pinned_port() {
    let f = fixture(HostOs::Linux);
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    run.park(Park::Shutdown, &RunOptions::new(), &Recorder::new()).await.unwrap();
    run.resume(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert_eq!(f.fake.last("vagrant up").unwrap().env("ISOLOOM_HOSTPORTS"), Some(r#"{"web/web":41000}"#));
    let status = run.status().await.unwrap();
    let web = status.machines.iter().find(|m| m.name == "web").unwrap();
    assert_eq!(web.published.iter().find(|p| p.port == 80).unwrap().host_port, 41000);
}

/// With `ISOLOOM_PUBLISH_FIXED`, a VM forwards the run's own port number instead of a free one.
#[tokio::test]
async fn in_fixed_mode_a_vm_forwards_the_labs_own_port() {
    let f = fixture(HostOs::Linux);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let started = run.up(&opts().with_param("ISOLOOM_PUBLISH_FIXED", "1"), &Recorder::new()).await.unwrap();
    assert_eq!(f.fake.last("vagrant up").unwrap().env("ISOLOOM_HOSTPORTS"), Some(r#"{"web/web":8086}"#));
    assert_eq!(started.outputs[0].value.as_deref(), Some("http://127.0.0.1:8086/"));
}

/// A VM with only fixed ports (Isoloom before this change, or a run that pins a port) picks
/// nothing and sets no `ISOLOOM_HOSTPORTS`; it forwards each port at its own number.
#[tokio::test]
async fn a_vm_with_only_fixed_ports_sets_no_host_ports_env() {
    let f = fixture_from(HostOs::Linux, &fixed_manifest());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let started = run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(f.fake.last("vagrant up").unwrap().env("ISOLOOM_HOSTPORTS"), None);
    assert_eq!(started.outputs[0].value.as_deref(), Some("http://127.0.0.1:8086/"));
}
