//! A state as `show -json` gives it: its managed instances (with what a VM's provider writes of
//! its node, id, addresses and OS) and its data sources.

use super::{BTreeMap, Deserialize, DeserializeOwned};

/// One resource instance of the state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resource {
    /// Its address (`module.web.aws_instance.vm`).
    pub(crate) address: String,
    /// Its type (`aws_instance`).
    pub(crate) kind: String,
    /// Its provider's id for it, when it has one.
    pub(crate) id: Option<String>,
    /// A create or a provisioner of it failed: the next apply makes it again.
    pub(crate) tainted: bool,
    /// What its attributes say of the VM it is, when it is one.
    pub(crate) vm: VmFacts,
}

/// What a VM resource's attributes say of it, read by the names the providers give them; each
/// none where its provider writes no such attribute.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct VmFacts {
    /// The Proxmox VE node it runs on (`node_name`, `target_node`).
    pub(crate) node: Option<String>,
    /// Its id on its Proxmox VE cluster (`vm_id`, `vmid`).
    pub(crate) vm_id: Option<String>,
    /// Its address, as its provider writes it ([`ADDRESSES`], then the addresses its network
    /// interfaces list), loopback and link-local ones left out.
    pub(crate) address: Option<String>,
    /// Its address on the module's own networks, as its provider writes it ([`PRIVATE_ADDRESSES`],
    /// then its interfaces' private ones): what a machine beside it reaches it at. None where the
    /// provider writes only one address (ESXi's, libvirt's: [`VmFacts::address`] is that one).
    pub(crate) private_address: Option<String>,
    /// Its attributes say it runs Windows: its type (Azure's Windows VM), its Windows settings
    /// (`os_profile_windows_config`), its guest OS (ESXi's `guestos`, vSphere's `guest_id`,
    /// Telmate's `qemu_os`, bpg's `operating_system`), or, for an AWS instance, the AMI it was made
    /// from, which a data source of the module says is a Windows one ([`Shown`]).
    pub(crate) windows: bool,
    /// The image it was made from, when its provider names it by id (AWS's `ami`).
    pub(crate) image: Option<String>,
}

/// The attributes a VM's provider writes its address on the module's own networks in: AWS's and
/// OCI's, Azure's, Linode's, DigitalOcean's, Hetzner's.
const PRIVATE_ADDRESSES: [&str; 4] = ["private_ip", "private_ip_address", "ipv4_address_private", "private_ipv4"];

/// The resource types that are always a Windows VM.
const WINDOWS_TYPES: [&str; 1] = ["azurerm_windows_virtual_machine"];

/// The attributes a provider writes a VM's guest OS in, as text: ESXi's, vSphere's, Telmate's.
const GUEST_OS: [&str; 3] = ["guestos", "guest_id", "qemu_os"];

/// The attributes a VM's provider writes its address in, best first: Telmate/proxmox's, ESXi's
/// and Linode's, vSphere's, Hetzner's and DigitalOcean's, the clouds' public ones (AWS, OCI,
/// Scaleway, Azure, Vultr, `OpenStack`), then the private ones and Telmate's SSH host.
const ADDRESSES: [&str; 11] = [
    "default_ipv4_address",
    "ip_address",
    "default_ip_address",
    "ipv4_address",
    "public_ip",
    "public_ip_address",
    "main_ip",
    "access_ip_v4",
    "private_ip",
    "private_ip_address",
    "ssh_host",
];

/// A network interface as libvirt's and Google's providers write it.
#[derive(Deserialize)]
struct Interface {
    /// libvirt's.
    #[serde(default)]
    addresses: Vec<String>,
    /// Google's private address.
    #[serde(default)]
    network_ip: Option<String>,
    /// Google's public addresses.
    #[serde(default)]
    access_config: Vec<AccessConfig>,
}

/// Google's public address of an interface.
#[derive(Deserialize)]
struct AccessConfig {
    #[serde(default)]
    nat_ip: Option<String>,
}

/// `ipv4_addresses`: bpg/proxmox's, by interface; Xen Orchestra's, flat.
#[derive(Deserialize)]
#[serde(untagged)]
enum Ipv4Addresses {
    ByInterface(Vec<Vec<String>>),
    Flat(Vec<String>),
}

/// Whether `address` is one another computer reaches the VM at: an IP address that isn't
/// loopback, link-local or unspecified.
fn reachable_address(address: &str) -> bool {
    match address.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => !(ip.is_loopback() || ip.is_link_local() || ip.is_unspecified()),
        Ok(std::net::IpAddr::V6(ip)) => !(ip.is_loopback() || ip.is_unspecified() || (ip.segments()[0] & 0xffc0) == 0xfe80),
        Err(_) => false,
    }
}

/// What `show -json` says of the state: its managed resources (what the run made, what may cost)
/// apart from its data sources (what it only read).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Shown {
    /// `mode == "managed"`.
    pub(crate) managed: Vec<Resource>,
    /// `mode == "data"`: their addresses.
    pub(crate) data: Vec<String>,
}

#[derive(Deserialize)]
struct ShowDoc {
    #[serde(default)]
    values: Option<Values>,
}

#[derive(Deserialize)]
struct Values {
    #[serde(default)]
    root_module: Module,
}

#[derive(Default, Deserialize)]
struct Module {
    #[serde(default)]
    resources: Vec<ResourceDoc>,
    #[serde(default)]
    child_modules: Vec<Module>,
}

#[derive(Deserialize)]
struct ResourceDoc {
    address: String,
    mode: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    values: Option<Attributes>,
    #[serde(default)]
    tainted: bool,
}

/// The attributes of a resource the runner reads: its id, and what a VM's say of it.
#[derive(Deserialize)]
struct Attributes {
    #[serde(default)]
    id: Option<serde_json::Value>,
    /// bpg/proxmox's node.
    #[serde(default)]
    node_name: Option<serde_json::Value>,
    /// Telmate/proxmox's node.
    #[serde(default)]
    target_node: Option<serde_json::Value>,
    /// bpg/proxmox's VM id.
    #[serde(default)]
    vm_id: Option<serde_json::Value>,
    /// Telmate/proxmox's VM id.
    #[serde(default)]
    vmid: Option<serde_json::Value>,
    /// Every other attribute, by name: where a VM's address is read from. Each is read in its own
    /// shape where a provider writes one, and skipped when it isn't in it, so no provider's own
    /// attribute of the same name makes the state unreadable.
    #[serde(flatten)]
    rest: BTreeMap<String, serde_json::Value>,
}

impl Attributes {
    /// What they say of the VM, a resource of type `kind`.
    fn vm(&self, kind: &str) -> VmFacts {
        VmFacts {
            node: scalar(self.node_name.as_ref()).or_else(|| scalar(self.target_node.as_ref())),
            vm_id: scalar(self.vm_id.as_ref()).or_else(|| scalar(self.vmid.as_ref())),
            address: self.address(),
            private_address: self.private_address(),
            windows: WINDOWS_TYPES.contains(&kind) || self.says_windows(),
            image: scalar(self.rest.get("ami")),
        }
    }

    /// Whether its settings or its guest OS say Windows.
    fn says_windows(&self) -> bool {
        #[derive(Deserialize)]
        struct Os {
            #[serde(default, rename = "type")]
            kind: Option<String>,
        }
        let windows = |os: &str| os.trim().to_ascii_lowercase().starts_with("win");
        let settings = self
            .shaped::<Vec<serde_json::Value>>("os_profile_windows_config")
            .is_some_and(|blocks| !blocks.is_empty());
        let guest = GUEST_OS.iter().filter_map(|name| scalar(self.rest.get(*name))).any(|os| windows(&os));
        let typed = self
            .shaped::<Vec<Os>>("operating_system")
            .unwrap_or_default()
            .iter()
            .filter_map(|os| os.kind.as_deref())
            .any(windows);
        settings || guest || typed
    }

    /// Whether this data source is an AWS image whose platform is Windows (`aws_ami`'s
    /// `platform`).
    fn windows_image(&self) -> bool {
        scalar(self.rest.get("platform")).is_some_and(|p| p.eq_ignore_ascii_case("windows"))
    }

    /// The VM's address on the module's own networks: the first of [`PRIVATE_ADDRESSES`] that is
    /// one, else the first private one its interfaces list (Google's `network_ip`).
    fn private_address(&self) -> Option<String> {
        let named = PRIVATE_ADDRESSES.iter().filter_map(|name| scalar(self.rest.get(*name)));
        let interfaces: Vec<Interface> = self.shaped("network_interface").unwrap_or_default();
        named.chain(interfaces.into_iter().filter_map(|i| i.network_ip)).find(|a| reachable_address(a))
    }

    /// Attribute `name` in the shape `T`; none when it isn't there or isn't in that shape.
    fn shaped<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        self.rest.get(name).cloned().and_then(|v| serde_json::from_value(v).ok())
    }

    /// The VM's address: the first of [`ADDRESSES`] that is one, else the first its interfaces
    /// list.
    fn address(&self) -> Option<String> {
        let named = ADDRESSES.iter().filter_map(|name| scalar(self.rest.get(*name)));
        let by_interface: Vec<String> = match self.shaped("ipv4_addresses") {
            Some(Ipv4Addresses::ByInterface(lists)) => lists.into_iter().flatten().collect(),
            Some(Ipv4Addresses::Flat(list)) => list,
            None => Vec::new(),
        };
        let interfaces: Vec<Interface> = self.shaped("network_interface").unwrap_or_default();
        let listed = interfaces.into_iter().flat_map(|i| {
            let public = i.access_config.into_iter().filter_map(|a| a.nat_ip);
            public.chain(i.network_ip).chain(i.addresses).collect::<Vec<_>>()
        });
        named.chain(by_interface).chain(listed).find(|a| reachable_address(a))
    }
}

/// An attribute that is a string or a number, as text; none for anything else, or an empty one.
fn scalar(value: Option<&serde_json::Value>) -> Option<String> {
    match value? {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// `show -json`'s state, read: an empty state (no `values`) has nothing; a mode other than
/// `managed` or `data` is unexpected, never skipped.
pub(crate) fn shown(json: &str) -> Result<Shown, String> {
    let doc: ShowDoc = serde_json::from_str(json).map_err(|e| format!("`show -json` isn't the state's JSON: {e}"))?;
    let mut out = Shown::default();
    let mut windows_images = Vec::new();
    let mut pending = vec![doc.values.map(|v| v.root_module).unwrap_or_default()];
    while let Some(module) = pending.pop() {
        for r in module.resources {
            match r.mode.as_str() {
                "managed" => {
                    let id = r.values.as_ref().and_then(|v| match v.id.as_ref()? {
                        serde_json::Value::String(s) => Some(s.clone()),
                        serde_json::Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    });
                    let vm = r.values.as_ref().map(|v| v.vm(&r.kind)).unwrap_or_default();
                    out.managed.push(Resource {
                        address: r.address,
                        kind: r.kind,
                        id,
                        tainted: r.tainted,
                        vm,
                    });
                }
                "data" => {
                    if let Some(id) = r
                        .values
                        .as_ref()
                        .filter(|v| r.kind == "aws_ami" && v.windows_image())
                        .and_then(|v| scalar(v.id.as_ref()))
                    {
                        windows_images.push(id);
                    }
                    out.data.push(r.address);
                }
                other => return Err(format!("resource `{}` of mode `{other}`", r.address)),
            }
        }
        pending.extend(module.child_modules);
    }
    // An instance made from an image a data source says is Windows' runs Windows.
    for r in &mut out.managed {
        if r.vm.image.as_ref().is_some_and(|ami| windows_images.contains(ami)) {
            r.vm.windows = true;
        }
    }
    Ok(out)
}
