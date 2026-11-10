//! LANs: each one drawn is a network with its own block, and each machine on it gets its address
//! there, the one it asks for or the next free one.

use indexmap::IndexMap;
use isoloom_spec::validate::Cidr;

use crate::cables::is_link_network;
use crate::document::NetworkDoc;
use crate::draft::Draft;
use crate::{Error, Lan, Result, Topology};

/// The last octets a machine may take on `block`, lowest first: those Isoloom gives machines
/// ([`Cidr::host`]: not the network address, the gateway, the two last usable addresses (its
/// controller and router) or the broadcast). A /24 starts at .10, leaving room for the
/// infrastructure people expect low in a LAN.
fn usable(block: Cidr) -> impl Iterator<Item = u8> {
    let floor = if block.len == 24 { 10 } else { 0 };
    (floor..=u8::MAX).filter(move |octet| block.host(*octet).is_some())
}

/// A LAN's block: one Isoloom gives machines addresses on (its network's own address, a /24 to a
/// /29).
fn block_of(name: &str, lan: &Lan) -> Result<Cidr> {
    Cidr::parse(&lan.cidr)
        .filter(|block| usable(*block).next().is_some())
        .ok_or_else(|| Error::BadCidr {
            lan: name.into(),
            cidr: lan.cidr.clone(),
        })
}

/// Each LAN becomes a network, and each machine on it gets its address there.
pub(crate) fn address(topo: &Topology, draft: &mut Draft) -> Result<()> {
    for (name, lan) in &topo.networks {
        address_one(topo, draft, name, lan)?;
    }
    check_known(topo)
}

fn address_one(topo: &Topology, draft: &mut Draft, name: &str, lan: &Lan) -> Result<()> {
    if is_link_network(name) {
        return Err(Error::ReservedName(name.into()));
    }
    let block = block_of(name, lan)?;
    draft
        .networks
        .insert(name.into(), NetworkDoc::new(lan.cidr.clone(), lan.internet, lan.tc.clone()));
    let taken = asked_addresses(topo, draft, name)?;
    let mut free = usable(block).filter(|o| !taken.contains_key(o));
    for (node, _) in members(topo, draft, name).into_iter().filter(|(_, octet)| octet.is_none()) {
        let octet = free.next().ok_or_else(|| Error::LanFull {
            lan: name.into(),
            cidr: lan.cidr.clone(),
        })?;
        draft.attach(node, name, octet);
    }
    Ok(())
}

/// The machines on `lan` (automatic routers aside), in the topology's order, each with the
/// octet it asks for there.
fn members<'t>(topo: &'t Topology, draft: &Draft, lan: &str) -> Vec<(&'t str, Option<u8>)> {
    topo.nodes
        .iter()
        .filter(|(node, _)| !draft.is_router(node))
        .filter_map(|(node, n)| n.networks.get(lan).map(|octet| (node.as_str(), *octet)))
        .collect()
}

/// The addresses machines ask for on `lan`, given to them first: octet → machine.
fn asked_addresses(topo: &Topology, draft: &mut Draft, lan: &str) -> Result<IndexMap<u8, String>> {
    let asked: Vec<(&str, u8)> = members(topo, draft, lan).into_iter().filter_map(|(node, octet)| Some((node, octet?))).collect();
    let mut taken: IndexMap<u8, String> = IndexMap::new();
    for (node, octet) in asked {
        if let Some(first) = taken.insert(octet, node.to_owned()) {
            return Err(Error::AddressTaken {
                lan: lan.into(),
                octet,
                first,
                second: node.to_owned(),
            });
        }
        draft.attach(node, lan, octet);
    }
    Ok(taken)
}

/// Refuses a node on a LAN the project doesn't have.
fn check_known(topo: &Topology) -> Result<()> {
    for (node, n) in &topo.nodes {
        if let Some(lan) = n.networks.keys().find(|l| !topo.networks.contains_key(*l)) {
            return Err(Error::UnknownLan {
                node: node.clone(),
                lan: lan.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usable_in(cidr: &str) -> Vec<u8> {
        Cidr::parse(cidr).map(|block| usable(block).collect()).unwrap_or_default()
    }

    fn range(octets: std::ops::RangeInclusive<u8>) -> Vec<u8> {
        octets.collect()
    }

    #[test]
    fn a_block_offers_what_isoloom_leaves_to_machines() {
        assert_eq!(usable_in("10.10.1.0/24"), range(10..=252));
        assert_eq!(usable_in("10.10.1.0/25"), range(2..=124));
        assert_eq!(usable_in("10.10.1.0/28"), range(2..=12));
        assert_eq!(usable_in("10.10.1.0/29"), range(2..=4));
    }

    #[test]
    fn a_block_not_at_the_start_of_its_octet_offers_its_own_addresses() {
        assert_eq!(usable_in("10.10.1.8/29"), range(10..=12));
        assert_eq!(usable_in("10.10.1.128/25"), range(130..=252));
        assert_eq!(usable_in("10.10.1.240/28"), range(242..=252));
    }

    #[test]
    fn a_block_isoloom_does_not_address_offers_nothing() {
        for size in ["10.10.0.0/16", "10.10.1.0/23", "10.10.1.0/30", "10.10.1.0/32"] {
            assert_eq!(usable_in(size), Vec::<u8>::new(), "{size}");
        }
    }
}
