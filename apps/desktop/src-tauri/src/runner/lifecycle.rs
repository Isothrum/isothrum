//! A lab's life: its folder prepared from the spec, started, read, stopped.

use std::path::Path;

use isoloom_spec::model::Spec;
use isoloom_targets::{GeneratedFile, Lab};

use super::{RUNTIME, RunError, Runtime, compose, lab_of, lines, run, runtime};
use crate::lab_files;
use crate::tools::has;

/// The runtime that fits the lab and Isoloom's files for it: Docker when every machine has a
/// container form, else Vagrant (every machine a VM), else hybrid (both).
pub(super) fn generated(lab: &Lab) -> Result<(Runtime, Vec<GeneratedFile>), RunError> {
    [Runtime::Docker, Runtime::Vagrant, Runtime::Hybrid]
        .into_iter()
        .find_map(|rt| {
            isoloom_targets::generate(lab, rt.target())
                .ok()
                .map(|f| (rt, f))
        })
        .ok_or_else(|| {
            RunError::NotDocker(
                "This lab runs neither as containers nor as local VMs: export it and run it with \
             `isoloom run` on a target that fits it."
                    .into(),
            )
        })
}

/// Checks the spec and writes it with its generated files into `dir`: Docker's when every machine
/// is a container, else Vagrant's (only VMs) or the hybrid output's (VMs and containers).
/// `own`: the lab's own files (provisioning scripts, appliance configurations), at the paths
/// isoloom.yml gives them (relative, inside the lab's folder).
pub(crate) fn prepare(dir: &Path, yaml: &str, own: &[(String, String)]) -> Result<Spec, RunError> {
    if let Some((path, _)) = own.iter().find(|(p, _)| !lab_files::inside_lab(p)) {
        return Err(RunError::Spec(format!(
            "`{path}` isn't a path inside the lab"
        )));
    }
    let lab = valid_lab(yaml)?;
    let (runtime, generated) = generated(&lab)?;
    if runtime != Runtime::Docker && !has("vagrant") {
        return Err(RunError::NotDocker(
            "This lab has VMs: they run with Vagrant (and VirtualBox or libvirt), which isn't \
             installed. Install it from vagrantup.com, or export the lab."
                .into(),
        ));
    }
    lab_files::write(dir, "isoloom.yml", yaml)?;
    lab_files::write(dir, RUNTIME, runtime.name())?;
    let files: Vec<(&str, &str)> = generated
        .iter()
        .map(|f| (f.path.as_str(), f.contents.as_str()))
        .chain(own.iter().map(|(p, c)| (p.as_str(), c.as_str())))
        .collect();
    lab_files::write_all(dir, &files)?;
    Ok(lab.for_target(runtime.target()))
}

/// The lab in `yaml`, when Isoloom finds no problem in it.
fn valid_lab(yaml: &str) -> Result<Lab, RunError> {
    let lab = lab_of(yaml)?;
    let problems = lab.problems();
    if problems.is_empty() {
        return Ok(lab);
    }
    Err(RunError::Spec(lines(&problems)))
}

/// Starts the lab; the links suspended before it was started again stay cut.
pub(crate) fn up(dir: &Path) -> Result<String, RunError> {
    let mut out = if let rt @ (Runtime::Vagrant | Runtime::Hybrid) = runtime(dir) {
        crate::vms::up(dir, rt)?
    } else {
        run(
            compose(dir, &["up", "-d", "--wait", "--remove-orphans"]),
            "Starting the lab",
        )?
    };
    let notes = super::links::reapply_all_links(dir, &super::lab_spec(dir)?);
    if !notes.is_empty() {
        out.push('\n');
        out.push_str(&notes);
    }
    Ok(out)
}

/// Stops the lab: its machines are gone, and its suspended links with them.
pub(crate) fn down(dir: &Path) -> Result<String, RunError> {
    let out = if let rt @ (Runtime::Vagrant | Runtime::Hybrid) = runtime(dir) {
        crate::vms::down(dir, rt)?
    } else {
        run(
            compose(dir, &["down", "--volumes", "--remove-orphans"]),
            "Stopping the lab",
        )?
    };
    super::links::clear_links(dir);
    super::display::remove_forwarders(dir);
    Ok(out)
}

/// The lab's services and their state, as `docker compose ps` reports them (one JSON per line).
pub(crate) fn status(dir: &Path) -> Result<String, RunError> {
    if let rt @ (Runtime::Vagrant | Runtime::Hybrid) = runtime(dir) {
        return crate::vms::status(dir, rt);
    }
    run(
        compose(dir, &["ps", "--all", "--format", "json"]),
        "Reading the lab's state",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{lab_spec, prepared};
    use std::path::PathBuf;

    const CONTAINERS: &str = r#"
version: 1
name: pair
networks:
  office: { cidr: 10.10.1.0/24 }
machines:
  a:
    networks: { office: 10 }
    app: { image: "alpine:3.20" }
  b:
    networks: { office: 11 }
    app: { image: "alpine:3.20" }
"#;

    const WITH_VM: &str = r#"
version: 1
name: mixed
networks:
  office: { cidr: 10.10.1.0/24 }
machines:
  a:
    networks: { office: 10 }
    app: { image: "alpine:3.20" }
  b:
    networks: { office: 11 }
    os: windows-server@2022
    app: { steps: [{ run: provision/b.ps1 }] }
"#;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("isothrum-test-{name}-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&d));
        d
    }

    #[test]
    fn a_lab_of_containers_gets_its_compose_file() {
        let dir = temp("prepare");
        let spec = prepare(&dir, CONTAINERS, &[]).unwrap();
        assert_eq!(spec.name, "pair");
        let compose = std::fs::read_to_string(dir.join(".isoloom/docker/compose.yml")).unwrap();
        assert!(compose.contains("name: pair"));
        assert!(dir.join("isoloom.yml").is_file());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The lab the editor's compiler writes for a drawing.
    fn compiled(topology: &str) -> String {
        let topo: isothrum_compiler::Topology = serde_json::from_str(topology).unwrap();
        isothrum_compiler::compile(&topo).unwrap().into_parts().0
    }

    #[test]
    fn a_drawn_lab_runs_as_the_compiler_writes_it() {
        // A drawn Linux VM with steps but no port is its OS and its steps: Docker builds it as a
        // container, so a lab of containers and such VMs runs on Docker.
        let yaml = compiled(
            r#"{"name":"drawn","networks":{"office":{"cidr":"10.10.1.0/24"}},"nodes":{
              "web":{"kind":{"container":{"image":"nginx:1.27-alpine"}},"services":[{"port":80,"protocol":"http"}],"networks":{"office":null}},
              "tools":{"kind":{"vm":{"os":"debian@12","provision":["provision/tools.sh"]}},"networks":{"office":null}},
              "user":{"kind":{"container":{"image":"alpine:3.20","idle":true}},"access":true,"networks":{"office":null}}}}"#,
        );
        let dir = temp("drawn");
        let own = [("provision/tools.sh".to_owned(), "#!/bin/sh\n".to_owned())];
        let spec = prepare(&dir, &yaml, &own).unwrap();
        assert_eq!(runtime(&dir), Runtime::Docker);
        assert!(spec.machines["tools"].docker.is_some());
        assert!(dir.join(".isoloom/docker/build/tools/Dockerfile").is_file());
        assert_eq!(lab_spec(&dir).unwrap().machines.len(), 3);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_drawn_service_vm_falls_back_from_docker_to_the_vms() {
        // Its steps install a service (a port) and nothing says how it runs in the foreground:
        // Isoloom refuses it as a container, so the lab runs with VMs and containers (hybrid).
        let yaml = compiled(
            r#"{"name":"drawn","networks":{"office":{"cidr":"10.10.1.0/24"}},"nodes":{
              "web":{"kind":{"container":{"image":"nginx:1.27-alpine"}},"services":[{"port":80,"protocol":"http"}],"networks":{"office":null}},
              "db":{"kind":{"vm":{"os":"debian@12","provision":["provision/db.sh"]}},"services":[{"port":5432}],"networks":{"office":null}},
              "user":{"kind":{"container":{"image":"alpine:3.20","idle":true}},"access":true,"networks":{"office":null}}}}"#,
        );
        let (rt, files) = generated(&lab_of(&yaml).unwrap()).unwrap();
        assert_eq!(rt, Runtime::Hybrid);
        assert!(
            files
                .iter()
                .any(|f| f.path == ".isoloom/hybrid/Vagrantfile")
        );
        // Only VMs: Vagrant.
        let vms = compiled(
            r#"{"name":"drawn","networks":{"office":{"cidr":"10.10.1.0/24"}},"nodes":{
              "db":{"kind":{"vm":{"os":"debian@12","provision":["provision/db.sh"]}},"services":[{"port":5432}],"networks":{"office":null}},
              "user":{"kind":{"vm":{"os":"kali"}},"access":true,"networks":{"office":null}}}}"#,
        );
        let (rt_vms, _) = generated(&lab_of(&vms).unwrap()).unwrap();
        assert_eq!(rt_vms, Runtime::Vagrant);
    }

    #[test]
    fn a_lab_with_vms_runs_with_vagrant() {
        let (rt, files) = generated(&lab_of(WITH_VM).unwrap()).unwrap();
        assert_eq!(rt, Runtime::Hybrid);
        assert!(
            files
                .iter()
                .any(|f| f.path == ".isoloom/hybrid/Vagrantfile")
        );
        let only_vm = WITH_VM.replace(
            "    app: { image: \"alpine:3.20\" }\n",
            "    os: windows@11\n",
        );
        let (rt_only_vm, _) = generated(&lab_of(&only_vm).unwrap()).unwrap();
        assert_eq!(rt_only_vm, Runtime::Vagrant);
        // Prepared: the runtime is recorded and the Vagrantfile found (when Vagrant is installed).
        let dir = temp("vm");
        match prepare(&dir, WITH_VM, &[]) {
            Ok(_) => {
                assert_eq!(runtime(&dir), Runtime::Hybrid);
                assert!(prepared(&dir));
            }
            Err(e) => assert!(!has("vagrant") && e.to_string().contains("Vagrant"), "{e}"),
        }
        drop(std::fs::remove_dir_all(dir));
    }

    #[test]
    fn the_labs_own_files_stay_inside_it() {
        let dir = temp("own");
        prepare(
            &dir,
            CONTAINERS,
            &[("configs/r1.cfg".into(), "hostname r1\n".into())],
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("configs/r1.cfg")).unwrap(),
            "hostname r1\n"
        );
        for bad in ["../escape.sh", "/etc/passwd", "a/../../b"] {
            assert!(
                prepare(&dir, CONTAINERS, &[(bad.into(), String::new())]).is_err(),
                "{bad}"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_invalid_spec_is_refused_with_its_problems() {
        let err = prepare(
            &temp("bad"),
            "version: 1\nname: x\nnetworks: {}\nmachines: {}\n",
            &[],
        );
        assert!(matches!(err, Err(RunError::Spec(_))));
    }
}
