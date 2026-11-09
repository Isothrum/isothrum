//! Cables: each one between two machines' ports is a point-to-point network of its own, a /29
//! cut from `10.200.0.0/16`, the two ends on its `.2` and `.3` (its `.1` left free).

use std::net::Ipv4Addr;

use isoloom_spec::validate::Cidr;

use crate::document::NetworkDoc;
use crate::draft::Draft;
use crate::{Error, Link, Result, Topology};

/// The block the cables are cut from, `10.200.0.0/16`: its first two octets. Isoloom takes a /24
/// to /29 per network, so a cable's is the smallest, a /29 with two ends (.2 and .3).
const LINK_BASE: [u8; 2] = [10, 200];
/// The block the cables are cut from, as the errors say it.
pub(crate) const LINK_BLOCK: &str = "10.200.0.0/16";
/// A /24 holds 32 /29 blocks.
const LINKS_PER_THIRD: usize = 32;
/// A /16 holds 256 such /24s.
const MAX_LINKS: usize = 256 * LINKS_PER_THIRD;

/// The network name of the `i`th cable.
fn link_network(i: usize) -> String {
    format!("link-{i}")
}

/// Whether `name` is one the cables' networks take (`link-<n>`), which a LAN can't have.
pub(crate) fn is_link_network(name: &str) -> bool {
    name.starts_with("link-")
}

/// The /29 of the `i`th cable: its network address and the last octets of its two ends; none past
/// the block's last.
fn link_block(i: usize) -> Option<(String, u8, u8)> {
    let third = u8::try_from(i / LINKS_PER_THIRD).ok()?;
    let fourth = u8::try_from((i % LINKS_PER_THIRD) * 8).ok()?;
    let [a, b] = LINK_BASE;
    let block = Cidr {
        base: u32::from(Ipv4Addr::new(a, b, third, fourth)),
        len: 29,
    };
    Some((block.to_string(), fourth + 2, fourth + 3))
}

/// Refuses more cables than the cables' block holds.
pub(crate) fn check_count(topo: &Topology) -> Result<()> {
    match topo.links.len() {
        n if n > MAX_LINKS => Err(Error::TooManyLinks(n)),
        _ => Ok(()),
    }
}

/// Each cable becomes its own /29 network, its two ends on it.
pub(crate) fn allocate(topo: &Topology, draft: &mut Draft) -> Result<()> {
    let mut used_ports: Vec<(&str, &str)> = Vec::new();
    // `check_count` refused more cables than there are blocks.
    let blocks = (0..).map_while(link_block);
    for (i, (link, block)) in topo.links.iter().zip(blocks).enumerate() {
        check(topo, i, link, &mut used_ports)?;
        cable(draft, i, link, block);
    }
    Ok(())
}

/// Refuses a cable to an unknown node, on a port already cabled, or from a node to itself.
fn check<'a>(topo: &Topology, i: usize, link: &'a Link, used_ports: &mut Vec<(&'a str, &'a str)>) -> Result<()> {
    for end in [&link.a, &link.b] {
        if !topo.nodes.contains_key(&end.node) {
            return Err(Error::UnknownNode {
                link: i,
                node: end.node.clone(),
            });
        }
        if used_ports.contains(&(end.node.as_str(), end.port.as_str())) {
            return Err(Error::PortInUse {
                node: end.node.clone(),
                port: end.port.clone(),
            });
        }
        used_ports.push((end.node.as_str(), end.port.as_str()));
    }
    if link.a.node == link.b.node {
        return Err(Error::SelfLink {
            link: i,
            node: link.a.node.clone(),
        });
    }
    Ok(())
}

/// The `i`th cable's network, on `block`; an automatic router at an end joins it, a machine gets
/// its address.
fn cable(draft: &mut Draft, i: usize, link: &Link, (cidr, a_octet, b_octet): (String, u8, u8)) {
    // A cable between two automatic routers carries nothing of its own.
    if draft.is_router(&link.a.node) && draft.is_router(&link.b.node) {
        return;
    }
    let net = link_network(i);
    draft.networks.insert(net.clone(), NetworkDoc::new(cidr, false, link.tc.clone()));
    draft.attach(&link.a.node, &net, a_octet);
    draft.attach(&link.b.node, &net, b_octet);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_cable_takes_the_last_block() {
        assert_eq!(link_block(MAX_LINKS - 1), Some(("10.200.255.248/29".to_owned(), 250, 251)));
        assert_eq!(link_block(MAX_LINKS), None);
    }

    #[test]
    fn the_block_holds_8192_cables() {
        let end = |port: &str| crate::Endpoint {
            node: "a".into(),
            port: port.into(),
        };
        let mut topo = Topology {
            name: "many".into(),
            nodes: indexmap::IndexMap::new(),
            networks: indexmap::IndexMap::new(),
            links: vec![
                Link {
                    a: end("x"),
                    b: end("y"),
                    tc: None
                };
                MAX_LINKS
            ],
        };
        assert!(check_count(&topo).is_ok());
        topo.links.push(Link {
            a: end("x"),
            b: end("y"),
            tc: None,
        });
        assert!(matches!(check_count(&topo), Err(Error::TooManyLinks(8193))));
    }

    #[test]
    fn the_blocks_run_through_the_whole_16_in_order() {
        let blocks: Vec<_> = (0..).map_while(link_block).collect();
        assert_eq!(blocks.len(), MAX_LINKS);
        assert_eq!(blocks.first(), Some(&("10.200.0.0/29".to_owned(), 2, 3)));
        assert_eq!(blocks.get(32), Some(&("10.200.1.0/29".to_owned(), 2, 3)));
    }

    #[test]
    fn the_cables_block_is_said_as_its_first_octets_make_it() {
        let [a, b] = LINK_BASE;
        assert_eq!(LINK_BLOCK, format!("{a}.{b}.0.0/16"));
    }

    #[test]
    fn a_name_starting_with_link_is_the_cables() {
        assert!(is_link_network("link-0") && is_link_network("link-x"));
        assert!(!is_link_network("office") && !is_link_network("uplink-1"));
    }
}
