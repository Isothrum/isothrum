//! Where a Proxmox VE server can put machines: its nodes, and on each the storages for disks,
//! cloud images and cloud-init snippets, and the bridges (with their addresses), each with the one
//! proposed: the preferred name of [`Needs`] when the node has it, the bridge on the server's own
//! network when there is one.

use std::future::Future;
use std::net::IpAddr;

use serde::Serialize;

use super::api::{Iface, Node, Storage};
use super::check::Needs;
use super::{CallError, Session, read_data};

/// The choices a server offers.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    /// Each node, with its choices.
    pub nodes: Vec<NodeChoices>,
}

/// One node's storages and bridges, with the default of each (None when it has none).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct NodeChoices {
    /// The node's name.
    pub name: String,
    /// Storages that hold VM disks ('images' content).
    pub disk_storages: Vec<String>,
    /// Storages cloud images can be downloaded to ('iso' content).
    pub image_storages: Vec<String>,
    /// Storages that hold cloud-init snippets ('snippets' content).
    pub snippet_storages: Vec<String>,
    /// Its network bridges.
    pub bridges: Vec<Bridge>,
    /// The disk storage proposed (the preferred one, when the node has it); None when it has no such
    /// storage.
    pub default_disk: Option<String>,
    /// The image storage proposed (the preferred one, when the node has it); None when it has no
    /// such storage.
    pub default_image: Option<String>,
    /// The snippet storage proposed (the preferred one, when the node has it); None when it has
    /// no such storage.
    pub default_snippet: Option<String>,
    /// The bridge whose network holds the server's own address (the one this machine reaches),
    /// else the preferred one, else the first.
    pub default_bridge: Option<String>,
}

/// A bridge and its address, so a user sees which one is on their network.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Bridge {
    /// The bridge's name, e.g. `vmbr0`.
    pub name: String,
    /// e.g. `10.0.0.50/24`; None for a bridge with no address.
    pub cidr: Option<String>,
}

/// The choices of the server `session` is signed in to, the ones proposed preferring `preferred`;
/// `server_ip` is the server's address as this computer reaches it (to propose the bridge on its
/// network).
///
/// # Errors
///
/// A read failed, or the user can see no node ([`CallError::NoNode`]).
pub async fn read(session: &Session, preferred: &Needs, server_ip: Option<IpAddr>) -> Result<Placement, CallError> {
    read_answers(|path| async move { session.get(&path).await }, preferred, server_ip).await
}

/// The choices, from the answers' bodies `get` gives for a path (`/nodes`, `/nodes/<n>/storage`,
/// `/nodes/<n>/network?type=any_bridge`).
pub(crate) async fn read_answers<F, Fut>(get: F, preferred: &Needs, server_ip: Option<IpAddr>) -> Result<Placement, CallError>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<String, CallError>>,
{
    let mut nodes = Vec::new();
    for name in node_names(&read_data::<Vec<Node>>(&get("/nodes".into()).await?)?) {
        let storages: Vec<Storage> = read_data(&get(format!("/nodes/{name}/storage")).await?)?;
        let bridges = bridges(&read_data::<Vec<Iface>>(&get(format!("/nodes/{name}/network?type=any_bridge")).await?)?);
        nodes.push(node_choices(name, &storages, bridges, preferred, server_ip));
    }
    if nodes.is_empty() {
        return Err(CallError::NoNode);
    }
    Ok(Placement { nodes })
}

/// The nodes in `/nodes`, online ones first, by name.
pub(crate) fn node_names(nodes: &[Node]) -> Vec<String> {
    let mut list: Vec<(bool, String)> = nodes
        .iter()
        .filter_map(|n| Some((n.status.as_deref().is_some_and(|s| s != "online"), n.node.clone()?)))
        .collect();
    list.sort();
    list.into_iter().map(|(_, n)| n).collect()
}

/// A node's choices from its storages and bridges.
pub(crate) fn node_choices(name: String, storages: &[Storage], bridges: Vec<Bridge>, preferred: &Needs, server_ip: Option<IpAddr>) -> NodeChoices {
    let disk_storages = storages_with(storages, "images");
    let image_storages = storages_with(storages, "iso");
    let snippet_storages = storages_with(storages, "snippets");
    NodeChoices {
        default_disk: pick(&disk_storages, &preferred.disk_storage),
        default_image: pick(&image_storages, &preferred.image_storage),
        default_snippet: pick(&snippet_storages, &preferred.snippet_storage),
        default_bridge: default_bridge(&bridges, &preferred.bridge, server_ip),
        name,
        disk_storages,
        image_storages,
        snippet_storages,
        bridges,
    }
}

/// The usable storages (enabled and active, when the API says) that accept `content`, by name.
pub(crate) fn storages_with(storages: &[Storage], content: &str) -> Vec<String> {
    let mut names: Vec<String> = storages
        .iter()
        .filter(|s| s.usable() && s.contents().iter().any(|c| c == content))
        .filter_map(|s| s.id.clone())
        .collect();
    names.sort();
    names
}

/// `preferred` when it is among `names`, else the first.
fn pick(names: &[String], preferred: &str) -> Option<String> {
    names.iter().find(|n| *n == preferred).or_else(|| names.first()).cloned()
}

/// The bridges in `/nodes/<n>/network?type=any_bridge`, with their address, by name.
pub(crate) fn bridges(ifaces: &[Iface]) -> Vec<Bridge> {
    let mut list: Vec<Bridge> = ifaces
        .iter()
        .filter_map(|i| {
            let name = i.name.clone()?;
            let cidr = i.cidr.clone().or_else(|| {
                let (address, netmask) = (i.address.as_deref()?, i.netmask.as_deref()?);
                Some(format!("{address}/{netmask}"))
            });
            Some(Bridge { name, cidr })
        })
        .collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

/// The bridge whose network holds `server_ip`, else `preferred`, else the first.
pub(crate) fn default_bridge(bridges: &[Bridge], preferred: &str, server_ip: Option<IpAddr>) -> Option<String> {
    let on_lan = server_ip.and_then(|ip| bridges.iter().find(|b| b.cidr.as_deref().is_some_and(|c| cidr_contains(c, ip))));
    on_lan
        .or_else(|| bridges.iter().find(|b| b.name == preferred))
        .or_else(|| bridges.first())
        .map(|b| b.name.clone())
}

/// Whether `cidr` (`10.0.0.50/24`; a dotted netmask works too) holds `ip`.
pub(crate) fn cidr_contains(cidr: &str, ip: IpAddr) -> bool {
    let Some((address, len)) = cidr.split_once('/') else { return false };
    let Ok(address) = address.trim().parse::<IpAddr>() else { return false };
    let len = len
        .trim()
        .parse::<u32>()
        .ok()
        .or_else(|| len.trim().parse::<std::net::Ipv4Addr>().ok().map(|m| u32::from(m).count_ones()));
    let Some(len) = len else { return false };
    match (address, ip) {
        (IpAddr::V4(a), IpAddr::V4(b)) if len <= 32 => {
            let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
            u32::from(a) & mask == u32::from(b) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(b)) if len <= 128 => {
            let mask = if len == 0 { 0 } else { u128::MAX << (128 - len) };
            u128::from(a) & mask == u128::from(b) & mask
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// `json` as the `data` of an answer the API's way.
    fn body(json: &Value) -> String {
        json!({ "data": json }).to_string()
    }

    /// `json`, a list of what a path answers, read.
    fn listed<T: serde::de::DeserializeOwned>(json: Value) -> Vec<T> {
        serde_json::from_value(json).unwrap()
    }

    /// A server as its API answers: one node, `local` and `local-lvm`,
    /// `vmbr0` on 192.168.150.0/24 and `vmbr1` on the network this computer reaches it on.
    fn answer(path: &str) -> std::future::Ready<Result<String, CallError>> {
        std::future::ready(Ok(body(&match path {
            "/nodes" => json!([{ "node": "pve", "status": "online" }]),
            "/nodes/pve/storage" => json!([
                { "storage": "local", "type": "dir", "content": "images,iso,snippets,vztmpl,rootdir,backup", "enabled": 1, "active": 1 },
                { "storage": "local-lvm", "type": "lvmthin", "content": "images,rootdir", "enabled": 1, "active": 1 }
            ]),
            "/nodes/pve/network?type=any_bridge" => json!([
                { "iface": "vmbr1", "type": "bridge", "cidr": "10.0.0.50/24" },
                { "iface": "vmbr0", "type": "bridge", "cidr": "192.168.150.10/24" }
            ]),
            other => panic!("unexpected API read {other}"),
        })))
    }

    #[tokio::test]
    async fn the_servers_own_options_with_their_defaults() {
        let ip = "10.0.0.50".parse().ok();
        let p = read_answers(|p: String| answer(&p), &Needs::default(), ip).await.unwrap();
        assert_eq!(
            p,
            Placement {
                nodes: vec![NodeChoices {
                    name: "pve".into(),
                    disk_storages: vec!["local".into(), "local-lvm".into()],
                    image_storages: vec!["local".into()],
                    snippet_storages: vec!["local".into()],
                    bridges: vec![
                        Bridge {
                            name: "vmbr0".into(),
                            cidr: Some("192.168.150.10/24".into())
                        },
                        Bridge {
                            name: "vmbr1".into(),
                            cidr: Some("10.0.0.50/24".into())
                        }
                    ],
                    default_disk: Some("local-lvm".into()),
                    default_image: Some("local".into()),
                    default_snippet: Some("local".into()),
                    default_bridge: Some("vmbr1".into()),
                }]
            }
        );
        // An address on no bridge's network (a hostname not resolved, a NAT'd one): vmbr0.
        assert_eq!(
            read_answers(|p: String| answer(&p), &Needs::default(), "172.16.0.9".parse().ok())
                .await
                .unwrap()
                .nodes[0]
                .default_bridge
                .as_deref(),
            Some("vmbr0")
        );
        assert_eq!(
            read_answers(|p: String| answer(&p), &Needs::default(), None).await.unwrap().nodes[0]
                .default_bridge
                .as_deref(),
            Some("vmbr0")
        );
    }

    #[tokio::test]
    async fn a_failed_read_is_the_error_and_no_node_is_one() {
        let denied = |_: String| std::future::ready(Err::<String, _>(CallError::Status(403)));
        assert!(matches!(read_answers(denied, &Needs::default(), None).await, Err(CallError::Status(403))));
        let none = |_: String| std::future::ready(Ok::<_, CallError>(body(&json!([]))));
        assert_eq!(
            read_answers(none, &Needs::default(), None).await.unwrap_err().to_string(),
            "this user can't see any node"
        );
    }

    #[test]
    fn nodes_online_first_and_storages_that_can_be_used() {
        let nodes: Vec<Node> = listed(json!([{ "node": "b", "status": "offline" }, { "node": "c", "status": "online" }, { "node": "a", "status": "online" }]));
        assert_eq!(node_names(&nodes), ["a", "c", "b"]);
        let storages: Vec<Storage> = listed(json!([
            { "storage": "nfs", "content": "images", "enabled": 1, "active": 0 },
            { "storage": "off", "content": "images", "enabled": 0 },
            { "storage": "ceph", "content": "rootdir, images" },
            { "storage": "isos", "content": "iso,vztmpl" }
        ]));
        assert_eq!(storages_with(&storages, "images"), ["ceph"]);
        assert_eq!(storages_with(&storages, "iso"), ["isos"]);
        let c = node_choices("n".into(), &storages, vec![], &Needs::default(), None);
        assert_eq!(
            (c.default_disk.as_deref(), c.default_image.as_deref(), c.default_snippet, c.default_bridge),
            (Some("ceph"), Some("isos"), None, None)
        );
    }

    #[test]
    fn bridges_show_their_address_and_networks_hold_addresses() {
        let ifaces: Vec<Iface> = listed(json!([{ "iface": "vmbr2" }, { "iface": "vmbr0", "address": "192.168.1.5", "netmask": "255.255.255.0" }]));
        assert_eq!(
            bridges(&ifaces),
            [
                Bridge {
                    name: "vmbr0".into(),
                    cidr: Some("192.168.1.5/255.255.255.0".into())
                },
                Bridge {
                    name: "vmbr2".into(),
                    cidr: None
                }
            ]
        );
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert!(cidr_contains("10.0.0.50/24", ip("10.0.0.7")));
        assert!(!cidr_contains("10.0.0.50/24", ip("10.0.1.7")));
        assert!(cidr_contains("192.168.1.5/255.255.255.0", ip("192.168.1.200")));
        assert!(cidr_contains("fd00::1/64", ip("fd00::9")));
        assert!(!cidr_contains("fd00::1/64", ip("10.0.0.7")));
        assert!(!cidr_contains("nonsense", ip("10.0.0.7")));
        assert!(!cidr_contains("10.0.0.1/40", ip("10.0.0.7")));
    }
}
