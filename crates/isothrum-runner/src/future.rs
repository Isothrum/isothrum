//! The boxed future the ports' object-safe methods return ([`ProcessRunner`](crate::ProcessRunner),
//! [`HostPorts`](crate::HostPorts)): an embedder implements `ProcessRunner` without `async fn` in
//! traits, and the runner holds the ports as `dyn` values shared between threads.

use std::future::Future;
use std::pin::Pin;

/// A boxed future that can be sent between threads.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
