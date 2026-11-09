//! The `compose` actions (run-manifest-v0.md §6), each a read-then-act sequence one argv can't
//! express. Ported from the launcher's `runtime/docker/lifecycle.rs` and `compose.rs`.

use std::collections::BTreeMap;

use isoloom_manifest::{SnakeName, Step, Template};

use super::{Compose, NO_PINS, config, docker, pins, ps};
use crate::Result;
use crate::error::Error;
use crate::executor::{Cx, env_of};
use crate::store::write_private;

/// The label Compose puts on what a project creates.
const PROJECT_LABEL: &str = "com.docker.compose.project";

/// `compose.pin_ports`: on a fresh run only (no container yet), the ephemeral host ports get
/// fixed ones, so a resumed run keeps its addresses (Docker picks a new ephemeral port each time a
/// container starts). Pins made for existing containers stay as they were created.
pub(super) async fn pin_ports(c: &Compose, cx: &Cx<'_>, step: &Step) -> Result<()> {
    let Some(file) = c.pins(cx)? else {
        return Ok(());
    };
    let env = env_of(cx, "lifecycle.up", step)?;
    let listed = c.compose(cx, step, &["ps", "--all", "--format", "json"], &env).await?;
    if ps::parse_ps(&listed).iter().any(|e| !e.one_off()) {
        return Ok(());
    }
    write_private(&file, NO_PINS.as_bytes()).map_err(Error::io(&file, "written"))?;
    let config = c.compose(cx, step, &["config", "--format", "json"], &env).await?;
    if let Some(yaml) = pins::pick(cx, &config) {
        write_private(&file, yaml.as_bytes()).map_err(Error::io(&file, "written"))?;
    }
    Ok(())
}

/// `compose.check_ports_free`: unless the run is up, every host port it publishes (the pins
/// applied) must be free, and not reserved by another run of this runner starting meanwhile.
/// Run inside the run's lock, right before `up`, so a pinned port taken since it was picked is
/// caught here rather than as Docker's bind error.
pub(super) async fn check_ports_free(c: &Compose, cx: &Cx<'_>, step: &Step) -> Result<()> {
    let env = env_of(cx, "lifecycle.up", step)?;
    let listed = c.compose(cx, step, &["ps", "--format", "json"], &env).await?;
    if ps::parse_ps(&listed).iter().any(|e| !e.one_off() && e.state == "running") {
        return Ok(());
    }
    let config = c.compose(cx, step, &["config", "--format", "json"], &env).await?;
    cx.run.runner.ensure_ports_free(cx.run.name(), config::host_ports(&config))
}

/// `compose.remove_stale_networks`: the project's networks its Compose files no longer define
/// (left by an older version of the run), unplugged and removed.
pub(super) async fn remove_stale_networks(c: &Compose, cx: &Cx<'_>, step: &Step) -> Result<()> {
    let env = env_of(cx, "lifecycle.up", step)?;
    let config = c.compose(cx, step, &["config", "--format", "json"], &env).await?;
    let wanted = config::networks(&config, &c.project(cx)?);
    for net in project_networks(c, cx, step).await? {
        if !wanted.contains(&net) {
            remove_network(cx, step, &net).await?;
        }
    }
    Ok(())
}

/// `compose.remove_networks`: every network of the project, unplugged and removed (a container
/// still on one, an embedder's own, would keep its address block taken).
pub(super) async fn remove_networks(c: &Compose, cx: &Cx<'_>, step: &Step) -> Result<()> {
    for net in project_networks(c, cx, step).await? {
        remove_network(cx, step, &net).await?;
    }
    Ok(())
}

/// `compose.remove_containers`: the project's containers (`with.service` narrows to one service,
/// `with.one_off` to `docker compose run` containers), one-offs included, which `down` leaves.
pub(super) async fn remove_containers(c: &Compose, cx: &Cx<'_>, step: &Step, with: &BTreeMap<SnakeName, Template>) -> Result<()> {
    let arg = |key: &str| with.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| cx.vars.render(v, "with")).transpose();
    let mut args: Vec<String> = ["ps", "-aq", "--filter"].map(String::from).to_vec();
    args.push(format!("label={PROJECT_LABEL}={}", c.project(cx)?));
    if let Some(service) = arg("service")? {
        args.extend(["--filter".into(), format!("label=com.docker.compose.service={service}")]);
    }
    if arg("one_off")?.as_deref() == Some("true") {
        args.extend(["--filter".into(), "label=com.docker.compose.oneoff=True".into()]);
    }
    let ids = docker(cx, step, args, &[]).await?;
    let ids: Vec<String> = ids.split_whitespace().map(str::to_owned).collect();
    if ids.is_empty() {
        return Ok(());
    }
    let rm = ["rm", "-f"].map(String::from).into_iter().chain(ids).collect();
    docker(cx, step, rm, &[]).await.map(drop)
}

/// The networks labelled as the project's.
async fn project_networks(c: &Compose, cx: &Cx<'_>, step: &Step) -> Result<Vec<String>> {
    let filter = format!("label={PROJECT_LABEL}={}", c.project(cx)?);
    let args = ["network", "ls", "--filter", &filter, "--format", "{{.Name}}"].map(String::from).to_vec();
    let listed = docker(cx, step, args, &[]).await?;
    Ok(listed.lines().map(str::trim).filter(|n| !n.is_empty()).map(str::to_owned).collect())
}

/// Unplugs every container from `net` (force), then removes it.
async fn remove_network(cx: &Cx<'_>, step: &Step, net: &str) -> Result<()> {
    let inspect = ["network", "inspect", "-f", "{{range .Containers}}{{.Name}} {{end}}", net]
        .map(String::from)
        .to_vec();
    let attached = docker(cx, step, inspect, &[]).await?;
    for container in attached.split_whitespace() {
        let disconnect = ["network", "disconnect", "-f", net, container].map(String::from).to_vec();
        docker(cx, step, disconnect, &[]).await?;
    }
    docker(cx, step, ["network", "rm", net].map(String::from).to_vec(), &[]).await.map(drop)
}
