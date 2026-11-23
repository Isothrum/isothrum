//! An SSH server as a caller sets one up: its host keys shown, then confirmed; signed in to with a
//! key it lets in, refused a key it doesn't know; signed in to with a password.

use std::path::{Path, PathBuf};
use std::time::Duration;

use isothrum_machine::Context;
use isothrum_machine::place::ssh::{SshError, Target, ensure_key, known_hosts};

use crate::{here, setting, settings};

/// How long one command over SSH may take.
const LIMIT: Duration = Duration::from_secs(30);

/// The SSH server the settings name: its address, port and user.
pub(super) struct Server {
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) user: String,
}

impl Server {
    /// The server `ISOTHRUM_TEST_SSH_HOST` names; why not, when it isn't set.
    pub(super) fn named() -> Result<Server, String> {
        let [host] = settings(["ISOTHRUM_TEST_SSH_HOST"])?;
        Ok(Server {
            host,
            port: setting("ISOTHRUM_TEST_SSH_PORT").and_then(|p| p.parse().ok()).unwrap_or(22),
            user: setting("ISOTHRUM_TEST_SSH_USER").unwrap_or_else(|| "root".into()),
        })
    }

    /// The server signed in to with the key at `identity`, its host keys confirmed in `known`.
    pub(super) fn target(&self, identity: PathBuf, known: PathBuf) -> Target {
        Target::new(self.host.clone(), self.user.clone(), identity, known).on_port(self.port)
    }

    /// Its host keys confirmed in `known`, as a user confirms the ones shown.
    pub(super) async fn confirm_in(&self, cx: &Context, known: &Path) {
        let offered = known_hosts::offered(cx, &self.host, self.port, known).await.expect("its keys");
        let shown: Vec<String> = offered.keys.iter().map(|k| k.fingerprint.clone()).collect();
        known_hosts::confirm(cx, &self.host, self.port, &shown, &[known.to_path_buf()])
            .await
            .expect("confirmed");
    }
}

/// A fresh key, made in `dir`: its private file and its public line.
pub(super) async fn fresh_key(cx: &Context, dir: &Path) -> (PathBuf, String) {
    let key = dir.join("id_ed25519");
    let public = ensure_key(cx, &key, "isothrum-e2e").await.expect("a key");
    assert!(public.starts_with("ssh-ed25519 "), "{public}");
    (key, public)
}

/// Until its host keys are confirmed nothing is sent to it; shown, then confirmed, they read as
/// confirmed. A key the server doesn't know is then refused.
#[tokio::test]
#[ignore = "real server: cargo xtask e2e --tier server"]
async fn a_servers_host_keys_are_confirmed_before_anything_is_sent() {
    let server = match Server::named() {
        Ok(s) => s,
        Err(why) => skip!("no SSH server: {why}"),
    };
    let cx = here().await;
    let dir = tempfile::tempdir().expect("a folder");
    let (key, _) = fresh_key(&cx, dir.path()).await;
    let known = dir.path().join("known_hosts");
    let target = server.target(key, known.clone());
    assert_eq!(
        target.exec(&cx, "true", LIMIT).await,
        Err(SshError::HostKeyRefused { host: server.host.clone() })
    );
    let offered = known_hosts::offered(&cx, &server.host, server.port, &known).await.expect("its keys");
    println!("{offered:?}");
    assert_eq!(offered.state, known_hosts::State::New);
    server.confirm_in(&cx, &known).await;
    let again = known_hosts::offered(&cx, &server.host, server.port, &known).await.expect("its keys");
    assert_eq!(again.state, known_hosts::State::Confirmed);
    let denied = target.exec(&cx, "true", LIMIT).await.expect_err("an unknown key is refused");
    println!("an unknown key: {denied}");
    assert!(matches!(&denied, SshError::Failed(why) if why.contains("Permission denied")), "{denied:?}");
}

/// A key the server lets in signs in, and a command runs there; so does the user's password,
/// answered through the askpass helper.
#[tokio::test]
#[ignore = "real server: cargo xtask e2e --tier server"]
async fn a_key_and_a_password_each_sign_in_and_run_a_command() {
    let (server, [key, password]) = match Server::named().and_then(|s| Ok((s, settings(["ISOTHRUM_TEST_SSH_KEY", "ISOTHRUM_TEST_SSH_PASSWORD"])?))) {
        Ok(found) => found,
        Err(why) => skip!("no SSH server to sign in to: {why}"),
    };
    let cx = here().await;
    let dir = tempfile::tempdir().expect("a folder");
    let known = dir.path().join("known_hosts");
    server.confirm_in(&cx, &known).await;
    let by_key = server.target(PathBuf::from(key), known.clone());
    assert_eq!(
        by_key.exec(&cx, "echo signed-in", LIMIT).await.map(|s| s.trim().to_owned()),
        Ok("signed-in".into())
    );
    let (unknown, _) = fresh_key(&cx, dir.path()).await;
    let by_password = server.target(unknown, known).with_password(password, dir.path().to_path_buf());
    assert_eq!(
        by_password.exec(&cx, "echo signed-in", LIMIT).await.map(|s| s.trim().to_owned()),
        Ok("signed-in".into())
    );
}
