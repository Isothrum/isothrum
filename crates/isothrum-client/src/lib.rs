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
//! ```no_run
//! # async fn demo() -> Result<(), isothrum_client::ClientError> {
//! use isothrum_client::{Client, proto::{Command, OpOptions, Target}};
//!
//! let client = Client::connect(std::path::Path::new("/home/ada/.isothrum"), None).await?;
//! let up = Command::Up {
//!     target: Target::new("/labs/web", None),
//!     options: OpOptions::default(),
//!     fresh: false,
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
mod spawn;
pub mod v1;
pub mod wire;

pub use client::{Accepted, Client, Followed, Subscription};
pub use discovery::{ServiceInfo, ServicePaths};
pub use error::ClientError;
