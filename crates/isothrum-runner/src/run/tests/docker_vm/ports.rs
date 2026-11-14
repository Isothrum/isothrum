//! The host ports a docker-vm run's VM forwards to this computer: an ephemeral one at the port the
//! runner pinned, the helper VM's SSH at a free one, a port another lab holds or none left free
//! stopping the start before anything is made.

use super::*;

/// The fixture with its published port ephemeral, not fixed.
fn ephemeral() -> String {
    let ephemeral = DOCKER_VM.replace(r#""mode": "fixed""#, r#""mode": "ephemeral""#);
    assert_ne!(ephemeral, DOCKER_VM, "the fixture's port is fixed");
    ephemeral
}

/// An ephemeral port is forwarded from the host port the runner pinned, not from the one Compose
/// publishes in the VM: status, the outputs and `${published.…}` name the pinned one, never the
/// VM's number on this computer, where another program may answer.
#[tokio::test(start_paused = true)]
async fn an_ephemeral_port_in_the_vm_answers_at_the_pinned_host_port() {
    let l = local_on(&ephemeral(), &[50123]);
    let run = l.runner.open(&l.manifest, None).unwrap();
    let started = run.up(&opts(), &Recorder::new()).await.unwrap();
    let page = started.outputs.iter().find(|o| o.name == "web__portal").unwrap();
    assert_eq!(page.value.as_deref(), Some("http://127.0.0.1:50123/"));
    let status = run.status().await.unwrap();
    assert_eq!(crate::testing::published_numbers(&status.machines, "web"), [(80, 50123)]);
}

/// On QEMU, vagrant-qemu forwards every VM's SSH (guest 22) to one fixed host port (50022) with
/// weak auto-correct, so two local docker-vm runs — or a half-created VM left behind — collide on
/// it. The runner gives the helper VM's SSH forward a free host port of its own, carried to the
/// Vagrantfile in `ISOLOOM_HOSTPORTS` under `<host>/ssh` ([`HOST`], the guest machine's name); the run's own
/// `web/portal` is fixed, so that is the only entry.
#[tokio::test(start_paused = true)]
async fn on_qemu_the_helper_vms_ssh_forward_takes_a_free_host_port() {
    let l = local_on(DOCKER_VM, &[50123]);
    let run = l.runner.open(&l.manifest, None).unwrap();
    let mut secrets = Secrets::new();
    secrets.insert("DEMO_SECRET", SecretValue::new(SECRET));
    run.up(
        &RunOptions::new().with_provider(isoloom_manifest::Provider::Qemu).with_secrets(secrets),
        &Recorder::new(),
    )
    .await
    .unwrap();
    let up = l.fake.last("vagrant up").unwrap();
    assert_eq!(
        up.env("ISOLOOM_HOSTPORTS"),
        Some(format!(r#"{{"{HOST}/ssh":50123}}"#).as_str()),
        "the chosen SSH port reaches the Vagrantfile"
    );
}

#[tokio::test(start_paused = true)]
async fn a_forwarded_port_another_lab_holds_stops_the_start_before_the_vm_is_made() {
    let l = local();
    l.ports.take(8080, Protocol::Tcp);
    let run = l.runner.open(&l.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::PortInUse);
    assert!(l.fake.last("vagrant up").is_none(), "nothing was made");
}

/// An ephemeral port no free host port is left for fails the start, naming the port, as Compose's
/// pick does: it was left out in silence, and the VM forwarded it at its `preferred` number, which
/// a resume needn't get again.
#[tokio::test(start_paused = true)]
async fn an_ephemeral_port_with_no_free_host_port_left_fails_the_start() {
    let l = local_with(&ephemeral());
    let run = l.runner.open(&l.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert!(
        matches!(&err, crate::Error::ToolFailed { step, reason } if step == "host-ports"
            && reason.starts_with("no free host port over tcp that another run starting hasn't reserved could be found to pin machine `")),
        "{err}"
    );
    assert!(l.fake.last("vagrant up").is_none(), "nothing was made");
}
