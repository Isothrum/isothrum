//! The Docker adapter: the objects a project's label marks (its containers, networks and volumes,
//! the anonymous ones included), their removal by id, and the networks a container is on; the
//! images the engine holds, what a reference stands for, an image's size, and the removal of a
//! reference; the address blocks the engine's networks hold; and what the running containers use
//! ([`stats()`]).

mod stats;

use std::fmt;

pub(crate) use stats::ContainerStats;

use super::{Call, CliError, argv, said_any};
use crate::process::Captured;

/// The tool.
const TOOL: &str = "docker";

/// A kind of Docker object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Object {
    /// A container (stopped ones included).
    Container,
    /// A network.
    Network,
    /// A volume.
    Volume,
}

impl Object {
    /// Its CLI word.
    fn word(self) -> &'static str {
        match self {
            Object::Container => "container",
            Object::Network => "network",
            Object::Volume => "volume",
        }
    }
}

/// A failed Docker call, as far as a driver acts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The object is gone already (`No such container`, `network … not found`, `no such volume`,
    /// `No such image`).
    NoSuchObject,
    /// Any other failure: its reason.
    Other(String),
}

impl Failure {
    /// What a failed call that printed `out` failed with.
    pub(crate) fn of(out: &Captured) -> Failure {
        if said_any(
            out,
            &["no such container", "no such volume", "no such network", "no such image", "no such object"],
        ) || said_any(out, &["network"]) && said_any(out, &["not found"])
        {
            Failure::NoSuchObject
        } else {
            Failure::Other(out.reason())
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::NoSuchObject => f.write_str("no such object"),
            Failure::Other(reason) => f.write_str(reason),
        }
    }
}

/// The ids of the `kind` objects labelled `label` (`key=value`).
pub(crate) async fn labelled(call: &Call<'_>, kind: Object, label: &str) -> Result<Vec<String>, CliError<Failure>> {
    let mut args = argv([kind.word(), "ls", "--quiet", "--filter"]);
    args.push(format!("label={label}"));
    if kind == Object::Container {
        args.push("--all".into());
    }
    let out = call.ok(TOOL, args, Failure::of).await?;
    Ok(out.stdout.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).map(str::to_owned).collect())
}

/// Removes the `kind` objects `ids` (a container forced, with its anonymous volumes); one already
/// gone is removed.
pub(crate) async fn remove(call: &Call<'_>, kind: Object, ids: &[String]) -> Result<(), CliError<Failure>> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut args = match kind {
        Object::Container => argv(["container", "rm", "--force", "--volumes"]),
        other => argv([other.word(), "rm"]),
    };
    args.extend(ids.iter().cloned());
    match call.ok(TOOL, args, Failure::of).await {
        Err(CliError::Failed {
            failure: Failure::NoSuchObject,
            ..
        })
        | Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}

/// What each running container of the engine uses now (`stats --no-stream`, one sample): every
/// running one, so a container gone since it was listed fails nothing.
pub(crate) async fn stats(call: &Call<'_>) -> Result<Vec<ContainerStats>, CliError<Failure>> {
    let out = call.ok(TOOL, argv(["stats", "--no-stream", "--format", "{{json .}}"]), Failure::of).await?;
    Ok(stats::parse(&out.stdout.join("\n")))
}

/// The id of every image the engine holds (`image ls --quiet --no-trunc`), each once.
pub(crate) async fn image_ids(call: &Call<'_>) -> Result<std::collections::BTreeSet<String>, CliError<Failure>> {
    let out = call.ok(TOOL, argv(["image", "ls", "--quiet", "--no-trunc"]), Failure::of).await?;
    Ok(out.stdout.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).map(str::to_owned).collect())
}

/// What `image inspect --format <format> <image>` prints, trimmed; none when the engine holds no
/// such image.
async fn inspect(call: &Call<'_>, image: &str, format: &str) -> Result<Option<String>, CliError<Failure>> {
    let args = vec!["image".into(), "inspect".into(), "--format".into(), format.into(), image.into()];
    match call.ok(TOOL, args, Failure::of).await {
        Ok(out) => Ok(Some(out.stdout.join("\n").trim().to_owned()).filter(|s| !s.is_empty())),
        Err(CliError::Failed {
            failure: Failure::NoSuchObject,
            ..
        }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The id of the image `reference` (`nginx:1.27`) now stands for; none when no image has it.
pub(crate) async fn image_id(call: &Call<'_>, reference: &str) -> Result<Option<String>, CliError<Failure>> {
    inspect(call, reference, "{{.Id}}").await
}

/// Image `image`'s size in bytes, as the engine says; none when it holds no such image.
pub(crate) async fn image_size(call: &Call<'_>, image: &str) -> Result<Option<u64>, CliError<Failure>> {
    let Some(said) = inspect(call, image, "{{.Size}}").await? else {
        return Ok(None);
    };
    said.parse().map(Some).map_err(|e| CliError::Unexpected {
        tool: TOOL,
        what: format!("an image size `{said}`: {e}"),
    })
}

/// Removes image reference `reference` (`image rm`, never forced: an image a container uses is
/// refused); the image goes with its last reference.
pub(crate) async fn remove_image(call: &Call<'_>, reference: &str) -> Result<(), CliError<Failure>> {
    call.ok(TOOL, vec!["image".into(), "rm".into(), reference.into()], Failure::of).await.map(drop)
}

/// The IPv4 address blocks (CIDR) the engine's networks hold, but those of the networks whose
/// `label_key` label is `except` (a Compose project's own): `network ls`, then `network inspect`
/// of the others. IPv6 blocks are left out.
pub(crate) async fn network_blocks(call: &Call<'_>, label_key: &str, except: Option<&str>) -> Result<Vec<String>, CliError<Failure>> {
    let listed = call
        .ok(
            TOOL,
            vec![
                "network".into(),
                "ls".into(),
                "--format".into(),
                format!("{{{{.Name}}}}\t{{{{.Label \"{label_key}\"}}}}"),
            ],
            Failure::of,
        )
        .await?;
    let others: Vec<String> = listed
        .stdout
        .iter()
        .filter_map(|l| {
            let (name, label) = l.split_once('\t').unwrap_or((l, ""));
            let name = name.trim();
            (!name.is_empty() && except != Some(label.trim())).then(|| name.to_owned())
        })
        .collect();
    if others.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = argv(["network", "inspect", "--format", "{{range .IPAM.Config}}{{.Subnet}} {{end}}"]);
    args.extend(others);
    let out = call.ok(TOOL, args, Failure::of).await?;
    Ok(out
        .stdout
        .iter()
        .flat_map(|l| l.split_whitespace())
        .filter(|block| !block.contains(':'))
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::testing;
    use crate::process::fake::{FakeProcesses, Reply};

    const LABEL: &str = "com.docker.compose.project=stack";

    #[tokio::test]
    async fn labelled_containers_stopped_ones_included_are_listed_and_removed_by_id_with_their_volumes() {
        let fake = FakeProcesses::script()
            .expect(
                "docker container ls --quiet --filter label=com.docker.compose.project=stack --all",
                Reply::ok("3f2a9c1e7b6d\n9c1e7b6d3f2a\n"),
            )
            .expect("docker container rm --force --volumes 3f2a9c1e7b6d 9c1e7b6d3f2a", Reply::ok(""))
            .build();
        let call = testing::call(&fake);
        let ids = labelled(&call, Object::Container, LABEL).await.expect("listed");
        assert_eq!(ids, ["3f2a9c1e7b6d", "9c1e7b6d3f2a"]);
        remove(&call, Object::Container, &ids).await.expect("removed");
        remove(&call, Object::Network, &[]).await.expect("nothing to remove runs nothing");
    }

    #[tokio::test]
    async fn an_object_already_gone_is_removed_and_any_other_refusal_fails_with_what_docker_said() {
        for (kind, said) in [
            (Object::Container, include_str!("../tool_output/fixtures/compose/27.1-container-rm-gone.txt")),
            (Object::Network, include_str!("../tool_output/fixtures/compose/27.1-network-rm-gone.txt")),
            (Object::Volume, include_str!("../tool_output/fixtures/compose/27.1-volume-rm-gone.txt")),
        ] {
            let fake = FakeProcesses::script().expect(&format!("docker {}", kind.word()), Reply::fail(1, said)).build();
            remove(&testing::call(&fake), kind, &["x1".to_owned()]).await.expect("gone is removed");
        }
        let busy = "Error response from daemon: remove stack_data: volume is in use - [3f2a9c1e7b6d]";
        let fake = FakeProcesses::script().expect("docker volume rm stack_data", Reply::fail(1, busy)).build();
        let e = remove(&testing::call(&fake), Object::Volume, &["stack_data".to_owned()])
            .await
            .expect_err("in use");
        assert!(
            matches!(&e, CliError::Failed { failure: Failure::Other(why), .. } if why.starts_with(busy)),
            "{e:?}"
        );
    }
}
