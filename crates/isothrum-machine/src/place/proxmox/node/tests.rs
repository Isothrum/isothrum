use std::path::PathBuf;
use std::sync::Arc;

use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::testing::shared;

fn node() -> Target {
    Target::new("10.0.0.50", "root", PathBuf::from("/k/id"), PathBuf::from("/kh"))
}

#[tokio::test]
async fn a_node_that_sources_its_sdn_config_reads_so_and_one_that_doesnt_too() {
    let yes = shared(Arc::new(FakeProcesses::script().expect("ssh", Reply::ok("")).build()));
    assert_eq!(sdn_sourced(&yes, &node()).await, Ok(true));
    let no = shared(Arc::new(FakeProcesses::script().expect("ssh", Reply::fail(1, "")).build()));
    assert_eq!(sdn_sourced(&no, &node()).await, Ok(false));
    let refused = shared(Arc::new(
        FakeProcesses::script()
            .expect("ssh", Reply::fail(255, "root@10.0.0.50: Permission denied (publickey)."))
            .build(),
    ));
    assert!(matches!(sdn_sourced(&refused, &node()).await, Err(SshError::Failed(why)) if why.contains("Permission denied")));
    assert!(SDN_SOURCE_CHECK.contains("/etc/network/interfaces"));
}

#[tokio::test]
async fn a_key_the_node_lets_in_signs_in() {
    let ok = shared(Arc::new(FakeProcesses::script().expect("ssh -i /k/id", Reply::ok("")).build()));
    assert_eq!(signs_in(&ok, &node()).await, Ok(()));
}

/// A real node signed in to by password (`ISOTHRUM_TEST_SSH_HOST`, `ISOTHRUM_TEST_SSH_PASSWORD`,
/// as root), its host keys confirmed first: whether it sources its SDN config, and a second key
/// let in by password then signing in by itself.
#[tokio::test]
#[ignore = "needs a real Proxmox VE node: ISOTHRUM_TEST_SSH_HOST and ISOTHRUM_TEST_SSH_PASSWORD"]
async fn a_real_node_by_password() {
    use crate::place::ssh::{ensure_key, known_hosts};
    let host = std::env::var("ISOTHRUM_TEST_SSH_HOST").unwrap();
    let password = std::env::var("ISOTHRUM_TEST_SSH_PASSWORD").unwrap();
    let ctx = crate::Context::here(Arc::new(isothrum_runner::SystemProcesses::default())).await;
    let dir = tempfile::tempdir().unwrap();
    let (key, known) = (dir.path().join("id_ed25519"), dir.path().join("known_hosts"));
    let public = ensure_key(&ctx, &key, "isothrum-test").await.unwrap();
    let offered = known_hosts::offered(&ctx, &host, 22, &known).await.unwrap();
    let shown: Vec<String> = offered.keys.iter().map(|k| k.fingerprint.clone()).collect();
    known_hosts::confirm(&ctx, &host, 22, &shown, std::slice::from_ref(&known)).await.unwrap();
    let by_key = Target::new(host.clone(), "root", key, known);
    let by_password = by_key.clone().with_password(password, dir.path().to_path_buf());
    println!("sdn sourced: {:?}", sdn_sourced(&ctx, &by_password).await);
    let added = crate::place::proxmox::bootstrap::authorize(&ctx, &by_password, &public).await.unwrap();
    println!("key added: {added}");
    signs_in(&ctx, &by_key).await.unwrap();
    // Taken out again: the test leaves the node's authorized_keys as it found them.
    let key_part = public.split_whitespace().nth(1).unwrap().to_owned();
    by_key
        .exec(&ctx, &format!("sed -i '\\#{key_part}#d' ~/.ssh/authorized_keys"), Duration::from_secs(30))
        .await
        .unwrap();
    assert!(signs_in(&ctx, &by_key).await.is_err(), "the key is gone again");
}
