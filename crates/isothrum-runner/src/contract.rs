//! Isoloom's contract names: the names the files Isoloom generates read or write, which the
//! runner must use as they are (a Vagrantfile's variables, a cloud resource's tags, a path on a
//! machine). They are copies, gathered here so there is one place to read them from: Isoloom
//! defines each one (named on each), and `isoloom-manifest` doesn't export them yet. When it does,
//! each constant here becomes its import, as `Provider::plugin`, `access::BOX_ENV` and
//! `ignore::FILE` already are; a rename on Isoloom's side then can't drift from the runner.
//!
//! Nothing here is the runner's own choice: a name the runner picks for itself (a pseudo-step's
//! id, a file in its own state folder) stays with the code that uses it.

/// The folder name every Isoloom output folder starts with (`.isoloom`, `.isoloom-2`…): a run
/// manifest's folder is under one. Isoloom: `isoloom_render::OUTPUT_DIR`.
pub(crate) const OUTPUT_DIR: &str = ".isoloom";

/// The Vagrant machine (its `define` name) that runs a Vagrant run's controller (Ansible, the
/// checks). Isoloom: `isoloom_render::controller::NAME`.
pub(crate) const CONTROLLER: &str = "isoloom-controller";

/// The file the Vagrant controller holds while it powers itself off after its play. Isoloom:
/// the `halt` provisioner `isoloom-vagrant`'s `vms/own.rs` writes.
pub(crate) const HALTING: &str = "/run/isoloom-halting";

/// The key-value tag holding a cloud resource's expiry (Unix seconds). Isoloom:
/// `isoloom-terraform`'s `cloud/instance.rs`.
pub(crate) const EXPIRES_TAG: &str = "isoloom-expires-at";

/// The key-value tag holding a cloud resource's environment (its instance name). Isoloom:
/// `isoloom-terraform`'s `cloud/instance.rs`.
pub(crate) const INSTANCE_TAG: &str = "isoloom-instance";

/// The word tag's prefix holding a resource's expiry, on the clouds whose tags are words
/// (DigitalOcean, Linode): `isoloom-expires-<unix seconds>`. Isoloom: `isoloom-terraform`'s
/// `cloud/instance.rs` (`tag_list`).
pub(crate) const EXPIRES_WORD: &str = "isoloom-expires-";

/// What every name Isoloom reserves starts with: on a cloud whose tags are words, an
/// instance's word is one with it that isn't the expiry's. Isoloom:
/// `isoloom_spec::validate::RESERVED_PREFIX`.
pub(crate) const RESERVED_PREFIX: &str = "isoloom-";

/// The param naming the ESXi host a Vagrant run's VMs go to. Isoloom: `isoloom-vagrant`'s
/// `manifest/slots.rs` and `vagrantfile.rs`.
pub(crate) const ESXI_HOSTNAME: &str = "ESXI_HOSTNAME";

/// The param naming the ESXi host's SSH port. Isoloom: `isoloom-vagrant`'s `manifest/slots.rs`
/// and `vagrantfile.rs`.
pub(crate) const ESXI_HOSTPORT: &str = "ESXI_HOSTPORT";

/// The ESXi host's SSH port when [`ESXI_HOSTPORT`] doesn't say one (the Vagrantfile's default).
/// Isoloom: `isoloom-vagrant`'s `manifest/slots.rs` and `vagrantfile.rs`.
pub(crate) const ESXI_DEFAULT_SSH_PORT: u16 = 22;

/// The variable a Vagrantfile reads as the command Vagrant's SSH to the guests goes through.
/// Isoloom: `isoloom-vagrant`'s `vagrantfile.rs` and `manifest/slots.rs`.
pub(crate) const SSH_PROXY_COMMAND: &str = "ISOLOOM_SSH_PROXY_COMMAND";

/// The variable naming the host ports a run forwards from this computer, as a JSON object
/// `{"<machine>/<port>": <host port>}`: the Vagrantfile reads it, defaulting each to the port's
/// `publish` number. Isoloom: `isoloom-vagrant`'s `vagrantfile::HOSTPORTS`.
pub(crate) const HOSTPORTS: &str = "ISOLOOM_HOSTPORTS";

/// The param asking each published port for its own `preferred` number instead of a free one.
/// Isoloom: `isoloom-compose`'s `manifest/slots.rs` (`PUBLISH_FIXED`) and `isoloom-vagrant`'s.
pub(crate) const PUBLISH_FIXED: &str = "ISOLOOM_PUBLISH_FIXED";

/// The folder on a Linux machine its guest-delivered secrets are kept in, one file per slot.
/// Isoloom: `isoloom-render`'s `secrets::KEPT_DIR`.
pub(crate) const SECRETS_DIR: &str = "/var/lib/isoloom/secrets";

/// The mark a Linux machine's set-up waits for: every guest-delivered secret is in
/// [`SECRETS_DIR`]. Isoloom: `isoloom_render::secrets::DELIVERED`.
pub(crate) const DELIVERED: &str = "/var/lib/isoloom/secrets/.delivered";
