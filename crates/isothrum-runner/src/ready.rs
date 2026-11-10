//! Ready conditions: what must hold, in order, once a run's start steps succeeded. A condition
//! that can't be checked fails: a wait that returns OK when SSH never answers reads a run
//! that never came up as ready.

mod file;

use std::num::NonZeroU32;
use std::time::Duration;

use isoloom_manifest::{Condition, Role, Template};

use crate::Result;
use crate::driver::Observed;
use crate::error::Error;
use crate::events::ReadyState;
use crate::executor::{Cx, Ran};
use crate::status::MachineState;

/// How often a `tcp` condition is tried again.
const TCP_RETRY: Duration = Duration::from_secs(2);

/// Checks the manifest's ready conditions after `ran` (`None`: a resume, where the `step_ok`
/// conditions, about start steps that didn't run, are left out).
pub(crate) async fn check(cx: &mut Cx<'_>, ran: Option<&Ran>) -> Result<()> {
    let run = cx.run;
    for condition in &run.manifest().ready {
        match condition {
            // A resume runs no start step: what they established was kept.
            Condition::StepOk { step, means } => {
                if let Some(ran) = ran {
                    step_ok(cx, ran, step.as_str(), means.as_deref())?;
                }
            }
            Condition::MachinesRunning { machines } => {
                let names: Vec<&str> = machines.iter().map(isoloom_manifest::Name::as_str).collect();
                machines_running(cx, &names).await?;
            }
            Condition::Tcp { address, port, timeout_s } => {
                let address = cx.vars.render(address, "ready.tcp.address")?;
                let port = cx.vars.render(port, "ready.tcp.port")?;
                tcp(cx, &address, &port, Duration::from_secs(u64::from(timeout_s.get()))).await?;
            }
            Condition::ReadyFile {
                host,
                user,
                path,
                log,
                via,
                timeout_s,
                unreachable_timeout_s,
                interval_s,
                ..
            } => {
                let asked = ReadyFile {
                    host,
                    user,
                    path,
                    log: log.as_ref(),
                    via: via.as_ref(),
                    timeout_s: *timeout_s,
                    unreachable_timeout_s: *unreachable_timeout_s,
                    interval_s: *interval_s,
                };
                file::wait(cx, &ready_file(cx, &asked)?).await?;
            }
        }
    }
    Ok(())
}

/// A `ready_file` condition's fields, as the manifest gives them.
struct ReadyFile<'m> {
    host: &'m Template,
    user: &'m Template,
    path: &'m Template,
    log: Option<&'m Template>,
    via: Option<&'m Template>,
    timeout_s: NonZeroU32,
    unreachable_timeout_s: Option<NonZeroU32>,
    interval_s: NonZeroU32,
}

/// The wait of a `ready_file` condition, its templates filled.
fn ready_file(cx: &Cx<'_>, asked: &ReadyFile<'_>) -> Result<file::Wait> {
    let render = |t: &Template, what: &str| cx.vars.render(t, &format!("ready.ready_file.{what}"));
    let host = render(asked.host, "host")?;
    let via = asked.via.map(|v| render(v, "via")).transpose()?.unwrap_or_default();
    let secs = |n: NonZeroU32| Duration::from_secs(u64::from(n.get()));
    Ok(file::Wait {
        condition: format!("ready_file {host}"),
        login: crate::ssh::Login::new(&render(asked.user, "user")?, &host, cx.ssh_jump.as_deref(), &via)?,
        path: render(asked.path, "path")?,
        log: asked.log.map(|l| render(l, "log")).transpose()?,
        within: secs(asked.timeout_s),
        unreachable: asked.unreachable_timeout_s.map(secs),
        every: secs(asked.interval_s),
    })
}

fn step_ok(cx: &Cx<'_>, ran: &Ran, step: &str, means: Option<&str>) -> Result<()> {
    let condition = format!("step_ok {step}");
    let detail = means.unwrap_or_default();
    if ran.ok.contains(step) {
        cx.events.ready(&condition, ReadyState::Ok, detail);
        return Ok(());
    }
    cx.events.ready(&condition, ReadyState::Failed, detail);
    Err(Error::NotReady {
        condition,
        detail: "the step didn't succeed".into(),
    })
}

async fn machines_running(cx: &mut Cx<'_>, names: &[&str]) -> Result<()> {
    let run = cx.run;
    let observed: Observed = run.observe(cx).await?;
    let down: Vec<String> = observed
        .machines
        .iter()
        .filter(|m| names.contains(&m.name.as_str()) || (names.is_empty() && m.role == Role::Target))
        .filter(|m| m.state != MachineState::Running)
        .map(|m| m.name.clone())
        .collect();
    let condition = "machines_running";
    if down.is_empty() {
        cx.events.ready(condition, ReadyState::Ok, "");
        return Ok(());
    }
    let detail = format!("not running: {}", down.join(", "));
    cx.events.ready(condition, ReadyState::Failed, &detail);
    Err(Error::NotReady {
        condition: condition.into(),
        detail,
    })
}

async fn tcp(cx: &Cx<'_>, address: &str, port: &str, within: Duration) -> Result<()> {
    let condition = format!("tcp {address}:{port}");
    let number: u16 = port.parse().map_err(|_| Error::Malformed {
        at: "ready.tcp.port".into(),
        reason: format!("`{port}` isn't a port"),
    })?;
    let deadline = tokio::time::Instant::now() + within;
    let mut said_waiting = false;
    loop {
        let tried = tokio::time::Instant::now();
        let left = deadline.saturating_duration_since(tried);
        if cx.run.runner.config.ports.reachable(address, number, left.min(TCP_RETRY)).await {
            cx.events.ready(&condition, ReadyState::Ok, "");
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            let detail = format!("no answer within {}s", within.as_secs());
            cx.events.ready(&condition, ReadyState::Failed, &detail);
            return Err(Error::NotReady { condition, detail });
        }
        if !said_waiting {
            let detail = format!("no answer yet; trying every {}s for {}s", TCP_RETRY.as_secs(), within.as_secs());
            cx.events.ready(&condition, ReadyState::Waiting, &detail);
            said_waiting = true;
        }
        // A refusal comes at once: wait out the rest of the interval before trying again.
        tokio::time::sleep_until((tried + TCP_RETRY).min(deadline)).await;
    }
}
