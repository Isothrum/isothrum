//! The way into a Terraform run's VM, when its containers run in one (cloud-docker, Proxmox
//! docker-vm): the login of the guest's `ssh.run` status step, filled with the run's outputs.

use isoloom_manifest::{Action, Run};

use crate::Result;
use crate::error::Error;
use crate::executor::Cx;
use crate::ssh::{self, Login};

use super::state::TfState;

/// The login into the run's guest VM, `state` its Terraform state, whose outputs are bound as
/// the run's are ([`crate::outputs::with_engine`]). `None` when the run has no guest VM; refused
/// when its guest is reached other than by `ssh.run`, and not running while its outputs give no
/// address.
pub(super) fn login(cx: &Cx<'_>, state: &TfState) -> Result<Option<Login>> {
    let Some(g) = &cx.manifest().guest else { return Ok(None) };
    let Run::Action { action: Action::SshRun, with } = &g.status.run else {
        return Err(Error::Unsupported {
            what: "a guest VM whose status isn't an `ssh.run` step: the runner has no way into it".to_owned(),
        });
    };
    let vars = crate::outputs::with_engine(cx.manifest(), &cx.vars, &state.outputs, &state.sensitive);
    let arg = |name: &str| vars.with_text(with, "guest.status", name);
    let host = arg("host")?;
    if host.is_empty() {
        return Err(Error::NotRunning {
            run: cx.run.name().to_string(),
        });
    }
    Ok(Some(ssh::with_login(cx, &host, arg)?))
}
