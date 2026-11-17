use std::sync::Arc;

use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::testing::shared;

fn target() -> Target {
    Target::new("10.0.0.5", "debian", PathBuf::from("/k/id"), PathBuf::from("/x/kh"))
}

#[test]
fn exec_runs_batch_mode_with_the_key_port_and_confirmed_keys() {
    let args = target().on_port(2222).exec_args("uptime").unwrap();
    assert_eq!(args[..4], ["-i", "/k/id", "-p", "2222"]);
    assert!(args.contains(&"BatchMode=yes".to_owned()));
    assert!(args.contains(&"StrictHostKeyChecking=yes".to_owned()));
    assert!(args.contains(&"UserKnownHostsFile=/x/kh".to_owned()));
    assert_eq!(args[args.len() - 2..], ["debian@10.0.0.5", "uptime"]);
    // A host that stops answering mid-command ends it within 45 s, not hours later.
    assert_eq!(
        batch_options(Path::new("/kh"))[..4],
        ["BatchMode=yes", "ConnectTimeout=10", "ServerAliveInterval=15", "ServerAliveCountMax=3"]
    );
}

#[test]
fn odd_hosts_and_users_are_refused() {
    assert_eq!(
        Target {
            host: "10.0.0.5;rm".to_owned(),
            ..target()
        }
        .exec_args("true"),
        Err(SshError::Invalid("host"))
    );
    assert_eq!(
        Target {
            user: "-oProxyCommand=x".to_owned(),
            ..target()
        }
        .exec_args("true"),
        Err(SshError::Invalid("user"))
    );
    assert!(safe_token("192.168.1.5") && safe_token("fd00::1") && safe_token("pve_lan-1"));
    assert!(!safe_token("") && !safe_token("-x") && !safe_token("a b") && !safe_token(&"a".repeat(254)));
    assert_eq!(sh_quote("it's"), r"'it'\''s'");
}

#[test]
fn known_hosts_paths_with_spaces_stay_one_file() {
    let kh = Path::new("/Users/a/Library/Application Support/app/ssh/known_hosts");
    assert!(batch_options(kh).contains(&r"UserKnownHostsFile=/Users/a/Library/Application\ Support/app/ssh/known_hosts".to_owned()));
    assert_eq!(config_path(r"C:\Users\x y\50%\kh"), r"C:\\Users\\x\ y\\50%%\\kh");
}

#[tokio::test]
async fn a_refused_host_key_is_its_own_failure_and_others_say_why() {
    let limit = Duration::from_secs(5);
    let refused = Reply::fail(
        255,
        "No ED25519 host key is known for 10.0.0.5 and you have requested strict checking.\nHost key verification failed.",
    );
    let refusing = shared(Arc::new(FakeProcesses::script().expect("ssh", refused).build()));
    assert_eq!(
        target().exec(&refusing, "true", limit).await,
        Err(SshError::HostKeyRefused { host: "10.0.0.5".to_owned() })
    );
    let denying = shared(Arc::new(
        FakeProcesses::script()
            .expect("ssh", Reply::fail(255, "Permission denied (publickey)."))
            .build(),
    ));
    let denied = target().exec(&denying, "true", limit).await.unwrap_err();
    assert!(matches!(&denied, SshError::Failed(why) if why.contains("Permission denied")), "{denied:?}");
    let answering = shared(Arc::new(FakeProcesses::script().expect("ssh -i /k/id -p 22", Reply::ok("pve")).build()));
    assert_eq!(target().exec(&answering, "hostname", limit).await.unwrap(), "pve");
}

#[tokio::test]
async fn a_key_is_made_once_where_the_caller_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("ssh").join("id_ed25519");
    std::fs::create_dir_all(key.parent().unwrap()).unwrap();
    std::fs::write(public_half(&key), "ssh-ed25519 AAAA test\n").unwrap();
    let fake = Arc::new(
        FakeProcesses::script()
            .expect_times("ssh-keygen -q -t ed25519 -N * -C test -f", 1, Reply::ok(""))
            .build(),
    );
    let runner: Arc<dyn isothrum_runner::ProcessRunner> = Arc::<FakeProcesses>::clone(&fake);
    let ctx = shared(runner);
    assert_eq!(ensure_key(&ctx, &key, "test").await.unwrap(), "ssh-ed25519 AAAA test");
    // Made now (the fake wrote nothing; the test writes the private half as ssh-keygen would).
    std::fs::write(&key, "private").unwrap();
    assert_eq!(ensure_key(&ctx, &key, "test").await.unwrap(), "ssh-ed25519 AAAA test", "kept as it is");
    assert_eq!(public_half(Path::new("/a/id_ed25519")), Path::new("/a/id_ed25519.pub"));
}

/// A real server's SSH, its host keys trusted on first use: `ISOTHRUM_TEST_SSH_HOST` (and
/// `ISOTHRUM_TEST_SSH_PORT`, 22 when unset). With no key confirmed, ssh refuses the server before
/// signing in; once its keys are confirmed, ssh gets as far as signing in (a key the server doesn't
/// know is denied), and the keys read back as confirmed.
#[tokio::test]
#[ignore = "needs a real SSH server: ISOTHRUM_TEST_SSH_HOST"]
async fn a_real_server() {
    let host = std::env::var("ISOTHRUM_TEST_SSH_HOST").unwrap();
    let port = std::env::var("ISOTHRUM_TEST_SSH_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(22);
    let ctx = crate::Context::here(Arc::new(isothrum_runner::SystemProcesses::default())).await;
    let dir = tempfile::tempdir().unwrap();
    let (key, known) = (dir.path().join("id_ed25519"), dir.path().join("known_hosts"));
    let public = ensure_key(&ctx, &key, "isothrum-test").await.unwrap();
    assert!(public.starts_with("ssh-ed25519 "), "{public}");
    let target = Target::new(host.clone(), "root", key, known.clone()).on_port(port);
    let limit = Duration::from_secs(30);
    assert_eq!(target.exec(&ctx, "true", limit).await, Err(SshError::HostKeyRefused { host: host.clone() }));
    let offered = known_hosts::offered(&ctx, &host, port, &known).await.unwrap();
    assert_eq!(offered.state, known_hosts::State::New);
    let fingerprints: Vec<String> = offered.keys.iter().map(|k| k.fingerprint.clone()).collect();
    println!("offered: {offered:?}");
    known_hosts::confirm(&ctx, &host, port, &fingerprints, std::slice::from_ref(&known))
        .await
        .unwrap();
    assert_eq!(
        known_hosts::offered(&ctx, &host, port, &known).await.unwrap().state,
        known_hosts::State::Confirmed
    );
    let denied = target.exec(&ctx, "true", limit).await.unwrap_err();
    println!("after confirming: {denied}");
    assert!(matches!(&denied, SshError::Failed(why) if why.contains("Permission denied")), "{denied:?}");
}
