use std::path::Path;

use super::*;

/// A run's project: `isoloom.yml`, its module `terraform/` (a variable file, a sub-module, and
/// what no copy takes: a provider cache, a state, crash logs, version control and Vagrant's
/// state), and the manifest's folder.
fn project() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let module = root.path().join("terraform");
    std::fs::create_dir_all(module.join("modules/site")).unwrap();
    std::fs::create_dir_all(module.join(".terraform/providers")).unwrap();
    for (file, text) in [
        ("main.tf", "resource {}"),
        ("vars.auto.tfvars.json", "{}"),
        ("modules/site/main.tf", "x"),
        ("index.html", "hello"),
        ("terraform.tfstate", "{}"),
        ("terraform.tfstate.backup", "{}"),
        (".terraform/providers/aws", "binary"),
        ("crash.log", "TF_VAR_db_password=hunter2"),
        ("crash.20261111.log", "x"),
        ("modules/site/crash.log", "x"),
        (".git/config", "x"),
        (".vagrant/machines", "x"),
    ] {
        std::fs::create_dir_all(module.join(file).parent().unwrap()).unwrap();
        std::fs::write(module.join(file), text).unwrap();
    }
    let dir = root.path().join(".isoloom/cloud-services");
    std::fs::create_dir_all(&dir).unwrap();
    (root, dir)
}

/// The files under `dir`, relative and sorted.
fn files(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            if e.path().is_dir() {
                walk(base, &e.path(), out);
            } else {
                out.push(e.path().strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// Regression: the copy left out `.terraform` and states only, so a crash log (which can hold a
/// variable's secret), `.git` and `.vagrant` went into the run's state folder.
#[test]
fn the_module_is_copied_without_its_providers_or_state() {
    let (_root, dir) = project();
    let source = inside_project(&dir, "copy", "../../terraform").unwrap();
    let copy = tempfile::tempdir().unwrap();
    copy_tree(&source, copy.path()).unwrap();
    assert_eq!(files(copy.path()), ["index.html", "main.tf", "modules/site/main.tf", "vars.auto.tfvars.json"]);
}

#[test]
fn a_new_copy_replaces_the_terraform_files_and_keeps_what_the_module_wrote() {
    let (_root, dir) = project();
    let source = inside_project(&dir, "copy", "../../terraform").unwrap();
    let copy = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(copy.path().join(".terraform")).unwrap();
    for (file, text) in [
        ("old.tf", "gone"),
        ("main.tf", "stale"),
        (".terraform.lock.hcl", "lock"),
        ("site.zip", "built"),
        (".terraform/x.tf", "cache"),
    ] {
        std::fs::write(copy.path().join(file), text).unwrap();
    }
    forget_terraform_files(copy.path()).unwrap();
    copy_tree(&source, copy.path()).unwrap();
    let now = files(copy.path());
    assert!(!now.contains(&"old.tf".to_owned()), "a file removed from the module isn't applied: {now:?}");
    assert_eq!(std::fs::read_to_string(copy.path().join("main.tf")).unwrap(), "resource {}");
    for kept in [".terraform.lock.hcl", "site.zip", ".terraform/x.tf"] {
        assert!(now.contains(&kept.to_owned()), "{kept}: {now:?}");
    }
}

#[test]
fn a_module_outside_the_project_is_refused() {
    let (root, dir) = project();
    assert!(inside_project(&dir, "copy", "../../terraform").is_ok());
    let refused = |from: &str| inside_project(&dir, "copy", from).unwrap_err().to_string();
    assert_eq!(
        refused("../../../"),
        "copy.with.from: `../../../` isn't a folder inside the run's project",
        "above the project"
    );
    assert!(refused("../../missing").contains("isn't a folder inside"));
    std::fs::write(root.path().join("file.tf"), "").unwrap();
    assert!(refused("../../file.tf").contains("isn't a folder inside"), "a file, not a folder");
}

/// Regression: a symbolic link in the module was followed, so one to a folder above it copied
/// until the stack ran out, and one elsewhere brought in what is outside the project. It is
/// refused, naming it.
#[cfg(unix)]
#[test]
fn a_symbolic_link_in_the_module_is_refused_not_followed() {
    let (root, dir) = project();
    let source = inside_project(&dir, "copy", "../../terraform").unwrap();
    std::os::unix::fs::symlink(root.path().join("terraform/index.html"), source.join("linked.html")).unwrap();
    let copy = tempfile::tempdir().unwrap();
    let err = copy_tree(&source, copy.path()).unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::Unsupported, "{err}");
    assert!(err.to_string().contains("symbolic link") && err.to_string().contains("linked.html"), "{err}");
}

/// Regression: a folder that couldn't be read was a folder with no Terraform file, and the run
/// was told to generate its files again.
#[cfg(unix)]
#[test]
fn a_folder_that_cant_be_read_is_an_error_not_a_module_without_files() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let locked = dir.path().join("module");
    std::fs::create_dir(&locked).unwrap();
    std::fs::write(locked.join("main.tf"), "").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let read = has_terraform_files(&locked);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    let err = read.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::Io, "{err}");
    assert!(
        has_terraform_files(&dir.path().join("missing")).is_ok_and(|has| !has),
        "a missing folder holds none"
    );
}

#[test]
fn terraform_files_are_its_own_extensions() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!has_terraform_files(dir.path()).unwrap());
    std::fs::write(dir.path().join("notes.md"), "").unwrap();
    assert!(!has_terraform_files(dir.path()).unwrap());
    std::fs::write(dir.path().join("main.tf.json"), "{}").unwrap();
    assert!(has_terraform_files(dir.path()).unwrap());
}
