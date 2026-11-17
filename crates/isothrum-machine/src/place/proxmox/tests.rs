use isothrum_runner::tls::{Fingerprint, Refusal, Trust};
use serde_json::json;

use super::api::{Iface, Node, NodeStatus, Storage};
use super::check::{Content, Finding, Needs, bridge_finding, find_iface, storage_findings};
use super::*;
use isothrum_runner::fake_https::{self as https, Request};

/// `json`, a list of what a path answers, read.
fn listed<T: serde::de::DeserializeOwned>(json: serde_json::Value) -> Vec<T> {
    serde_json::from_value(json).unwrap()
}

#[test]
fn a_login_without_a_realm_is_on_pam() {
    assert_eq!(with_default_realm("root"), "root@pam");
    assert_eq!(with_default_realm("admin@pve"), "admin@pve");
    assert_eq!(with_default_realm("root!ci"), "root@pam!ci");
    assert_eq!(with_default_realm("admin@pve!ci"), "admin@pve!ci");
    assert_eq!(with_default_realm(""), "");
}

#[test]
fn token_ids_are_told_from_users() {
    assert!(is_token("root@pam!ci"));
    assert!(!is_token("root@pam"));
    assert!(!is_token("root!x"));
    assert!(!is_token("root@pam!"));
    assert!(
        is_token("api@pve!ci!2") && is_token("@!x") && !is_token("root!x@pam"),
        "the realm comes before the first `!`"
    );
    assert_eq!(token_user("root@pam!ci"), "root@pam");
    assert_eq!(terraform_token("root@pam!ci", "abc"), "root@pam!ci=abc");
    let token = Credentials::from_login("root!ci", "s3cret");
    assert_eq!(
        token,
        Credentials::Token {
            id: "root@pam!ci".into(),
            secret: "s3cret".into()
        }
    );
    assert_eq!(token.user(), "root@pam");
    assert!(!format!("{token:?}").contains("s3cret"), "a secret is never in Debug");
    let user = Credentials::from_login("root", "pw");
    assert_eq!((user.user(), format!("{user:?}").contains("pw")), ("root@pam", false));
}

#[test]
fn urls_bracket_ipv6_hosts() {
    assert_eq!(Server::new("fd00::5", DEFAULT_PORT).api_url(), "https://[fd00::5]:8006/api2/json");
    assert_eq!(Server::new("pve.example", 8443).endpoint(), "https://pve.example:8443/");
}

#[test]
fn internal_bridges_have_no_ports() {
    let ifaces: Vec<Iface> = listed(json!([{"iface": "vmbr0", "bridge_ports": "eno1"}, {"iface": "vmbr1", "bridge_ports": " "}, {"iface": "vmbr2"}]));
    assert!(!find_iface(&ifaces, "vmbr0").is_some_and(Iface::has_no_ports));
    assert!(find_iface(&ifaces, "vmbr1").is_some_and(Iface::has_no_ports));
    assert!(find_iface(&ifaces, "vmbr2").is_some_and(Iface::has_no_ports));
    assert!(find_iface(&ifaces, "vmbr9").is_none());
    assert_eq!(bridge_finding(&ifaces, "vmbr0"), None);
    assert_eq!(bridge_finding(&ifaces, "vmbr7"), Some(Finding::NoBridge { bridge: "vmbr7".into() }));
}

#[test]
fn each_storage_must_accept_what_it_is_for() {
    let ok: Vec<Storage> =
        listed(json!([{"storage": "local", "content": "iso,vztmpl,snippets,backup"}, {"storage": "local-lvm", "content": "images,rootdir"}]));
    assert_eq!(storage_findings(&ok, &Needs::default()), []);
    let stock: Vec<Storage> = listed(json!([{"storage": "local", "content": "iso,vztmpl,backup"}, {"storage": "local-lvm", "content": "images,rootdir"}]));
    assert_eq!(
        storage_findings(&stock, &Needs::default()),
        [Finding::StorageLacks {
            storage: "local".into(),
            content: Content::Snippets
        }]
    );
    let elsewhere = Needs {
        disk_storage: "fast-ssd".into(),
        ..Needs::default()
    };
    assert!(storage_findings(&stock, &elsewhere).contains(&Finding::NoStorage { storage: "fast-ssd".into() }));
    let all: Vec<Storage> = listed(json!([{"storage": "local", "content": "images,iso,snippets"}]));
    let one = Needs {
        disk_storage: "local".into(),
        ..Needs::default()
    };
    assert_eq!(storage_findings(&all, &one), []);
}

#[test]
fn the_node_asked_or_the_first() {
    let nodes: Vec<Node> = listed(json!([{"node": "pve"}, {"node": "pve2"}]));
    assert_eq!(pick_node(&nodes, None).unwrap(), "pve");
    assert_eq!(pick_node(&nodes, Some("")).unwrap(), "pve");
    assert_eq!(pick_node(&nodes, Some("pve2")).unwrap(), "pve2");
    assert_eq!(
        pick_node(&nodes, Some("nope")).unwrap_err().to_string(),
        "there is no node \"nope\" (found: pve, pve2)"
    );
    assert_eq!(pick_node(&[], None).unwrap_err(), CallError::NoNode);
}

#[test]
fn capacity_needs_every_figure() {
    let status = |json: serde_json::Value| -> NodeStatus { serde_json::from_value(json).unwrap() };
    let c = Capacity::of(&status(json!({"cpuinfo": {"cpus": 8}, "memory": {"total": 64, "free": 20}})));
    assert_eq!(
        c,
        Some(Capacity {
            cores: 8,
            mem_total: 64,
            mem_free: 20
        })
    );
    assert_eq!(Capacity::of(&status(json!({"cpuinfo": {"cpus": 8}}))), None);
}

#[tokio::test]
async fn an_unreachable_server_fails_to_sign_in() {
    // Port 1 on the loopback refuses at once.
    let server = Server::new("127.0.0.1", 1);
    for login in ["root@pam", "root@pam!t"] {
        let signed = sign_in(&server, &Credentials::from_login(login, "pw"), Trust::Pinned(None)).await;
        assert!(matches!(signed, Err(SignInError::Failed(_))), "{signed:?}");
    }
}

/// A Proxmox VE server's answers to a password sign-in and the reads after it.
fn proxmox(request: &Request) -> (u16, String) {
    let data = |v: serde_json::Value| (200, json!({ "data": v }).to_string());
    match (request.method.as_str(), request.target.as_str()) {
        ("POST", "/api2/json/access/ticket") => data(json!({"ticket": "PVE:root@pam:1::sig", "username": "root@pam"})),
        ("GET", "/api2/json/version") => data(json!({"version": "8.2.4"})),
        ("GET", "/api2/json/nodes") => data(json!([{"node": "pve", "status": "online"}])),
        ("GET", "/api2/json/nodes/pve/status") => data(json!({"cpuinfo": {"cpus": 4}, "memory": {"total": 8, "free": 3}})),
        ("GET", "/api2/json/nodes/pve/storage") => {
            data(json!([{"storage": "local", "content": "iso,snippets"}, {"storage": "local-lvm", "content": "images"}]))
        }
        ("GET", "/api2/json/nodes/pve/network") => data(json!([{"iface": "vmbr0", "bridge_ports": "eno1"}, {"iface": "vmbr1"}])),
        _ => (404, "{}".into()),
    }
}

/// The loopback port `https` serves on.
fn port(https: &https::Server) -> u16 {
    https.endpoint.rsplit(':').next().unwrap().parse().unwrap()
}

#[tokio::test]
async fn a_self_signed_server_gets_no_credential_until_its_certificate_is_pinned() {
    let https = https::Server::start(proxmox).await;
    let server = Server::new("127.0.0.1", port(&https));
    let password = Credentials::from_login("root@pam", "pw");
    match sign_in(&server, &password, Trust::Pinned(None)).await {
        Err(SignInError::Certificate(Refusal::Unconfirmed { presented })) => assert_eq!(presented, https::fingerprint()),
        other => panic!("{other:?}"),
    }
    let other = Fingerprint::parse(&"AB".repeat(32)).unwrap();
    match sign_in(&server, &Credentials::from_login("root@pam!t", "s"), Trust::Pinned(Some(other))).await {
        Err(SignInError::Certificate(Refusal::Changed { pinned, presented })) => assert_eq!((pinned, presented), (other, https::fingerprint())),
        other => panic!("{other:?}"),
    }
    assert_eq!(https.requests().len(), 0, "no credential left before the certificate was pinned");

    let pinned = Trust::Pinned(Some(https::fingerprint()));
    let session = sign_in(&server, &password, pinned).await.unwrap();
    let node = session.node(None).await.unwrap();
    assert_eq!(node, "pve");
    assert_eq!(
        session.capacity(&node).await.unwrap(),
        Capacity {
            cores: 4,
            mem_total: 8,
            mem_free: 3
        }
    );
    assert_eq!(check::check(&session, &node, &Needs::default()).await, []);
    assert!(!check::bridge_is_internal(&session, &node, "vmbr0").await.unwrap());
    assert!(check::bridge_is_internal(&session, &node, "vmbr1").await.unwrap());
    assert_eq!(session.get("/nowhere").await.unwrap_err(), CallError::Status(404));
    sign_in(&server, &Credentials::from_login("root@pam!t", "s"), pinned).await.unwrap();
}

#[tokio::test]
async fn refused_credentials_are_rejected() {
    let https = https::Server::start(|_: &Request| (401, "{}".into())).await;
    let server = Server::new("127.0.0.1", port(&https));
    let pinned = Trust::Pinned(Some(https::fingerprint()));
    for login in ["root@pam", "root@pam!t"] {
        assert_eq!(
            sign_in(&server, &Credentials::from_login(login, "x"), pinned).await.unwrap_err(),
            SignInError::Rejected
        );
    }
}

/// Against a real Proxmox VE server, given by `PLACE_PROXMOX_{HOST,PORT,LOGIN,SECRET,FINGERPRINT}`:
/// a pinned sign-in, its placement and capacity read, a wrong pin refused.
#[tokio::test]
#[ignore = "needs a Proxmox VE server"]
async fn a_real_server() {
    let var = |name: &str| std::env::var(format!("PLACE_PROXMOX_{name}")).unwrap();
    let server = Server::new(var("HOST"), var("PORT").parse().unwrap());
    let credentials = Credentials::from_login(&var("LOGIN"), &var("SECRET"));
    let pin = Fingerprint::parse(&var("FINGERPRINT")).unwrap();
    let wrong = Fingerprint::parse(&"00".repeat(32)).unwrap();
    let refused = sign_in(&server, &credentials, Trust::Pinned(Some(wrong))).await.unwrap_err();
    println!("wrong pin: {refused}");
    assert!(matches!(refused, SignInError::Certificate(Refusal::Changed { presented, .. }) if presented == pin));
    let session = sign_in(&server, &credentials, Trust::Pinned(Some(pin))).await.unwrap();
    let placement = placement::read(&session, &Needs::default(), server.host.parse().ok()).await.unwrap();
    println!("placement: {placement:#?}");
    let node = session.node(None).await.unwrap();
    println!("capacity of {node}: {:?}", session.capacity(&node).await.unwrap());
    println!("findings: {:?}", check::check(&session, &node, &Needs::default()).await);
}
