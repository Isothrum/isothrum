//! The server self-test (`selftest::server`) on the Proxmox VE server the settings name, as a
//! caller runs it: its token signs in, a throwaway VM comes up on the bridge set, is reached over
//! SSH through the node (the SSH server's settings: the bridge may be internal to the node) with
//! the node's key, runs a command, and is taken down with everything the test made.

use std::sync::{Mutex, PoisonError};

use isothrum_machine::place;
use isothrum_machine::place::proxmox::{Credentials, sign_in, terraform_token};
use isothrum_machine::selftest::Progress;
use isothrum_machine::selftest::hypervisor::TestVm;
use isothrum_machine::selftest::server::{self, Found, Key, Proxmox, Server, SignIn, Step, StepProgress};
use isothrum_runner::SecretValue;
use isothrum_runner::tls::Trust;

use super::proxmox::{HANDSHAKE, named, needs};
use super::ssh;
use crate::{answer, here, setting, settings};

/// The test VM's name on the server, and its host name inside.
const VM_NAME: &str = "isothrum-e2e-server";
const VM_HOST: &str = "isothrum-e2e";

/// The steps, in the order a passing test takes them.
const STEPS: [Step; 9] = [
    Step::Host,
    Step::Prepare,
    Step::Image,
    Step::Network,
    Step::Vm,
    Step::Address,
    Step::Ssh,
    Step::Exec,
    Step::Down,
];

/// Every step passes in its order, the command runs inside, and the VM and the test's network are
/// taken down. The VM lets in the node's key, which the snippet's upload and the jump sign in with.
#[tokio::test]
#[ignore = "real server: cargo xtask e2e --tier server"]
async fn the_server_self_test_brings_a_vm_up_on_proxmox_and_takes_it_down() {
    let set = named().and_then(|s| {
        let node_ssh = ssh::Server::named()?;
        let [id, secret, key] = settings(["ISOTHRUM_TEST_PROXMOX_TOKEN_ID", "ISOTHRUM_TEST_PROXMOX_TOKEN_SECRET", "ISOTHRUM_TEST_SSH_KEY"])?;
        Ok((s, node_ssh, [id, secret, key]))
    });
    let ((server, pin), node_ssh, [id, secret, key]) = match set {
        Ok(set) => set,
        Err(why) => skip!("no Proxmox VE server to run the self-test on: {why}"),
    };
    let cx = here().await;
    // Terraform reads the certificate unchecked: the one offered is the one pinned first.
    let read = place::certificate(&server.host, server.port, HANDSHAKE).await.expect("its certificate");
    assert_eq!(read, pin, "the certificate offered is the one set");
    let session = sign_in(&server, &Credentials::from_login(&id, &secret), Trust::Pinned(Some(pin)))
        .await
        .expect("the token signs in");
    let node = session.node(setting("ISOTHRUM_TEST_PROXMOX_NODE").as_deref()).await.expect("its node");
    let public = answer(&cx, "ssh-keygen", &["-y", "-f", &key]).await.expect("the key's public half");
    let token = SignIn::Token(SecretValue::new(terraform_token(&id, &secret)));
    let target = through_node(Proxmox::new(server.endpoint(), token, node), &node_ssh, Key::new(key, public.trim()));
    let dir = tempfile::tempdir().expect("a folder");
    let said = Mutex::new(Vec::new());
    let sink = |progress: StepProgress| {
        if !matches!(progress, Progress::Passed(_, Found::Line { .. })) {
            println!("{progress:?}");
            said.lock().unwrap_or_else(PoisonError::into_inner).push(progress);
        }
    };
    let ran = Box::pin(server::run(&cx, dir.path(), &Server::Proxmox(target), &TestVm::new(VM_NAME, VM_HOST), &sink)).await;
    if let Err(failed) = &ran.passed {
        panic!("{failed}")
    }
    assert_eq!(ran.cleaned_up, Some(Ok(())), "taken down");
    let said = said.into_inner().unwrap_or_else(PoisonError::into_inner);
    assert_eq!(passed(&said), STEPS, "every step passes, in its order");
    assert!(
        said.iter()
            .any(|step| matches!(step, Progress::Passed(Step::Exec, Found::Uname(u)) if u.contains("Linux"))),
        "the command ran inside: {said:?}"
    );
}

/// `p` with the settings' storage and bridge, Terraform reading the certificate unchecked, and
/// everything over SSH through the node `node_ssh` with `key`: the snippet's upload, the jump to
/// the VM, and the key the VM lets in.
fn through_node(mut p: Proxmox, node_ssh: &ssh::Server, key: Key) -> Proxmox {
    let needs = needs();
    p.insecure = true;
    p.bridge = needs.bridge;
    p.datastore = Some(needs.disk_storage);
    p.image_datastore = Some(needs.image_storage);
    p.snippets_datastore = Some(needs.snippet_storage);
    p.node_ssh_user = Some(node_ssh.user.clone());
    p.node_ssh_key_file = Some(key.private_file.clone());
    p.node_address = Some(node_ssh.host.clone());
    p.jump = Some(if node_ssh.port == 22 {
        format!("{}@{}", node_ssh.user, node_ssh.host)
    } else {
        format!("{}@{}:{}", node_ssh.user, node_ssh.host, node_ssh.port)
    });
    p.key = Some(key);
    p
}

/// The steps that passed in `said`, in order (not the preparation's word that it takes an
/// earlier test's VM down).
fn passed(said: &[StepProgress]) -> Vec<Step> {
    said.iter()
        .filter_map(|progress| match progress {
            Progress::Passed(step, found) if !matches!(found, Found::Removing) => Some(*step),
            _ => None,
        })
        .collect()
}
