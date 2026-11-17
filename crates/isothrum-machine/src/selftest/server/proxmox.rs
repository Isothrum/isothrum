//! The test VM on a Proxmox VE server: the module `proxmox.tf`, its values from [`Proxmox`], and
//! its steps read from Terraform's lines (`<address>: Creation complete …`). A resource's address
//! is matched without its provider's prefix: the runner masks a secret's value in every line, and
//! a password or token that holds `proxmox` would mask that prefix.

use std::path::PathBuf;

use isothrum_runner::{ErrorReport, RunOptions, Runner, SecretValue, Secrets};

use super::{Course, Failure, Step, TestVm};

/// The module, written as the project's `main.tf`.
const MODULE: &str = include_str!("proxmox.tf");
/// The VM's machine name in the run (its resource's name).
const MACHINE: &str = "selftest_vm";
/// What of each watched resource's address survives a masked provider prefix.
const IMAGE: &str = "_download_file.selftest_image";
const NETWORK: &str = "_sdn_applier.selftest_net";
const VM: &str = "_virtual_environment_vm.selftest_vm";
/// The start of the module's own error when the VM gets no address.
const NO_ADDRESS: &str = "got no IPv4 address on bridge";
/// The API's port when the endpoint names none.
const API_PORT: u16 = 8006;

/// How the test signs in to the server's API.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SignIn {
    /// An API token, `user@realm!name=secret`.
    Token(SecretValue),
    /// A user (`root@pam`) and password; the password also signs in to the node over SSH when no
    /// key is given for it.
    Password {
        /// The user.
        user: String,
        /// The password.
        password: SecretValue,
    },
}

/// The key pair the test VM lets in and the test signs in with (and a jump host too).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Key {
    /// The private key's file.
    pub private_file: PathBuf,
    /// The public key, as `authorized_keys` takes it.
    pub public: String,
}

impl Key {
    /// The key pair whose private half is in `private_file`, public half `public`.
    #[must_use]
    pub fn new(private_file: impl Into<PathBuf>, public: impl Into<String>) -> Key {
        Key {
            private_file: private_file.into(),
            public: public.into(),
        }
    }
}

/// A Proxmox VE server, where on it the test VM goes, and how this computer reaches it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Proxmox {
    /// The API, `https://<server>:8006/`.
    pub endpoint: String,
    /// Accept the server's certificate unchecked.
    pub insecure: bool,
    /// How the test signs in to the API.
    pub sign_in: SignIn,
    /// The node the VM goes on.
    pub node: String,
    /// The bridge its first card goes on, which must give it an address (DHCP).
    pub bridge: String,
    /// Where its disk goes (the module's default: `local-lvm`).
    pub datastore: Option<String>,
    /// A datastore with `iso` content, for the cloud image (default: `local`).
    pub image_datastore: Option<String>,
    /// A datastore with `snippets` content, for cloud-init (default: `local`).
    pub snippets_datastore: Option<String>,
    /// Who the snippet's upload signs in to the node as over SSH (default: `root`).
    pub node_ssh_user: Option<String>,
    /// The key that upload signs in with; without one, the password of [`SignIn::Password`].
    pub node_ssh_key_file: Option<PathBuf>,
    /// The node's SSH address, when the API reports one this computer can't reach.
    pub node_address: Option<String>,
    /// The key the VM lets in; without one, the test runner's own.
    pub key: Option<Key>,
    /// `user@host` every SSH to the VM goes through first, when only that host reaches the
    /// bridge (its node, when the bridge is internal to it).
    pub jump: Option<String>,
}

impl Proxmox {
    /// The server whose API is `endpoint`, signed in to with `sign_in`, the VM on `node`'s
    /// `vmbr0`, every other setting the module's default.
    #[must_use]
    pub fn new(endpoint: impl Into<String>, sign_in: SignIn, node: impl Into<String>) -> Proxmox {
        Proxmox {
            endpoint: endpoint.into(),
            insecure: false,
            sign_in,
            node: node.into(),
            bridge: "vmbr0".to_owned(),
            datastore: None,
            image_datastore: None,
            snippets_datastore: None,
            node_ssh_user: None,
            node_ssh_key_file: None,
            node_address: None,
            key: None,
            jump: None,
        }
    }

    /// The API's `host:port`, from its endpoint (port 8006 when it names none).
    pub(super) fn api_address(&self) -> String {
        let rest = self.endpoint.split_once("://").map_or(self.endpoint.as_str(), |(_, r)| r);
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
        let has_port = match authority.rsplit_once(']') {
            Some((_, after)) => after.starts_with(':'),
            None => authority.contains(':'),
        };
        if has_port { authority.to_owned() } else { format!("{authority}:{API_PORT}") }
    }

    /// The run's options for VM `vm`, its key's public half `public`.
    fn options(&self, vm: &TestVm<'_>, public: &str) -> RunOptions {
        let (zone, vnet) = network_ids(vm.name);
        let mut secrets = Secrets::new();
        let mut opts = RunOptions::new()
            .with_env("endpoint", self.endpoint.clone())
            .with_env("insecure", self.insecure.to_string())
            .with_env("node", self.node.clone())
            .with_env("bridge", self.bridge.clone())
            .with_env("name", vm.name)
            .with_env("hostname", vm.hostname)
            .with_env("zone", zone)
            .with_env("vnet", vnet)
            .with_env("ssh_public_key", public.trim());
        match &self.sign_in {
            SignIn::Token(token) => secrets.insert("api_token", token.clone()),
            SignIn::Password { user, password } => {
                opts = opts.with_env("username", user.clone());
                secrets.insert("password", password.clone());
            }
        }
        let optional = [
            ("datastore", self.datastore.clone()),
            ("image_datastore", self.image_datastore.clone()),
            ("snippets_datastore", self.snippets_datastore.clone()),
            ("node_ssh_user", self.node_ssh_user.clone()),
            ("node_ssh_key_file", self.node_ssh_key_file.as_ref().map(|f| f.to_string_lossy().into_owned())),
            ("node_address", self.node_address.clone()),
            // The run's own: what its commands sign in with, and go through first.
            ("ssh_private_key_file", self.key.as_ref().map(|k| k.private_file.to_string_lossy().into_owned())),
            ("ssh_jump", self.jump.clone()),
        ];
        for (name, value) in optional {
            if let Some(value) = value.filter(|v| !v.is_empty()) {
                opts = opts.with_env(name, value);
            }
        }
        opts.with_secrets(secrets)
    }
}

/// The project: its file and text.
pub(super) fn project() -> (&'static str, String) {
    ("main.tf", MODULE.to_owned())
}

/// The run's options and course on `p`, the VM `vm`, once `runner` is there: its key, else the
/// runner's own.
pub(super) async fn prepared(p: &Proxmox, vm: &TestVm<'_>, runner: &Runner) -> Result<(RunOptions, Pve), String> {
    let public = match &p.key {
        Some(key) => key.public.clone(),
        None => runner.ssh_public_key().await.map_err(|e| e.to_string())?,
    };
    Ok((p.options(vm, &public), Pve))
}

/// The SDN zone's and VNet's ids of the test VM named `name` (eight characters at most, as
/// Proxmox VE takes them): the same at every test of that VM, so an earlier one's are found.
fn network_ids(name: &str) -> (String, String) {
    // FNV-1a: stable across builds and platforms.
    let hash = name.bytes().fold(0x811c_9dc5_u32, |h, b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193)) & 0x00ff_ffff;
    (format!("st{hash:06x}"), format!("sn{hash:06x}"))
}

/// How the test VM comes up on Proxmox VE: the image downloaded, the network applied, then the
/// VM created, which ends once its guest agent reports its address.
pub(super) struct Pve;

impl Course for Pve {
    const UP: &'static [Step] = &[Step::Image, Step::Network, Step::Vm, Step::Address];

    fn machine(&self) -> &'static str {
        MACHINE
    }

    fn ends(&mut self, line: &str) -> Option<usize> {
        let (address, _) = line.split_once(": Creation complete")?;
        if address.ends_with(IMAGE) {
            Some(0)
        } else if address.ends_with(NETWORK) {
            Some(1)
        } else if address.ends_with(VM) {
            Some(3)
        } else {
            None
        }
    }

    fn explain(&self, report: &ErrorReport) -> Option<(usize, Failure)> {
        let said = &report.message;
        let boxed = || Box::new(report.clone());
        if said.contains(NO_ADDRESS) {
            Some((3, Failure::NoAddress(boxed())))
        } else if said.contains("does not exist") && said.contains("bridge") {
            Some((2, Failure::NoBridge(boxed())))
        } else if said.contains(IMAGE) {
            Some((0, Failure::ImageDownload(boxed())))
        } else if said.contains("_sdn_") {
            Some((1, Failure::Runner(boxed())))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> SignIn {
        SignIn::Token(SecretValue::new("root@pam!t=s3cret"))
    }

    #[test]
    fn the_api_address_is_the_endpoints_host_and_port() {
        for (endpoint, address) in [
            ("https://pve.lan:8006/", "pve.lan:8006"),
            ("https://10.0.0.5/", "10.0.0.5:8006"),
            ("https://10.0.0.5", "10.0.0.5:8006"),
            ("https://[fd00::5]:8443/api2/json", "[fd00::5]:8443"),
            ("https://[fd00::5]/", "[fd00::5]:8006"),
            ("pve.lan:9000", "pve.lan:9000"),
        ] {
            assert_eq!(Proxmox::new(endpoint, token(), "pve").api_address(), address, "{endpoint}");
        }
    }

    #[test]
    fn the_values_name_the_modules_variables_and_the_secret_is_never_a_value() {
        let vm = TestVm::new("test-vm-1", "probe");
        let mut p = Proxmox::new("https://pve:8006/", token(), "pve2");
        p.bridge = "vmbr1".into();
        p.image_datastore = Some("isos".into());
        p.jump = Some("root@pve".into());
        p.key = Some(Key::new("/keys/id_ed25519", "ssh-ed25519 AAAA me\n"));
        let opts = p.options(&vm, "ssh-ed25519 AAAA me\n");
        let value = |k: &str| opts.env.get(k).map(String::as_str);
        assert_eq!(value("node"), Some("pve2"));
        assert_eq!(value("bridge"), Some("vmbr1"));
        assert_eq!(value("image_datastore"), Some("isos"));
        assert_eq!(value("datastore"), None, "the module's default");
        assert_eq!(value("ssh_public_key"), Some("ssh-ed25519 AAAA me"));
        assert_eq!(value("ssh_private_key_file"), Some("/keys/id_ed25519"));
        assert_eq!(value("ssh_jump"), Some("root@pve"));
        assert_eq!(opts.secrets.get("api_token").map(SecretValue::expose), Some("root@pam!t=s3cret"));
        assert!(opts.env.values().all(|v| !v.contains("s3cret")));
        // Every value is one of the module's variables, or one of the run's own.
        for name in opts.env.keys().map(String::as_str).chain(opts.secrets.iter().map(|(k, _)| k)) {
            let declared = MODULE.contains(&format!("variable \"{name}\""));
            assert!(declared || ["ssh_private_key_file", "ssh_jump"].contains(&name), "{name} isn't a variable");
        }
        let password = Proxmox::new(
            "https://pve:8006/",
            SignIn::Password {
                user: "root@pam".into(),
                password: SecretValue::new("pw"),
            },
            "pve",
        );
        let signed_in = password.options(&vm, "k");
        assert_eq!(signed_in.env.get("username").map(String::as_str), Some("root@pam"));
        assert_eq!(signed_in.secrets.get("password").map(SecretValue::expose), Some("pw"));
    }

    #[test]
    fn the_network_ids_are_short_stable_and_the_vms_own() {
        let (zone, vnet) = network_ids("cyber-test-0123456789abcdef");
        assert_eq!((zone.len(), vnet.len()), (8, 8));
        assert!(zone.starts_with("st") && vnet.starts_with("sn") && zone.get(2..) == vnet.get(2..));
        assert!(zone.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        assert_eq!(network_ids("cyber-test-0123456789abcdef"), (zone.clone(), vnet));
        assert_ne!(network_ids("cyber-test-fedcba9876543210").0, zone);
    }

    #[test]
    fn a_step_ends_on_its_resource_even_with_the_providers_name_masked() {
        for prefix in ["proxmox", "***"] {
            let mut pve = Pve;
            assert_eq!(
                pve.ends(&format!(
                    "{prefix}_download_file.selftest_image: Creation complete after 3s [id=local:iso/x.img]"
                )),
                Some(0)
            );
            assert_eq!(pve.ends(&format!("{prefix}_sdn_applier.selftest_net: Creation complete after 1s")), Some(1));
            assert_eq!(
                pve.ends(&format!("{prefix}_virtual_environment_vm.selftest_vm: Creation complete after 41s [id=101]")),
                Some(3)
            );
            assert_eq!(
                pve.ends(&format!("{prefix}_virtual_environment_vm.selftest_vm: Still creating... [10s elapsed]")),
                None
            );
            assert_eq!(pve.ends(&format!("{prefix}_sdn_applier.selftest_finalizer: Creation complete after 1s")), None);
        }
        assert_eq!(Pve.machine(), MACHINE);
    }

    #[test]
    fn the_failures_people_hit_belong_to_their_step() {
        let failed = |message: &str| ErrorReport::new(isothrum_runner::ErrorCode::ToolFailed, message);
        let explained = |message: &str| Pve.explain(&failed(message)).map(|(at, why)| (at, std::mem::discriminant(&why)));
        let no_address = format!(
            "step `up`: terraform failed: Resource postcondition failed (proxmox_virtual_environment_vm.selftest_vm): the VM {NO_ADDRESS} `vmbr1` within 8m"
        );
        let report = failed("x");
        assert_eq!(
            explained(&no_address),
            Some((3, std::mem::discriminant(&Failure::NoAddress(Box::new(report.clone())))))
        );
        assert_eq!(
            explained("error creating VM: bridge 'sn12ab34' does not exist"),
            Some((2, std::mem::discriminant(&Failure::NoBridge(Box::new(report.clone())))))
        );
        assert_eq!(
            explained("Error downloading (***_download_file.selftest_image): 404"),
            Some((0, std::mem::discriminant(&Failure::ImageDownload(Box::new(report.clone())))))
        );
        assert_eq!(
            explained("Error (proxmox_sdn_zone_simple.selftest_zone): zone exists").map(|(at, _)| at),
            Some(1)
        );
        assert_eq!(explained("401 Unauthorized"), None);
    }

    /// What the course watches is what the module makes, and what it explains, what it says.
    #[test]
    fn the_module_has_the_watched_resources_and_says_no_address_as_explained() {
        for resource in [
            "resource \"proxmox_download_file\" \"selftest_image\"",
            "resource \"proxmox_sdn_applier\" \"selftest_net\"",
            "resource \"proxmox_virtual_environment_vm\" \"selftest_vm\"",
            "output \"ssh_user\"",
            NO_ADDRESS,
        ] {
            assert!(MODULE.contains(resource), "{resource} missing from the module");
        }
        for end in [IMAGE, NETWORK, VM] {
            let (kind, name) = end.trim_start_matches('_').split_once('.').unwrap();
            assert!(MODULE.contains(&format!("\"proxmox_{kind}\" \"{name}\"")), "{end}");
        }
        // Terraform makes what doesn't depend on another side by side: the network waits for the
        // image (and the VM for the network), so a later step's line never ends an earlier one.
        assert!(MODULE.contains("depends_on = [proxmox_sdn_applier.selftest_finalizer, proxmox_download_file.selftest_image]"));
        assert!(MODULE.contains("depends_on = [proxmox_sdn_applier.selftest_net]"));
    }
}
