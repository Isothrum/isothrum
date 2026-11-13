//! What the Docker CLI prints, read: the networks `docker inspect` says a container is on. Its
//! fixtures are in `fixtures/docker/`.

use std::collections::BTreeSet;

/// The `--format` that makes `docker inspect <container>` print the container's networks as one
/// JSON object keyed by network name (Docker and Podman alike).
pub(crate) const NETWORKS_FORMAT: &str = "{{json .NetworkSettings.Networks}}";

/// The names of the networks a container is on, from `docker inspect --format`
/// [`NETWORKS_FORMAT`]'s output (`null` when it is on none); why not when that isn't the object.
pub(crate) fn networks(out: &str) -> Result<BTreeSet<String>, String> {
    let on: Option<serde_json::Map<String, serde_json::Value>> =
        serde_json::from_str(out.trim()).map_err(|e| format!("its networks aren't the JSON object `docker inspect` prints: {e}"))?;
    Ok(on.unwrap_or_default().into_iter().map(|(name, _)| name).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_containers_networks_are_read_by_name() {
        let on = |out: &str| networks(out).unwrap().into_iter().collect::<Vec<_>>();
        assert_eq!(
            on(include_str!("fixtures/docker/27.3-inspect-networks.json")),
            ["web-portal_default", "web-portal_lab"]
        );
        assert_eq!(on(include_str!("fixtures/docker/podman-5.2-inspect-networks.json")), ["web-portal_lab"]);
        assert_eq!(on("null\r\n"), Vec::<String>::new(), "on no network");
        assert_eq!(on("{}"), Vec::<String>::new());
        assert!(networks("").is_err(), "no answer isn't no network");
        assert!(
            networks("{\"web-portal_lab\": {\"IPAMConfig\"").is_err(),
            "a cut answer isn't read as a shorter one"
        );
    }
}
