//! The `server` tier: real servers, each named by its settings (`ISOTHRUM_TEST_*`), what isn't
//! set skipped:
//!
//! - an SSH server: `ISOTHRUM_TEST_SSH_HOST` (`_PORT`, default 22; `_USER`, default `root`), a
//!   private key it lets that user in with (`ISOTHRUM_TEST_SSH_KEY`) and the user's password
//!   (`ISOTHRUM_TEST_SSH_PASSWORD`);
//! - a Proxmox VE server: `ISOTHRUM_TEST_PROXMOX_HOST` (`_PORT`, default 8006), its
//!   certificate's SHA-256 (`_FINGERPRINT`), `root@pam`'s password (`_PASSWORD`), an API token
//!   (`_TOKEN_ID`, `user@realm!name`, and `_TOKEN_SECRET`), and where machines go (`_NODE`,
//!   `_STORAGE`, `_BRIDGE`); its node over SSH is the SSH server's settings;
//! - an ESXi host: `ISOTHRUM_TEST_ESXI_HOST` (`_PORT`, default 22).
//!
//! The server self-test (`selftest::server`) runs on the Proxmox VE server, reached through its
//! node's SSH settings, with Terraform from this computer.
//!
//! A secret is never printed.

mod esxi;
mod proxmox;
mod selftest;
mod ssh;
