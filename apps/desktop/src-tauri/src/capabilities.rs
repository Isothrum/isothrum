//! Which pages may call the app's commands. Release builds: the bundled editor and isothrum.com
//! (`capabilities/editor.json`). Debug builds add the same permissions for a local build of the
//! site, so `ISOTHRUM_URL=http://localhost:3000` works while developing.

use serde::Deserialize;
use tauri::ipc::CapabilityBuilder;
use tauri::{Manager, Runtime};

const EDITOR: &str = include_str!("../capabilities/editor.json");
/// A local build of the site: allowed in debug builds only.
const LOCAL_SITE: &str = "http://localhost:*/*";

#[derive(Deserialize)]
struct Capability {
    windows: Vec<String>,
    permissions: Vec<String>,
}

/// The editor capability, parsed from its bundled JSON.
fn editor() -> serde_json::Result<Capability> {
    serde_json::from_str(EDITOR)
}

/// The editor's permissions, for a local build of the site.
fn local_site() -> serde_json::Result<CapabilityBuilder> {
    let editor = editor()?;
    let builder = CapabilityBuilder::new("editor-local-site")
        .local(false)
        .remote(LOCAL_SITE.into())
        .windows(editor.windows);
    Ok(editor
        .permissions
        .into_iter()
        .fold(builder, tauri::ipc::CapabilityBuilder::permission))
}

/// Lets a local build of the site call the commands, in debug builds only.
pub(crate) fn allow_local_site<R: Runtime, M: Manager<R>>(app: &M) -> tauri::Result<()> {
    if cfg!(debug_assertions) {
        let capability = local_site().map_err(std::io::Error::other)?;
        app.add_capability(capability)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_builds_trust_isothrum_com_only() {
        let editor: serde_json::Value = serde_json::from_str(EDITOR).unwrap();
        assert_eq!(
            editor["remote"]["urls"],
            serde_json::json!(["https://www.isothrum.com/*"])
        );
        assert!(!EDITOR.contains("localhost") && !EDITOR.contains("127.0.0.1"));
    }

    /// The updater's file for macOS is the `.app` bundle, archived (`Isothrum.app.tar.gz`): without
    /// the `app` target, a release has nothing for Macs to update from.
    #[test]
    fn every_platform_has_an_update_to_download() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["bundle"]["createUpdaterArtifacts"], true);
        let targets = conf["bundle"]["targets"].as_array().unwrap();
        for t in ["app", "appimage", "msi"] {
            assert!(targets.contains(&t.into()), "{t} in {targets:?}");
        }
    }

    #[test]
    fn the_bundled_editor_has_a_csp() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let csp = conf["app"]["security"]["csp"].as_str().unwrap_or_default();
        assert!(csp.contains("default-src 'self'"), "{csp}");
        assert!(csp.contains("object-src 'none'"), "{csp}");
    }
}
