//! A Proxmox VE server as a caller sets one up: its certificate read and pinned (the right pin
//! accepted, a wrong one refused before anything is sent), signed in to by password and by
//! token, its placement and setup checks read; and its node over SSH: the caller's key let in by
//! password, then an API token made with it. What the test makes on the node it removes.

use std::time::Duration;

use isothrum_machine::place::proxmox::bootstrap::{self, ApiToken};
use isothrum_machine::place::proxmox::check::{self, Needs};
use isothrum_machine::place::proxmox::{Credentials, Server, SignInError, node, placement, sign_in};
use isothrum_machine::place::ssh::Target;
use isothrum_machine::{Context, place};
use isothrum_runner::tls::{Fingerprint, Refusal, Trust};

use super::ssh::{self, fresh_key};
use crate::{here, setting, settings};

/// How long the certificate's handshake may take.
pub(super) const HANDSHAKE: Duration = Duration::from_secs(10);
/// How long a command on the node may take.
const LIMIT: Duration = Duration::from_secs(60);

/// The server the settings name, and its certificate's fingerprint as set.
pub(super) fn named() -> Result<(Server, Fingerprint), String> {
    let [host, pin] = settings(["ISOTHRUM_TEST_PROXMOX_HOST", "ISOTHRUM_TEST_PROXMOX_FINGERPRINT"])?;
    let port = setting("ISOTHRUM_TEST_PROXMOX_PORT").and_then(|p| p.parse().ok()).unwrap_or(8006);
    let pin = Fingerprint::parse(&pin).ok_or("ISOTHRUM_TEST_PROXMOX_FINGERPRINT isn't a SHA-256")?;
    Ok((Server::new(host, port), pin))
}

/// Where the settings say machines go, Proxmox VE's defaults for those not set.
pub(super) fn needs() -> Needs {
    let mut needs = Needs::default();
    if let Some(storage) = setting("ISOTHRUM_TEST_PROXMOX_STORAGE") {
        needs.image_storage.clone_from(&storage);
        needs.snippet_storage.clone_from(&storage);
        needs.disk_storage = storage;
    }
    if let Some(bridge) = setting("ISOTHRUM_TEST_PROXMOX_BRIDGE") {
        needs.bridge = bridge;
    }
    needs
}

/// The certificate read from the handshake is the one set; pinned, it is accepted, and a wrong
/// pin is refused naming the certificate offered, before any credential is sent.
#[tokio::test]
#[ignore = "real server: cargo xtask e2e --tier server"]
async fn the_servers_certificate_is_read_and_only_its_pin_accepted() {
    let ((server, pin), [password]) = match named().and_then(|s| Ok((s, settings(["ISOTHRUM_TEST_PROXMOX_PASSWORD"])?))) {
        Ok(found) => found,
        Err(why) => skip!("no Proxmox VE server: {why}"),
    };
    let read = place::certificate(&server.host, server.port, HANDSHAKE).await.expect("its certificate");
    assert_eq!(read, pin, "the certificate offered is the one set");
    let credentials = Credentials::from_login("root@pam", &password);
    let wrong = Fingerprint::parse(&"00".repeat(32)).expect("a fingerprint");
    let refused = sign_in(&server, &credentials, Trust::Pinned(Some(wrong)))
        .await
        .expect_err("a wrong pin is refused");
    println!("a wrong pin: {refused}");
    assert!(
        matches!(refused, SignInError::Certificate(Refusal::Changed { presented, .. }) if presented == pin),
        "{refused:?}"
    );
    let unpinned = sign_in(&server, &credentials, Trust::Pinned(None)).await.expect_err("no pin yet is refused");
    assert!(
        matches!(unpinned, SignInError::Certificate(Refusal::Unconfirmed { presented }) if presented == pin),
        "{unpinned:?}"
    );
    sign_in(&server, &credentials, Trust::Pinned(Some(pin))).await.expect("the right pin signs in");
    let rejected = sign_in(&server, &Credentials::from_login("root@pam", "not-the-password"), Trust::Pinned(Some(pin))).await;
    assert!(matches!(rejected, Err(SignInError::Rejected)), "{rejected:?}");
}

/// Signed in with the API token, the server's placement lists the node with the storages and
/// bridge set, the setup checks find nothing missing there, and its capacity reads.
#[tokio::test]
#[ignore = "real server: cargo xtask e2e --tier server"]
async fn the_servers_placement_and_setup_checks_read_through_its_token() {
    let ((server, pin), [id, secret]) = match named().and_then(|s| Ok((s, settings(["ISOTHRUM_TEST_PROXMOX_TOKEN_ID", "ISOTHRUM_TEST_PROXMOX_TOKEN_SECRET"])?)))
    {
        Ok(found) => found,
        Err(why) => skip!("no Proxmox VE token: {why}"),
    };
    let session = sign_in(&server, &Credentials::from_login(&id, &secret), Trust::Pinned(Some(pin)))
        .await
        .expect("signed in");
    let needs = needs();
    let asked = setting("ISOTHRUM_TEST_PROXMOX_NODE");
    let node = session.node(asked.as_deref()).await.expect("its node");
    let placement = placement::read(&session, &needs, server.host.parse().ok()).await.expect("its placement");
    println!("{placement:#?}");
    let choices = placement.nodes.iter().find(|n| n.name == node).expect("the node is offered");
    assert!(
        choices.bridges.iter().any(|b| b.name == needs.bridge),
        "{} is offered: {choices:?}",
        needs.bridge
    );
    let findings = check::check(&session, &node, &needs).await;
    assert_eq!(findings, [], "{node} is ready for {needs:?}");
    let capacity = session.capacity(&node).await.expect("its capacity");
    println!("{node}: {capacity:?}");
    assert!(capacity.cores > 0 && capacity.mem_total >= capacity.mem_free, "{capacity:?}");
}

/// The node, signed in to over SSH by password, lets the caller's key in; the key then signs in on
/// its own, and an API token made with it signs in to the API. The token and the key are taken
/// out again, the key refused once more.
#[tokio::test]
#[ignore = "real server: cargo xtask e2e --tier server"]
async fn the_nodes_key_and_token_are_set_up_by_password_and_taken_out_again() {
    let (node_ssh, (server, pin), [password]) = match ssh::Server::named().and_then(|s| Ok((s, named()?, settings(["ISOTHRUM_TEST_SSH_PASSWORD"])?))) {
        Ok(found) => found,
        Err(why) => skip!("no Proxmox VE node to set up: {why}"),
    };
    let cx = here().await;
    let dir = tempfile::tempdir().expect("a folder");
    let known = dir.path().join("known_hosts");
    node_ssh.confirm_in(&cx, &known).await;
    let (key, public) = fresh_key(&cx, dir.path()).await;
    let by_key = node_ssh.target(key, known);
    let by_password = by_key.clone().with_password(password, dir.path().to_path_buf());
    println!("SDN sourced: {:?}", node::sdn_sourced(&cx, &by_password).await);
    let name = bootstrap::token_name("isothrum-e2e", &public);
    let made = bootstrap::bootstrap(&cx, &by_password, &public, &name).await;
    let signed_in = match &made {
        Ok(m) => Some(token_signs_in(&server, pin, &m.token).await),
        Err(_) => None,
    };
    let key_signed_in = node::signs_in(&cx, &by_key).await;
    take_out(&cx, &by_password, &node_ssh.user, &name, &public).await;
    let made = made.expect("set up");
    assert!(made.key_added, "the key was new to the node");
    assert_eq!(key_signed_in, Ok(()), "the key signs in on its own");
    assert_eq!(signed_in, Some(Ok(())), "the token signs in to the API");
    assert!(node::signs_in(&cx, &by_key).await.is_err(), "the key is refused again");
}

/// Whether `token` signs in to `server`, pinned to `pin`.
async fn token_signs_in(server: &Server, pin: Fingerprint, token: &ApiToken) -> Result<(), String> {
    let credentials = Credentials::from_login(&token.id, &token.secret);
    sign_in(server, &credentials, Trust::Pinned(Some(pin)))
        .await
        .map(drop)
        .map_err(|e| e.to_string())
}

/// Takes token `name` of `user@pam` and key `public` out of the node again.
async fn take_out(cx: &Context, node: &Target, user: &str, name: &str, public: &str) {
    let key = public.split_whitespace().nth(1).unwrap_or_default();
    let commands = [
        format!("pveum user token remove {user}@pam {name}"),
        format!("sed -i '\\#{key}#d' ~/.ssh/authorized_keys"),
    ];
    for command in commands {
        if let Err(e) = node.exec(cx, &command, LIMIT).await {
            println!("couldn't take it out ({command}): {e}");
        }
    }
}
