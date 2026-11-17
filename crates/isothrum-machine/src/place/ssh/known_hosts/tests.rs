use std::sync::Arc;

use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::testing::shared;

/// Two public host keys (made for these tests), with the fingerprints `ssh-keygen -lf` gives them.
const KEY_1: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIMV/X9GBl8lRmDMg/hSFR41QZH/fgA5PLNatzgxigSSL";
const FINGERPRINT_1: &str = "SHA256:Coh2bRLeV1HsLRUtSn+G8PFvq/E3/excrj4hN1k1YEQ";
const KEY_2: &str = "AAAAC3NzaC1lZDI1NTE5AAAAICqe9BTRKrDHvwjUyYLAuhUCKs3XEHQ043EztdSCVfwq";
const FINGERPRINT_2: &str = "SHA256:Z+fWYzq6ygA/L76meHUVttMwHeuEdgXt4uJu/8tVUEM";

fn scan_of(pattern: &str, key: &str) -> String {
    format!("{pattern} ssh-ed25519 {key}\n")
}

fn key(blob: &str) -> HostKey {
    HostKey {
        algorithm: "ssh-ed25519".to_owned(),
        blob: blob.to_owned(),
    }
}

#[test]
fn a_key_reads_as_openssh_prints_its_fingerprint() {
    assert_eq!(key(KEY_1).fingerprint().as_deref(), Some(FINGERPRINT_1));
    assert_eq!(key(KEY_2).fingerprint().as_deref(), Some(FINGERPRINT_2));
    assert_eq!(key("not base64!").fingerprint(), None);
    assert_eq!(pattern("10.0.0.50", 22), "10.0.0.50");
    assert_eq!(pattern("pve.lan", 2222), "[pve.lan]:2222");
}

#[test]
fn base64_reads_back_what_it_writes() {
    assert_eq!(base64_decode("aGVsbG8=").as_deref(), Some(&b"hello"[..]));
    assert_eq!(base64_decode("aGVsbG8").as_deref(), Some(&b"hello"[..]));
    assert_eq!(base64_decode("aGk=").as_deref(), Some(&b"hi"[..]));
    assert_eq!(base64_decode("a"), None);
    assert_eq!(base64_encode_unpadded(b"hello"), "aGVsbG8");
    assert_eq!(base64_encode_unpadded(b"hi!"), "aGkh");
    assert_eq!(base64_encode_unpadded(b""), "");
}

#[test]
fn only_the_named_hosts_keys_are_read() {
    let text = format!(
        "# pve:22 SSH-2.0-OpenSSH_9.2\n{}other ssh-ed25519 {KEY_2}\n@revoked pve ssh-ed25519 {KEY_2}\n|1|c2FsdA==|aGFzaA== ssh-ed25519 {KEY_2}\n",
        scan_of("pve", KEY_1)
    );
    let keys = keys_in(&text, "pve");
    assert_eq!(keys.iter().map(|k| k.blob.as_str()).collect::<Vec<_>>(), [KEY_1, KEY_2]);
}

#[test]
fn the_offered_keys_are_new_confirmed_or_changed() {
    assert_eq!(state(&[key(KEY_1)], &[]), State::New);
    assert_eq!(state(&[key(KEY_1), key(KEY_2)], &[key(KEY_1)]), State::Confirmed);
    assert_eq!(state(&[key(KEY_2)], &[key(KEY_1)]), State::Changed);
}

#[tokio::test]
async fn a_new_server_is_shown_with_its_fingerprints_and_confirmed_into_every_file() {
    let dir = tempfile::tempdir().unwrap();
    let files = [dir.path().join("a").join("known_hosts"), dir.path().join("b").join("known_hosts")];
    let fake = Arc::new(
        FakeProcesses::script()
            .expect(
                "ssh-keyscan -T 10 -p 2222 -t ed25519,ecdsa,rsa pve.lan",
                Reply::ok(&scan_of("[pve.lan]:2222", KEY_1)),
            )
            .build(),
    );
    let runner: Arc<dyn isothrum_runner::ProcessRunner> = Arc::<FakeProcesses>::clone(&fake);
    let ctx = shared(runner);
    let offered = offered(&ctx, "pve.lan", 2222, &files[0]).await.unwrap();
    assert_eq!(offered.state, State::New);
    assert_eq!(
        offered.keys,
        [Shown {
            algorithm: "ssh-ed25519".to_owned(),
            fingerprint: FINGERPRINT_1.to_owned()
        }]
    );
    confirm(&ctx, "pve.lan", 2222, &[FINGERPRINT_1.to_owned()], &files).await.unwrap();
    for file in &files {
        assert_eq!(std::fs::read_to_string(file).unwrap(), format!("[pve.lan]:2222 ssh-ed25519 {KEY_1}\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(file).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}

#[tokio::test]
async fn keys_that_changed_after_they_were_shown_are_not_confirmed() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("known_hosts");
    let ctx = shared(Arc::new(
        FakeProcesses::script().expect("ssh-keyscan", Reply::ok(&scan_of("pve", KEY_2))).build(),
    ));
    let e = confirm(&ctx, "pve", 22, &[FINGERPRINT_1.to_owned()], std::slice::from_ref(&file))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("changed while they were being confirmed"), "{e}");
    assert!(!file.exists(), "nothing written");
}

#[tokio::test]
async fn a_server_whose_keys_changed_says_so_and_a_new_confirmation_replaces_them() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("known_hosts");
    std::fs::write(&file, format!("other ssh-ed25519 {KEY_2}\n{}", scan_of("pve", KEY_1))).unwrap();
    let fake = Arc::new(
        FakeProcesses::script()
            .expect("ssh-keyscan", Reply::ok(&scan_of("pve", KEY_2)))
            .expect("ssh-keygen -F pve", Reply::ok(&scan_of("pve", KEY_1)))
            .expect("ssh-keygen -R pve", Reply::ok(""))
            .build(),
    );
    let runner: Arc<dyn isothrum_runner::ProcessRunner> = Arc::<FakeProcesses>::clone(&fake);
    let ctx = shared(runner);
    let offered = offered(&ctx, "pve", 22, &file).await.unwrap();
    assert_eq!((offered.state, offered.keys[0].fingerprint.as_str()), (State::Changed, FINGERPRINT_2));
    confirm(&ctx, "pve", 22, &[FINGERPRINT_2.to_owned()], std::slice::from_ref(&file))
        .await
        .unwrap();
    assert!(fake.last("ssh-keygen -R pve -f").is_some(), "the old key forgotten first: {:?}", fake.calls());
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("other ssh-ed25519"), "the other hosts kept: {text}");
    assert!(text.ends_with(&scan_of("pve", KEY_2)), "{text}");
}

#[tokio::test]
async fn a_known_file_without_the_server_reads_as_new() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("known_hosts");
    std::fs::write(&file, format!("other ssh-ed25519 {KEY_2}\n")).unwrap();
    let ctx = shared(Arc::new(
        FakeProcesses::script()
            .expect("ssh-keyscan", Reply::ok(&scan_of("pve", KEY_1)))
            .expect("ssh-keygen -F pve", Reply::fail(1, ""))
            .build(),
    ));
    assert_eq!(offered(&ctx, "pve", 22, &file).await.unwrap().state, State::New);
}

#[tokio::test]
async fn a_server_that_offers_no_key_or_cant_be_reached_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("known_hosts");
    let ctx = shared(Arc::new(
        FakeProcesses::script()
            .expect("ssh-keyscan -T 10 -p 22 -t ed25519,ecdsa,rsa silent", Reply::ok(""))
            .expect("ssh-keyscan -T 10 -p 22 -t ed25519,ecdsa,rsa down", Reply::fail(1, "connect refused"))
            .build(),
    ));
    assert!(
        offered(&ctx, "silent", 22, &file)
            .await
            .unwrap_err()
            .to_string()
            .contains("offered no SSH host key")
    );
    assert!(offered(&ctx, "down", 22, &file).await.unwrap_err().to_string().contains("connect refused"));
    assert_eq!(offered(&ctx, "-oProxyCommand=x", 22, &file).await.unwrap_err(), SshError::Invalid("host"));
}
