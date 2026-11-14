//! The client of the Isothrum runner service.
//!
//! One service per runner home (`isothrum serve --home <home>`) runs every operation of that
//! home: one at a time per run, runs side by side, each operation's events streamed to every
//! subscriber. An app or the `isothrum` command line is its client: it sends commands, and
//! follows the events.
//!
//! [`Client::connect`] finds the home's service through `<home>/service.json` and starts one
//! when none answers. The wire protocol is in [`proto`]: newline-delimited JSON over a Unix domain
//! socket in the home (owner-only) or a Windows named pipe (the user's only).
//!
//! # A service of another release
//!
//! [`Client::connect`] takes the service it finds, whatever its release (it fails only when the
//! service speaks another protocol). An app that bundles Isothrum wants its own release:
//! [`Client::connect_exact`] connects only to a service of this crate's [`VERSION`] and protocol.
//! It finds the one that runs, and when it is another release's:
//!
//! - it asks it to stop when idle (`shutdown_if_idle`, which every protocol keeps): one with
//!   operations queued or running is left running, and [`ClientError::Busy`] names them, for the
//!   app to tell its user (and connect again once they ended);
//! - one that doesn't answer is ended (its pid, from `service.json`) only when the home's
//!   service lock is still held, the process with that pid started no later than the service
//!   said it did (a pid reused since is never ended), and the home's journal shows no operation
//!   of it unfinished;
//! - once it is gone (its lock released, its endpoint closed), `binary` is started, as
//!   [`Client::connect`] starts it, and the client connects, checking the new service is this
//!   release ([`ClientError::Version`] when `binary` is another).
//!
//! The app reads none of the home's files itself. Two apps replacing one home's service take
//! turns (`<home>/service.replace.lock`).
//!
//! ```no_run
//! # async fn demo(bundled: &std::path::Path) -> Result<(), isothrum_client::ClientError> {
//! use isothrum_client::{Client, ClientError};
//!
//! match Client::connect_exact(std::path::Path::new("/home/ada/.isothrum"), Some(bundled)).await {
//!     Ok(client) => assert_eq!(client.service().version, isothrum_client::VERSION),
//!     Err(ClientError::Busy { version, operations, .. }) => {
//!         println!("Isothrum {version} is still running {} operation(s): try again once they end", operations.len());
//!     }
//!     Err(e) => return Err(e),
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ```no_run
//! # async fn demo() -> Result<(), isothrum_client::ClientError> {
//! use isothrum_client::{Client, proto::{Command, OpOptions, Target}};
//!
//! let client = Client::connect(std::path::Path::new("/home/ada/.isothrum"), None).await?;
//! let up = Command::Up {
//!     target: Target::new("/labs/web", None),
//!     options: OpOptions::default(),
//!     lifetime_s: None,
//! };
//! let mut operation = client.follow(up).await?;
//! while let Some(event) = operation.next().await {
//!     println!("{:?}", event?);
//! }
//! # Ok(())
//! # }
//! ```

mod client;
mod discovery;
mod error;
pub mod proto;
mod replace;
mod spawn;
pub mod v1;
pub mod wire;

pub use client::{Accepted, Client, Followed, Subscription};
pub use discovery::{ServiceInfo, ServicePaths};
pub use error::ClientError;

/// This crate's release: the version [`Client::connect_exact`] wants of a service.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
