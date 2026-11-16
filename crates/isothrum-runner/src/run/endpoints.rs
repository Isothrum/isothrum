//! A deployment's published service endpoints, as this computer reaches them: where the
//! deployment says each one is, or, when its network is internal to a jump host (the run's
//! `ssh_jump` value, `user@host`), a local forward through that host to it.
//!
//! A forward is the console tunnels' ([`crate::graphical::tunnel`]): `ssh -N -L
//! 127.0.0.1:<free port>:<host>:<port>` through the jump host, signed in with the run's key (its
//! `ssh_private_key_file`, else the runner's), the jump host's key pinned the first time the run
//! meets it, recorded in the run's tool ledger. This process keeps it while the run publishes the
//! endpoint, so a caller asks again and again and gets the same address: one whose `ssh` ended is
//! opened again at the next ask; one the run no longer publishes is ended. A `down` ends them from
//! any process, through the ledger, and so does this process's exit (`kill_on_drop`).

use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex, PoisonError};

use super::Running;
use crate::Result;
use crate::events::Emitter;
use crate::formats::{Published, ToolCx};
use crate::graphical::Tunnel;
use crate::graphical::tunnel::{self, Purpose, To};
use crate::options::RunOptions;
use crate::ssh::Login;

/// The run's value naming the jump host its network is reached through.
const JUMP_VAR: &str = "ssh_jump";

/// The run's value naming the private key that signs in there.
const KEY_VAR: &str = "ssh_private_key_file";

/// The forwards this process keeps, by run (its runner's home and its name), each by the
/// `host:port` it goes to.
type Forwards = BTreeMap<(std::path::PathBuf, String), BTreeMap<String, Tunnel>>;

/// The forwards this process keeps.
static FORWARDS: LazyLock<Mutex<Forwards>> = LazyLock::new(Mutex::default);

/// A service endpoint the deployment publishes, as this computer reaches it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ServiceEndpoint {
    /// Its name, as the deployment gives it (`<machine>/<service>`, by the `published` output's convention).
    pub name: String,
    /// Where this computer reaches it, `host:port`: the deployment's address, or a forward's on
    /// this computer's loopback. None when it can't be reached now ([`ServiceEndpoint::why`]).
    pub address: Option<String>,
    /// Whether `address` is a local forward through the run's jump host.
    pub forwarded: bool,
    /// Why it can't be reached now: its forward didn't open.
    pub why: Option<String>,
}

impl Running<'_> {
    /// The service endpoints the deployment publishes (a Terraform module's `published` output:
    /// `{ "web/site" = "192.0.2.10:8080" }`), each where this computer reaches it: as the
    /// deployment gives it, or, when the run has a jump host (its `ssh_jump` value, `user@host`:
    /// the deployment's network is internal to it), at a local forward through that host, kept
    /// open by this process (the module docs say how long). One whose forward doesn't open says
    /// why, the others are given all the same. `opts` gives what the tools get. Emits nothing and
    /// never locks the run.
    ///
    /// # Errors
    ///
    /// As the deployment's outputs can't be read; `input_invalid` when the run's `ssh_jump` isn't
    /// `user@host`.
    pub async fn service_endpoints(&self, opts: &RunOptions) -> Result<Vec<ServiceEndpoint>> {
        let emitter = Emitter::silent(self.name.as_str(), self.runner.config.clock.as_ref());
        let (published, jump, key) = self
            .with_cx(opts, &emitter, self.runner.config.processes.as_ref(), async |cx: &ToolCx<'_>| {
                Ok((self.driver.published(cx).await?, cx.var(JUMP_VAR), cx.var(KEY_VAR)))
            })
            .await?;
        let Some(jump) = jump else {
            return Ok(published
                .into_iter()
                .map(|p| reached(&p, Ok(format!("{}:{}", p.host, p.port)), false))
                .collect());
        };
        let via = jump
            .split_once('@')
            .and_then(|(user, host)| Login::new(user, host))
            .ok_or_else(|| crate::Error::Input {
                name: JUMP_VAR.to_owned(),
                reason: format!("`{}` isn't `user@host`", jump.escape_debug()),
            })?;
        let run = (self.runner.config.home.clone(), self.name.to_string());
        let wanted: Vec<String> = published.iter().map(|p| format!("{}:{}", p.host, p.port)).collect();
        let mut kept = locked().remove(&run).unwrap_or_default();
        kept.retain(|to, _| wanted.contains(to));
        let mut out = Vec::with_capacity(published.len());
        for (p, to) in published.iter().zip(wanted) {
            if !kept.get_mut(&to).is_some_and(Tunnel::runs) {
                kept.remove(&to);
                let target = To {
                    via: via.clone(),
                    host: p.host.clone(),
                    port: p.port,
                    key: key.clone().map(Into::into),
                };
                match tunnel::open(self.runner, &self.name, &self.state_dir(), Purpose::Endpoint(&p.name), &target).await {
                    Ok(forward) => {
                        kept.insert(to.clone(), forward);
                    }
                    Err(e) => {
                        out.push(reached(p, Err(e.to_string()), true));
                        continue;
                    }
                }
            }
            let at = kept.get(&to).map(|t| t.local().to_string());
            out.push(reached(p, at.ok_or_else(String::new), true));
        }
        locked().insert(run, kept);
        Ok(out)
    }
}

/// `p` reached at `at` (or why not), `forwarded` or not.
fn reached(p: &Published, at: std::result::Result<String, String>, forwarded: bool) -> ServiceEndpoint {
    let (address, why) = match at {
        Ok(at) => (Some(at), None),
        Err(why) => (None, Some(why)),
    };
    ServiceEndpoint {
        name: p.name.clone(),
        address,
        forwarded,
        why,
    }
}

/// The forwards this process keeps, locked.
fn locked() -> std::sync::MutexGuard<'static, Forwards> {
    FORWARDS.lock().unwrap_or_else(PoisonError::into_inner)
}
