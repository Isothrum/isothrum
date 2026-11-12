//! The anonymous volumes of a run's containers, removed with the run (`down`).
//!
//! Compose's `down --volumes` and `rm --volumes` remove a container's anonymous volumes (an image's
//! own `VOLUME`, MySQL's data directory), but not once Compose has recreated the container: the
//! new container mounts its predecessor's anonymous volumes by their names, and Docker keeps a
//! volume mounted by name when its container goes ("Do not remove named mountpoints", moby's
//! `removeMountPoints`). A run whose services were recreated (an update that changed them, a
//! machine joining or leaving) left those volumes behind at every `down`.
//!
//! So `down` reads them while the containers are there, and removes them once the containers are
//! gone. Two guards:
//! - Only volumes Docker labels anonymous (`com.docker.volume.anonymous`), never a named one: a
//!   volume the environment declares, kept or not, is Compose's and the manifest's.
//! - Only those this run's containers mount, never a prune: another project's are never touched.

use std::collections::BTreeSet;

use super::{CALL_LIMIT, Compose, PROJECT_LABEL, docker_spec};
use crate::Result;
use crate::executor::{Cx, Echo, call_ok};

/// The runner's own step the removal reports under.
pub(crate) const STEP: &str = "left-volumes";

/// The label Docker puts on a volume it made for a container's anonymous mount.
const ANONYMOUS_LABEL: &str = "com.docker.volume.anonymous";

/// `docker inspect`'s format for the volumes a container mounts, a name a line.
const MOUNTED: &str = "{{range .Mounts}}{{if eq .Type \"volume\"}}{{println .Name}}{{end}}{{end}}";

impl Compose {
    /// The anonymous volumes this run's containers mount, sorted. None when it has no container,
    /// or when Docker can't be asked: the `down` then goes on as it would have, and says so in the
    /// log.
    pub(crate) async fn anonymous_volumes(&self, cx: &Cx<'_>) -> Vec<String> {
        match self.mounted_anonymous(cx).await {
            Ok(volumes) => volumes,
            Err(e) => {
                tracing::warn!(error = %e, "the anonymous volumes of the run's containers couldn't be read: none will be removed");
                Vec::new()
            }
        }
    }

    /// The volumes labelled anonymous among those the project's containers mount.
    async fn mounted_anonymous(&self, cx: &Cx<'_>) -> Result<Vec<String>> {
        let project = format!("label={PROJECT_LABEL}={}", self.project(cx)?);
        let ids = self.ask(cx, vec!["ps".into(), "-aq".into(), "--filter".into(), project]).await?;
        let ids: Vec<String> = ids.split_whitespace().map(str::to_owned).collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut inspect = vec!["inspect".to_owned(), "--format".to_owned(), MOUNTED.to_owned()];
        inspect.extend(ids);
        let mounted: BTreeSet<String> = self.ask(cx, inspect).await?.split_whitespace().map(str::to_owned).collect();
        if mounted.is_empty() {
            return Ok(Vec::new());
        }
        let listed = self
            .ask(
                cx,
                ["volume", "ls", "-q", "--filter", &format!("label={ANONYMOUS_LABEL}")]
                    .map(str::to_owned)
                    .to_vec(),
            )
            .await?;
        let anonymous: BTreeSet<&str> = listed.split_whitespace().collect();
        Ok(mounted.into_iter().filter(|v| anonymous.contains(v.as_str())).collect())
    }

    /// One read-only Docker call, quiet in the log.
    async fn ask(&self, cx: &Cx<'_>, args: Vec<String>) -> Result<String> {
        call_ok(cx, STEP, &docker_spec(cx, args, Vec::new()), CALL_LIMIT, Echo::Quiet).await
    }

    /// Removes `volumes` ([`Compose::anonymous_volumes`], read before the `down`) once the
    /// containers are gone, as the runner's step [`STEP`]: `docker volume rm --force`, which
    /// passes over one already gone. One still in use stays, said in the log: the run is gone all
    /// the same.
    pub(crate) async fn remove_volumes(&self, cx: &Cx<'_>, volumes: &[String]) -> Result<()> {
        if volumes.is_empty() {
            return Ok(());
        }
        cx.events
            .stepped(STEP, Some("Removing the anonymous volumes the containers left…"), async {
                let mut args: Vec<String> = ["volume", "rm", "--force"].map(str::to_owned).to_vec();
                args.extend(volumes.iter().cloned());
                if let Err(e) = call_ok(cx, STEP, &docker_spec(cx, args, Vec::new()), CALL_LIMIT, Echo::Quiet).await {
                    tracing::warn!(error = %e, "an anonymous volume of the run's containers stays");
                }
                Ok(())
            })
            .await
    }
}
