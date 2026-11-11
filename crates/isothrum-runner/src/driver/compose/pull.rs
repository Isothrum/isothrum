//! A Compose start's images, pulled in a phase of their own before its `up` (a step's
//! `docker compose … up`, the access machine's hot-add): `docker compose … pull` with the
//! project, files and profiles the `up` names, its services (their dependencies too, unless it
//! starts none), and the pull policy it asks for (`--pull`, Compose's `missing` when it says
//! none), images a service builds left out. The pull is its own step (`pull-<step>`), its lines
//! that step's `log` events.
//!
//! A download still moving has no time limit: a large image through a slow connection takes the
//! time it takes. It is stopped once it shows no progress (a new line: a layer's count moving, a
//! layer done) for [`STALL`], as `download_stalled`; a pull that fails (a registry refusing it, a
//! name it doesn't know, the network) is `download_failed`, with Compose's reason. The `up` then
//! runs under its own limit, with nothing left to pull.
//!
//! An `up` this doesn't read whole (an option it doesn't know), or one that pulls nothing
//! (`--pull never`), runs as it is, pulling what it pulls itself. So does every `up` on a Compose
//! without `pull --policy`, which refuses the option: its pull is skipped.

use std::time::Duration;

use isoloom_manifest::{Run, Step};

use crate::Result;
use crate::error::Error;
use crate::executor::{Cx, download, spec_for};
use crate::process::ProcessSpec;

/// How long pulling a project's images may go without progress before it counts as stuck.
pub(crate) const STALL: Duration = Duration::from_secs(15 * 60);

/// What the pull's step says when it starts.
const NOTE: &str = "Pulling the environment's images…";

/// What Compose says of an option it doesn't know (an older one, of `pull --policy`).
const UNKNOWN_FLAG: &str = "unknown flag";

/// Compose's global options that take a value.
const GLOBAL_VALUED: &[&str] = &[
    "-f",
    "--file",
    "-p",
    "--project-name",
    "--profile",
    "--env-file",
    "--project-directory",
    "--ansi",
    "--progress",
    "--parallel",
];

/// Its global options that take none.
const GLOBAL_FLAGS: &[&str] = &["--compatibility", "--dry-run", "--all-resources"];

/// `up`'s options that take a value.
const UP_VALUED: &[&str] = &[
    "--attach",
    "--exit-code-from",
    "--no-attach",
    "--pull",
    "--scale",
    "-t",
    "--timeout",
    "--wait-timeout",
];

/// `up`'s options that take none.
const UP_FLAGS: &[&str] = &[
    "--abort-on-container-exit",
    "--abort-on-container-failure",
    "--always-recreate-deps",
    "--attach-dependencies",
    "--build",
    "-d",
    "--detach",
    "--force-recreate",
    "--menu",
    "--no-build",
    "--no-color",
    "--no-deps",
    "--no-log-prefix",
    "--no-recreate",
    "--no-start",
    "--quiet-build",
    "--quiet-pull",
    "--remove-orphans",
    "-V",
    "--renew-anon-volumes",
    "--timestamps",
    "--wait",
    "-w",
    "--watch",
    "-y",
    "--yes",
];

/// The id of the pull that comes before step (or operation) `id`.
fn id_before(id: &str) -> String {
    format!("pull-{id}")
}

/// Pulls the images argv step `step` of list `list` starts, when it is a Compose `up` that pulls
/// ([`pull_args`]), as a step of its own ([`pull`]); nothing for any other step, nor for one
/// whose command can't be filled (the step itself fails, saying why).
pub(crate) async fn before_step(cx: &Cx<'_>, list: &str, step: &Step) -> Result<()> {
    let Run::Argv(_) = &step.run else { return Ok(()) };
    let Ok(spec) = spec_for(cx, list, step) else { return Ok(()) };
    if spec.program != "docker" {
        return Ok(());
    }
    let Some(args) = pull_args(&spec.args) else { return Ok(()) };
    let pull_spec = ProcessSpec { args, stdin: None, ..spec };
    pull(cx, &id_before(step.id.as_str()), &pull_spec).await
}

/// Pulls the images `up` (a `docker compose … up` the driver runs itself, for `id`) starts, when
/// it pulls ([`pull_args`]), as a step of its own with the `up`'s folder and environment.
pub(super) async fn before_call(cx: &Cx<'_>, id: &str, up: &ProcessSpec) -> Result<()> {
    let Some(args) = pull_args(&up.args) else { return Ok(()) };
    let spec = ProcessSpec {
        args,
        stdin: None,
        ..up.clone()
    };
    pull(cx, &id_before(id), &spec).await
}

/// Runs `spec`, a `docker compose … pull`, as step `id`: stopped once it shows no progress for
/// [`STALL`]; a failure is `download_failed`, except Compose refusing an option it doesn't know,
/// which skips the pull (the `up` pulls as it always did).
async fn pull(cx: &Cx<'_>, id: &str, spec: &ProcessSpec) -> Result<()> {
    cx.events
        .stepped(id, Some(NOTE), async {
            let out = download(cx, id, spec, STALL).await?;
            if out.success() {
                return Ok(());
            }
            let reason = out.reason();
            if reason.contains(UNKNOWN_FLAG) {
                tracing::debug!(%reason, "this Compose can't pull on its own: its `up` pulls");
                return Ok(());
            }
            Err(Error::DownloadFailed {
                step: id.to_owned(),
                reason: cx.events.mask(&reason),
            })
        })
        .await
}

/// One argument of a Compose command line, as far as finding its words goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arg<'a> {
    /// A word: a command, a service.
    Word,
    /// An option with no value.
    Flag,
    /// Option `name`, taking a value: in the same argument (`--pull=missing`), or the next one.
    Valued {
        /// The option.
        name: &'a str,
        /// Its value, when in the same argument.
        inline: Option<&'a str>,
    },
}

/// What `arg` is, among the options `valued` and `flags`: `None` for an option it doesn't know.
fn read<'a>(arg: &'a str, valued: &[&str], flags: &[&str]) -> Option<Arg<'a>> {
    if !arg.starts_with('-') || arg == "-" {
        return Some(Arg::Word);
    }
    let (name, inline) = arg.split_once('=').map_or((arg, None), |(n, v)| (n, Some(v)));
    if valued.contains(&name) {
        Some(Arg::Valued { name, inline })
    } else if flags.contains(&name) && inline.is_none() {
        Some(Arg::Flag)
    } else {
        None
    }
}

/// The arguments of the pull a Compose `up` makes first, from the `up`'s (after `docker`:
/// `compose`, its global options, `up`, `up`'s options, its services): the global options kept,
/// then `pull --policy <policy> --ignore-buildable [--include-deps] <services>`. The policy is
/// the `up`'s `--pull` (`missing` when it says none); `--include-deps` unless it has `--no-deps`.
/// `None` when the arguments aren't a Compose `up` read whole (another command, an option this
/// doesn't know) or the `up` pulls nothing (`--pull never`, or a policy `pull` has no match for).
pub(super) fn pull_args(args: &[String]) -> Option<Vec<String>> {
    let (compose, rest) = args.split_first()?;
    if compose != "compose" {
        return None;
    }
    let mut global = 0;
    let up = loop {
        let arg = rest.get(global)?;
        match read(arg, GLOBAL_VALUED, GLOBAL_FLAGS)? {
            Arg::Word if arg == "up" => break global,
            Arg::Word => return None,
            Arg::Flag | Arg::Valued { inline: Some(_), .. } => global += 1,
            Arg::Valued { inline: None, .. } => global += 2,
        }
    };
    let (globals, rest) = rest.split_at(up);
    let mut policy = "missing";
    let mut deps = true;
    let mut services = Vec::new();
    let mut at = 1;
    while let Some(arg) = rest.get(at) {
        if arg == "--" {
            services.extend(rest.get(at + 1..).unwrap_or_default().iter().cloned());
            break;
        }
        match read(arg, UP_VALUED, UP_FLAGS)? {
            Arg::Word => services.push(arg.clone()),
            Arg::Flag => deps &= arg != "--no-deps",
            Arg::Valued { name, inline } => {
                let value = if let Some(v) = inline {
                    v
                } else {
                    at += 1;
                    rest.get(at)?.as_str()
                };
                if name == "--pull" {
                    policy = value;
                }
            }
        }
        at += 1;
    }
    if !matches!(policy, "missing" | "always") {
        return None;
    }
    let mut pull: Vec<String> = std::iter::once(compose.clone()).chain(globals.iter().cloned()).collect();
    pull.extend(["pull", "--policy", policy, "--ignore-buildable"].map(String::from));
    if deps {
        pull.push("--include-deps".into());
    }
    pull.extend(services);
    Some(pull)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_owned).collect()
    }

    fn pulled(up: &str) -> Option<String> {
        pull_args(&args(up)).map(|a| a.join(" "))
    }

    #[test]
    fn a_compose_up_pulls_its_services_with_its_project_files_and_policy() {
        // As Isoloom writes a start.
        assert_eq!(
            pulled(
                "compose --project-name web --file compose.yml --file /s/ports.yml up --detach --build --pull missing --remove-orphans --wait --wait-timeout 600 db web"
            )
            .as_deref(),
            Some("compose --project-name web --file compose.yml --file /s/ports.yml pull --policy missing --ignore-buildable --include-deps db web")
        );
        // A set-up job: its service alone, Compose's default policy.
        assert_eq!(
            pulled("compose -p web -f compose.yml up --no-deps --exit-code-from init init").as_deref(),
            Some("compose -p web -f compose.yml pull --policy missing --ignore-buildable init")
        );
        // The access machine's hot-add: its profile, an option's value in the same argument.
        assert_eq!(
            pulled("compose --project-name=web --profile access up -d --wait --no-deps --pull=always box").as_deref(),
            Some("compose --project-name=web --profile access pull --policy always --ignore-buildable box")
        );
        // Every service of the project.
        assert_eq!(
            pulled("compose --file c.yml up -d").as_deref(),
            Some("compose --file c.yml pull --policy missing --ignore-buildable --include-deps")
        );
        assert_eq!(
            pulled("compose up -d -- web").as_deref(),
            Some("compose pull --policy missing --ignore-buildable --include-deps web")
        );
    }

    #[test]
    fn an_up_that_pulls_nothing_or_isnt_read_whole_is_left_as_it_is() {
        for not in [
            // It pulls nothing.
            "compose -p web up -d --pull never web",
            "compose -p web up -d --pull=build web",
            // Not an `up`.
            "compose -p web start",
            "compose -p web --profile check run --rm isoloom-check",
            "compose -p web ps --all --format json",
            "run --rm hashicorp/terraform apply",
            // An option this doesn't know: its value could pass for a service.
            "compose -p web up -d --some-new-option x web",
            "compose --some-global x up -d web",
            "compose -p web up -dV web",
            // A value missing.
            "compose -p web up -d --pull",
            "compose -p",
            "",
        ] {
            assert_eq!(pulled(not), None, "{not}");
        }
        assert_eq!(pull_args(&[]), None);
    }
}
