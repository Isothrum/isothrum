//! A tool run from its pinned image instead of the host's binary, where the manifest says so
//! (`tools[].container` on `container_hosts`): Terraform for a Proxmox server on macOS, whose
//! Local Network Privacy keeps third-party binaries (the bpg/proxmox provider) off the LAN while
//! Docker Desktop carries a container's traffic. The decision is the manifest's and the host's
//! only, so every operation of a run takes the same way.
//!
//! The container gets the run's folder, the run's state folder, the runner's SSH folder and the
//! shared plugin cache, each at its own path (so every path in the module, the state and the
//! variables stays valid), and the step's environment by name (`-e NAME`): no value is ever on
//! its command line. It reaches the network the way this host does: the proxy and CA bundle
//! variables and `TF_CLI_CONFIG_FILE` (a provider mirror, credentials) by name too, and the files
//! they name mounted read-only at their own paths.

use std::path::{Path, PathBuf};

use isoloom_manifest::Tool;

use crate::Result;
use crate::executor::Cx;
use crate::process::ProcessSpec;

/// The tool of the manifest that runs `program` from an image on this host, if one does.
pub(crate) fn tool_for<'a>(cx: &'a Cx<'_>, program: &str) -> Option<&'a Tool> {
    let host = cx.run.runner.config.host;
    cx.run
        .manifest()
        .tools
        .iter()
        .find(|t| t.container.is_some() && t.container_hosts.contains(&host) && program_of(t).is_some_and(|p| p == program))
}

/// The program `tool`'s probe runs: the tool's own binary.
fn program_of(tool: &Tool) -> Option<&str> {
    match &tool.probe {
        isoloom_manifest::HostArgv::Every(a) => Some(a.program().as_str()),
        isoloom_manifest::HostArgv::PerHost(p) => p.default.as_ref().map(|a| a.program().as_str()),
    }
}

/// `spec`, run from its tool's image when the manifest says so on this host; else as it is.
pub(crate) fn wrap(cx: &Cx<'_>, spec: ProcessSpec) -> Result<ProcessSpec> {
    let Some(image) = tool_for(cx, &spec.program).and_then(|t| t.container.as_ref()) else {
        return Ok(spec);
    };
    let state = cx.run.runner.store.run_dir(cx.run.name())?;
    let home = &cx.run.runner.config.home;
    let mounts = mounts(&[
        lab_root(cx.run.dir()),
        &state,
        &home.join("ssh"),
        &crate::driver::plugin_cache(home),
    ]);
    let network = HostNetwork::read(|name| std::env::var(name).ok());
    Ok(docker_run(image.as_str(), &mounts, &network, spec))
}

/// The terraform CLI's own configuration file (a provider mirror, credentials).
const TF_CLI_CONFIG_FILE: &str = "TF_CLI_CONFIG_FILE";

/// What a container needs of this host's network settings: the variables it gets by name, and
/// the files they name, mounted read-only.
#[derive(Debug, Default, PartialEq, Eq)]
struct HostNetwork {
    /// The variables set here: the proxies, the CA bundles, `TF_CLI_CONFIG_FILE`.
    names: Vec<String>,
    /// The CA bundles and the CLI configuration they name, those that are files.
    files: Vec<PathBuf>,
}

impl HostNetwork {
    /// The settings `var` (the environment) holds.
    fn read(var: impl Fn(&str) -> Option<String>) -> HostNetwork {
        let set = |name: &&str| var(name).filter(|v| !v.is_empty());
        let names = crate::network_env::NETWORK_VARS
            .iter()
            .chain([&TF_CLI_CONFIG_FILE])
            .filter(|n| set(n).is_some());
        let files = crate::network_env::CA_BUNDLE_VARS.iter().chain([&TF_CLI_CONFIG_FILE]).filter_map(set);
        let mut files: Vec<PathBuf> = files.map(PathBuf::from).filter(|f| f.is_file()).collect();
        files.sort();
        files.dedup();
        HostNetwork {
            names: names.map(|n| (*n).to_owned()).collect(),
            files,
        }
    }
}

/// The run folder a manifest's folder belongs to: the parent of its `.isoloom…` folder (the
/// folder itself when it isn't under one). A module reads the run's files from there.
pub(crate) fn lab_root(dir: &Path) -> &Path {
    dir.ancestors()
        .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".isoloom")))
        .and_then(Path::parent)
        .unwrap_or(dir)
}

/// `folders`, sorted, each once, none inside another.
fn mounts(folders: &[&Path]) -> Vec<PathBuf> {
    let mut all: Vec<PathBuf> = folders.iter().map(|p| p.to_path_buf()).collect();
    all.sort();
    all.dedup();
    let outer = all.clone();
    all.retain(|p| !outer.iter().any(|o| o != p && p.starts_with(o)));
    all
}

/// `spec` as `docker run` of `image`: in its folder, with `mounts` at their own paths, its
/// environment and the host's `network` settings by name (their files mounted read-only), its
/// input piped.
fn docker_run(image: &str, mounts: &[PathBuf], network: &HostNetwork, spec: ProcessSpec) -> ProcessSpec {
    let mut args: Vec<String> = vec!["run".into(), "--rm".into(), "-i".into()];
    if let Some(cwd) = &spec.cwd {
        args.extend(["-w".into(), cwd.display().to_string()]);
    }
    for m in mounts {
        let m = m.display().to_string();
        args.extend(["-v".into(), format!("{m}:{m}")]);
    }
    for f in &network.files {
        let f = f.display().to_string();
        args.extend(["-v".into(), format!("{f}:{f}:ro")]);
    }
    // `-e NAME` passes each variable by name only, never its value on the command line. The value
    // still rides the container's environment, so while this ephemeral `--rm` container runs its
    // value is visible via `docker inspect` / `/proc/1/environ` to anyone with Docker-daemon access
    // (already a host compromise). A `-var-file` mount would avoid even that (reviewed, finding S5).
    for (name, _) in &spec.env {
        args.extend(["-e".into(), name.clone()]);
    }
    for name in network.names.iter().filter(|n| !spec.env.iter().any(|(k, _)| k == *n)) {
        args.extend(["-e".into(), name.clone()]);
    }
    args.push(image.to_owned());
    args.extend(spec.args);
    ProcessSpec {
        program: "docker".into(),
        args,
        cwd: spec.cwd,
        env: spec.env,
        stdin: spec.stdin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lab_root_is_above_its_isoloom_folder() {
        assert_eq!(lab_root(Path::new("/l/x/.isoloom/proxmox")), Path::new("/l/x"));
        assert_eq!(lab_root(Path::new("/l/x/.isoloom-2/docker-vm/proxmox")), Path::new("/l/x"));
        assert_eq!(lab_root(Path::new("/tmp/m")), Path::new("/tmp/m"));
    }

    #[test]
    fn a_folder_inside_another_mount_isnt_mounted_twice() {
        let m = mounts(&[Path::new("/h/runs/r"), Path::new("/h"), Path::new("/l"), Path::new("/h/ssh"), Path::new("/l")]);
        assert_eq!(m, [PathBuf::from("/h"), PathBuf::from("/l")]);
    }

    #[test]
    fn the_container_gets_the_variables_by_name_never_their_values() {
        let spec = ProcessSpec {
            program: "terraform".into(),
            args: vec!["apply".into(), "-auto-approve".into()],
            cwd: Some("/l/x/.isoloom/proxmox".into()),
            env: vec![("TF_VAR_proxmox_password".into(), "s3cret".into())],
            stdin: None,
        };
        let run = docker_run(
            "hashicorp/terraform:1.9.8@sha256:00",
            &[PathBuf::from("/l/x"), PathBuf::from("/h")],
            &HostNetwork::default(),
            spec,
        );
        assert_eq!(run.program, "docker");
        assert_eq!(
            run.args,
            [
                "run",
                "--rm",
                "-i",
                "-w",
                "/l/x/.isoloom/proxmox",
                "-v",
                "/l/x:/l/x",
                "-v",
                "/h:/h",
                "-e",
                "TF_VAR_proxmox_password",
                "hashicorp/terraform:1.9.8@sha256:00",
                "apply",
                "-auto-approve"
            ]
        );
        assert!(!run.args.iter().any(|a| a.contains("s3cret")));
        assert_eq!(run.env, [("TF_VAR_proxmox_password".to_owned(), "s3cret".to_owned())]);
    }

    #[test]
    fn the_container_reaches_the_network_as_this_host_does() {
        let certs = tempfile::tempdir().unwrap();
        let (bundle, cli_config) = (certs.path().join("corp.pem"), certs.path().join("terraformrc"));
        std::fs::write(&bundle, "-----BEGIN CERTIFICATE-----").unwrap();
        std::fs::write(&cli_config, "provider_installation {}").unwrap();
        let env = [
            ("HTTPS_PROXY", "http://proxy.corp:3128".to_owned()),
            ("no_proxy", ".corp,10.0.0.0/8".to_owned()),
            ("SSL_CERT_FILE", bundle.display().to_string()),
            ("CURL_CA_BUNDLE", "/no/such/bundle.pem".to_owned()),
            ("TF_CLI_CONFIG_FILE", cli_config.display().to_string()),
            ("HTTP_PROXY", String::new()),
        ];
        let network = HostNetwork::read(|n| env.iter().find(|(k, _)| *k == n).map(|(_, v)| v.clone()));
        assert_eq!(
            network.names,
            ["HTTPS_PROXY", "no_proxy", "SSL_CERT_FILE", "CURL_CA_BUNDLE", "TF_CLI_CONFIG_FILE"]
        );
        assert_eq!(network.files, [bundle.clone(), cli_config.clone()], "only files that exist");

        let mut spec = ProcessSpec::new("terraform", ["init"]);
        spec.env = vec![("HTTPS_PROXY".into(), "http://step:8080".into())];
        let run = docker_run("terraform:1", &[], &network, spec);
        let line = run.args.join(" ");
        assert!(
            line.contains(&format!("-v {0}:{0}:ro", bundle.display())) && line.contains(&format!("-v {0}:{0}:ro", cli_config.display())),
            "{line}"
        );
        assert_eq!(run.args.iter().filter(|a| *a == "HTTPS_PROXY").count(), 1, "the step's own once: {line}");
        assert!(
            line.contains("-e no_proxy -e SSL_CERT_FILE") && !line.contains("proxy.corp"),
            "by name only: {line}"
        );
    }
}
