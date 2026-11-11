use std::io::Read;

use flate2::read::GzDecoder;

use super::*;

/// Every entry of the archive at `path`: its path, and whether it is a link.
fn listed(path: &Path) -> Vec<(String, bool)> {
    let mut tar = tar::Archive::new(GzDecoder::new(File::open(path).unwrap()));
    tar.entries()
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let link = e.header().entry_type().is_symlink();
            (e.path().unwrap().to_string_lossy().trim_end_matches('/').to_owned(), link)
        })
        .collect()
}

fn file(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// The names in the archive of the project at `root`, with the outputs `outputs`, in order.
fn archived(root: &Path, outputs: &[&str]) -> Vec<String> {
    let state = tempfile::tempdir().unwrap();
    let out = state.path().join(ARCHIVE);
    let outputs: Vec<String> = outputs.iter().map(|o| (*o).to_owned()).collect();
    write(root, &outputs, &out).unwrap();
    listed(&out).into_iter().map(|(n, _)| n).collect()
}

#[test]
fn the_walk_never_enters_what_no_copy_takes_even_when_outputs_go_in() {
    let filter = Filter::new("", &[".isoloom/docker"]);
    let out = Visit { add: false, descend: false };
    for rel in [
        ".git",
        "src/.git",
        "a/b/.vagrant",
        ".terraform",
        ".isoloom/docker/.terraform",
        "infra/env.tfstate.d",
    ] {
        assert_eq!(filter.visit(rel, true), out, "{rel}");
    }
    // A `.isoloom*` folder further down is the project's: Isoloom writes none there (nor a
    // `.isoloom-reach` anywhere).
    assert_eq!(filter.visit("infra/.isoloom-reach", true), Visit { add: true, descend: true });
    // An output a guest reads goes in; Isoloom's other outputs are walked only for it.
    assert_eq!(filter.visit(".isoloom/docker", true), Visit { add: true, descend: true });
    assert_eq!(filter.visit(".isoloom", true), Visit { add: false, descend: true });
    assert_eq!(filter.visit(".isoloom-project-env.tgz", false), out);
}

#[test]
fn an_ignored_folder_is_walked_only_when_a_rule_may_bring_something_back() {
    let plain = Filter::new("build/\n", &[]);
    assert_eq!(plain.visit("build", true), Visit { add: false, descend: false });
    let back = Filter::new("build/\n!build/keep.txt\n", &[]);
    assert_eq!(back.visit("build", true), Visit { add: false, descend: true });
    assert!(back.visit("build/keep.txt", false).add);
    assert!(!back.visit("build/out.o", false).add);
}

#[test]
fn terraform_state_stays_out_at_any_depth() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    for rel in ["", "infra/", "a/b/"] {
        file(root, &format!("{rel}main.tf"), "terraform {}\n");
        file(root, &format!("{rel}terraform.tfstate"), "{\"secret\": \"pw\"}\n");
        file(root, &format!("{rel}terraform.tfstate.backup"), "{\"secret\": \"pw\"}\n");
        file(root, &format!("{rel}.terraform.tfstate.lock.info"), "{}\n");
        file(root, &format!("{rel}crash.log"), "panic\n");
        file(root, &format!("{rel}.terraform/providers/x"), "binary\n");
    }
    let names = archived(root, &[]);
    assert_eq!(names, ["a", "infra", "main.tf", "a/b", "a/b/main.tf", "infra/main.tf"]);
}

#[test]
fn isoloom_outputs_go_in_only_when_listed_and_without_their_tools_state() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    file(root, "isoloom.yml", "version: 1\n");
    file(root, ".isoloom/.gitignore", "*\n");
    file(root, ".isoloom/resolved.json", "{}\n");
    file(root, ".isoloom/docker/compose.yml", "name: x\n");
    file(root, ".isoloom/docker/checks/web.sh", "true\n");
    file(root, ".isoloom/docker/.terraform/providers/x", "binary\n");
    file(root, ".isoloom/docker/terraform.tfstate", "{}\n");
    file(root, ".isoloom/cloud-vm/aws/main.tf", "terraform {}\n");
    file(root, ".isoloom/cloud-vm/aws/terraform.tfstate", "{}\n");
    file(root, ".isoloom-project-env.tgz", "old\n");
    // Isoloom's outputs are the top's: a folder of that name further down is the project's.
    file(root, "docs/.isoloom/note.md", "hello\n");
    let names = archived(root, &[".isoloom/docker"]);
    assert_eq!(
        names,
        [
            "docs",
            "isoloom.yml",
            ".isoloom/docker",
            ".isoloom/docker/checks",
            ".isoloom/docker/compose.yml",
            ".isoloom/docker/checks/web.sh",
            "docs/.isoloom",
            "docs/.isoloom/note.md",
        ]
    );
    // Without outputs, none of Isoloom's goes in.
    assert_eq!(archived(root, &[]), ["docs", "isoloom.yml", "docs/.isoloom", "docs/.isoloom/note.md"]);
}

#[test]
fn version_control_stays_out_at_any_depth() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    file(root, ".git/config", "[core]\n");
    file(root, "vendor/lib/.git/HEAD", "ref: x\n");
    file(root, "vendor/lib/lib.rs", "\n");
    file(root, ".isoloom/docker/.git/HEAD", "ref: x\n");
    file(root, ".isoloom/docker/compose.yml", "name: x\n");
    let names = archived(root, &[".isoloom/docker"]);
    assert_eq!(
        names,
        ["vendor", ".isoloom/docker", ".isoloom/docker/compose.yml", "vendor/lib", "vendor/lib/lib.rs"]
    );
}

#[test]
fn the_projects_own_rules_bring_back_what_they_left_out_but_never_what_no_copy_takes() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    file(root, ".isoloomignore", "*.log\n!keep.log\nbuild/\n!build/web/\n!*.tfstate\n!.git/\n");
    file(root, "keep.log", "kept\n");
    file(root, "noise.log", "noise\n");
    file(root, "build/out.o", "\n");
    file(root, "build/web/index.html", "hello\n");
    file(root, "terraform.tfstate", "{}\n");
    file(root, ".git/config", "[core]\n");
    // `.isoloomignore` is one of the top's `.isoloom*` entries: Isoloom's, so left out too.
    assert_eq!(archived(root, &[]), ["keep.log", "build/web", "build/web/index.html"]);
}

#[cfg(unix)]
#[test]
fn a_folder_no_copy_takes_is_never_read() {
    use std::os::unix::fs::PermissionsExt;
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    file(root, "isoloom.yml", "version: 1\n");
    file(root, ".isoloom/docker/compose.yml", "name: x\n");
    let shut = [".git", ".isoloom/docker/.terraform", "infra/.vagrant"];
    for rel in shut {
        file(root, &format!("{rel}/x"), "\n");
        std::fs::set_permissions(root.join(rel), std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    // Whoever may read a folder anyway (root) can't tell whether the walk reads it.
    let unreadable = shut.iter().all(|rel| std::fs::read_dir(root.join(rel)).is_err());
    let state = tempfile::tempdir().unwrap();
    let written = write(root, &[".isoloom/docker".to_owned()], &state.path().join(ARCHIVE));
    for rel in shut {
        std::fs::set_permissions(root.join(rel), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    if unreadable {
        written.unwrap();
    }
}

#[test]
fn the_archive_holds_the_project_as_the_generated_files_copy_it() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    file(root, "isoloom.yml", "version: 1\n");
    file(root, ".isoloom/docker/compose.yml", "name: x\n");
    file(root, ".isoloom/vagrant/Vagrantfile", "Vagrant.configure(2)\n");
    file(root, "build/web/index.html", "hello\n");
    file(root, "notes.log", "noise\n");
    file(root, ".git/config", "[core]\n");
    file(root, "src/.vagrant/key", "secret\n");
    file(root, ".isoloom-project-env.tgz", "old\n");
    file(root, ".isoloomignore", "*.log\n");
    #[cfg(unix)]
    std::os::unix::fs::symlink("build", root.join("latest")).unwrap();
    let state = tempfile::tempdir().unwrap();
    let out = state.path().join(ARCHIVE);
    write(root, &[".isoloom/docker".to_owned()], &out).unwrap();
    let names: Vec<String> = listed(&out).into_iter().map(|(n, _)| n).collect();
    let mut expected = vec!["build", "isoloom.yml"];
    if cfg!(unix) {
        expected.push("latest");
    }
    expected.extend(["src", ".isoloom/docker", ".isoloom/docker/compose.yml", "build/web", "build/web/index.html"]);
    assert_eq!(names, expected);
    #[cfg(unix)]
    assert!(listed(&out).iter().any(|(n, link)| n == "latest" && *link), "a link goes in as a link");
    // The file reads back whole, and the archive is the run's alone.
    let mut tar = tar::Archive::new(GzDecoder::new(File::open(&out).unwrap()));
    let mut page = String::new();
    for e in tar.entries().unwrap() {
        let mut e = e.unwrap();
        if e.path().unwrap().ends_with("index.html") {
            e.read_to_string(&mut page).unwrap();
        }
    }
    assert_eq!(page, "hello\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&out).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert!(!out.with_file_name(format!("{ARCHIVE}.part")).exists());
}

#[test]
fn a_missing_project_is_an_error_and_leaves_no_partial_archive() {
    let state = tempfile::tempdir().unwrap();
    let out = state.path().join(ARCHIVE);
    assert!(write(&state.path().join("no-such-project"), &[], &out).is_err());
    assert!(!out.exists() && !out.with_file_name(format!("{ARCHIVE}.part")).exists());
}

/// Regression: a name that isn't UTF-8 was made readable, so the archive asked for a path that
/// isn't there and failed with a `NotFound` naming it.
#[cfg(unix)]
#[test]
fn a_name_that_isnt_utf8_is_refused_by_name() {
    use std::os::unix::ffi::OsStringExt;
    let name = std::ffi::OsString::from_vec(b"bad\xff.txt".to_vec());
    let err = utf8_name(Path::new("/p/docs"), name).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(
        err.to_string().ends_with("has a name that isn't UTF-8") && err.to_string().starts_with("/p/docs/bad"),
        "{err}"
    );
    assert_eq!(utf8_name(Path::new("/p"), "café.txt".into()).unwrap(), "café.txt");
}
