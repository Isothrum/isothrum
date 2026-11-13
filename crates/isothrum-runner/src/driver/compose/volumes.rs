//! The run's anonymous volumes: the ones the engine made for its containers' `VOLUME`s (MySQL's
//! data directory), which `down --volumes` and `rm --volumes` remove only while the container
//! that got them is the one that made them. An update that recreates a service's container
//! inherits them into the new container as volumes named by their id (Compose keeps a service's
//! data across a recreate), and the engine then counts them named: a `down --volumes` after that
//! removes the containers and leaves them (two a lab, in the Cyber CTF rig's leftover audit of
//! every E2E group). So the `down` operation records them before its steps, from
//! the containers the project still has, and removes what the steps left after them. An update
//! orphans them too, at once: a recreate that makes new anonymous volumes for the new container
//! (the fixture lab's, three a lab after three updates) leaves the old ones dangling, and no
//! later `down` sees them attached. So the `update` operation records them before its steps and
//! removes, after them, the recorded ones no container holds any more.

use std::collections::BTreeSet;

use super::{PROJECT_LABEL, docker_spec};
use crate::Result;
use crate::executor::{Cx, Echo, call_ok};

/// The file of the run's state folder holding the names, one a line, between `before_down` and
/// `after_down`.
const RECORD: &str = "anonymous-volumes";
/// The label the engine puts on a volume it made for a container's `VOLUME` (Docker 25 and
/// later); an older engine's one has no label at all.
const ANONYMOUS_LABEL: &str = "com.docker.volume.anonymous";
/// How long a listing may take.
const LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Records the anonymous volumes of `project`'s containers in the run's state folder; a listing
/// that fails records none and says so (the `down` steps answer for the engine).
pub(super) async fn record(project: &str, cx: &Cx<'_>) -> Result<()> {
    let path = cx.state_dir().join(RECORD);
    // A record still here means the last operation was cut before its end (the app quit, a
    // signal; the launch-ui spec closed the app three seconds into an attach): the
    // volumes it recorded are still the run's, so they stay recorded, and what that operation
    // orphaned goes with what this one orphans.
    let mut names: BTreeSet<String> = match std::fs::read_to_string(&path) {
        Ok(text) => text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
        Err(e) => return Err(crate::error::Error::io(&path, "read")(e)),
    };
    let carried_over = names.len();
    match listed(project, cx).await {
        Ok(held) => names.extend(held),
        Err(e) => tracing::warn!(project, error = %e, "the run's anonymous volumes couldn't be listed before the operation"),
    }
    tracing::info!(
        project,
        recorded = names.len(),
        carried_over,
        "the run's anonymous volumes, recorded before the operation"
    );
    let mut text = names.iter().cloned().collect::<Vec<_>>().join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    crate::store::write(&path, text.as_bytes())
}

/// Removes the volumes [`record`] kept that the `down` steps left; one the engine still holds
/// (a container of another's on it) is said, not failed on: the run is down.
pub(super) async fn remove_recorded(cx: &Cx<'_>) -> Result<()> {
    let path = cx.state_dir().join(RECORD);
    let names: Vec<String> = match std::fs::read_to_string(&path) {
        Ok(text) => text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(crate::error::Error::io(&path, "read")(e)),
    };
    tracing::info!(
        recorded = names.len(),
        "the run's anonymous volumes after its down: every recorded one is removed"
    );
    if !names.is_empty() {
        let mut args: Vec<String> = ["volume", "rm", "--force"].map(String::from).to_vec();
        args.extend(names.iter().cloned());
        if let Err(e) = call_ok(cx, "anonymous-volumes", &docker_spec(cx, args, Vec::new()), LIMIT, Echo::Quiet).await {
            tracing::warn!(error = %e, "the run's anonymous volumes weren't all removed after its down");
        }
    }
    crate::store::remove(&path)
}

/// Removes the volumes [`record`] kept that no container holds after an update's steps (the
/// engine's dangling ones among them): the old container's, orphaned by a recreate that made
/// new ones. One still held stays, and the record goes.
pub(super) async fn remove_recorded_dangling(cx: &Cx<'_>) -> Result<()> {
    let path = cx.state_dir().join(RECORD);
    let recorded: BTreeSet<String> = match std::fs::read_to_string(&path) {
        Ok(text) => text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
        Err(e) => return Err(crate::error::Error::io(&path, "read")(e)),
    };
    if !recorded.is_empty() {
        let ls = ["volume", "ls", "--quiet", "--filter", "dangling=true"].map(String::from).to_vec();
        match call_ok(cx, "anonymous-volumes", &docker_spec(cx, ls, Vec::new()), LIMIT, Echo::Quiet).await {
            Ok(listed) => {
                let orphaned: Vec<String> = listed.split_whitespace().filter(|v| recorded.contains(*v)).map(str::to_owned).collect();
                tracing::info!(
                    recorded = recorded.len(),
                    orphaned = orphaned.len(),
                    "the run's anonymous volumes after the operation: the orphaned ones are removed"
                );
                if !orphaned.is_empty() {
                    let mut rm = ["volume", "rm", "--force"].map(String::from).to_vec();
                    rm.extend(orphaned);
                    if let Err(e) = call_ok(cx, "anonymous-volumes", &docker_spec(cx, rm, Vec::new()), LIMIT, Echo::Quiet).await {
                        tracing::warn!(error = %e, "the anonymous volumes an update orphaned weren't all removed");
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "the dangling volumes couldn't be listed after the update"),
        }
    }
    crate::store::remove(&path)
}

/// The anonymous volumes mounted in `project`'s containers now, each once.
async fn listed(project: &str, cx: &Cx<'_>) -> Result<Vec<String>> {
    let ps = ["ps", "-aq", "--filter", &format!("label={PROJECT_LABEL}={project}")]
        .map(String::from)
        .to_vec();
    let ids = call_ok(cx, "anonymous-volumes", &docker_spec(cx, ps, Vec::new()), LIMIT, Echo::Quiet).await?;
    let ids: Vec<String> = ids.split_whitespace().map(str::to_owned).collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut inspect = [
        "inspect",
        "--format",
        r#"{{range .Mounts}}{{if eq .Type "volume"}}{{.Name}}{{"\n"}}{{end}}{{end}}"#,
    ]
    .map(String::from)
    .to_vec();
    inspect.extend(ids);
    let mounted = call_ok(cx, "anonymous-volumes", &docker_spec(cx, inspect, Vec::new()), LIMIT, Echo::Quiet).await?;
    let mounted: BTreeSet<String> = mounted.split_whitespace().map(str::to_owned).collect();
    if mounted.is_empty() {
        return Ok(Vec::new());
    }
    let mut labels = ["volume", "inspect", "--format", "{{.Name}}\t{{json .Labels}}"].map(String::from).to_vec();
    labels.extend(mounted);
    let described = call_ok(cx, "anonymous-volumes", &docker_spec(cx, labels, Vec::new()), LIMIT, Echo::Quiet).await?;
    Ok(anonymous(&described))
}

/// The anonymous volumes among `described`, `docker volume inspect`'s `{{.Name}}\t{{json
/// .Labels}}` lines: one the engine labelled so, or one with no label and the engine's own kind
/// of name (64 hex digits: an older engine's). A volume Compose named for a project, or a user's
/// named one, carries labels or a name of its own and stays.
fn anonymous(described: &str) -> Vec<String> {
    described
        .lines()
        .filter_map(|line| {
            let (name, labels) = line.split_once('\t')?;
            let name = name.trim();
            let labels: serde_json::Value = serde_json::from_str(labels.trim()).unwrap_or(serde_json::Value::Null);
            let labelled = labels.get(ANONYMOUS_LABEL).is_some();
            let unlabelled = labels.as_object().is_none_or(serde_json::Map::is_empty);
            let engine_named = name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
            (labelled || (unlabelled && engine_named)).then(|| name.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::anonymous;

    const ENGINE: &str = "135385800a74aaa45a61d725cd8c13a7f4cb47a4822ee6f28defae9b133c1b08";
    const INHERITED: &str = "e465cfbd5789f9aff3cbbc62398bb3830e0f74b37b835640ad9d85b7a9dcbae1";

    #[test]
    fn the_engines_labelled_volumes_are_anonymous_whatever_their_name() {
        let described = format!("{ENGINE}\t{{\"com.docker.volume.anonymous\":\"\"}}\n{INHERITED}\t{{\"com.docker.volume.anonymous\":\"\"}}\n");
        assert_eq!(anonymous(&described), [ENGINE, INHERITED]);
    }

    #[test]
    fn an_older_engines_volume_with_no_label_and_an_id_for_a_name_is_anonymous() {
        assert_eq!(anonymous(&format!("{ENGINE}\tnull\n")), [ENGINE]);
        assert_eq!(anonymous(&format!("{ENGINE}\t{{}}\n")), [ENGINE]);
    }

    #[test]
    fn a_projects_named_volume_and_a_users_stay() {
        let described = format!(
            "cyberctf-lab_evidence\t{{\"com.docker.compose.project\":\"cyberctf-lab\",\"com.docker.compose.volume\":\"evidence\"}}\n\
             shared-data\tnull\n\
             {ENGINE}\t{{\"com.docker.compose.project\":\"cyberctf-lab\"}}\n\
             not a line\n"
        );
        assert!(anonymous(&described).is_empty(), "{:?}", anonymous(&described));
    }
}
