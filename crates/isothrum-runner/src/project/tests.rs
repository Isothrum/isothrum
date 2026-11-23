use super::*;

fn folder(files: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary folder");
    for f in files {
        std::fs::write(dir.path().join(f), format!("# {f}\n")).expect("a file written");
    }
    dir
}

fn detected(files: &[&str]) -> Result<Format> {
    Format::detect(folder(files).path())
}

#[test]
fn each_format_is_recognised_by_its_own_files() {
    for (files, format) in [
        (&["compose.yaml"][..], Format::Compose),
        (&["compose.yml"][..], Format::Compose),
        (&["docker-compose.yaml"][..], Format::Compose),
        (&["docker-compose.yml"][..], Format::Compose),
        (&["Vagrantfile", "setup.sh"][..], Format::Vagrant),
        (&["main.tf", "variables.tf"][..], Format::Terraform),
        (&["main.tf.json"][..], Format::Terraform),
    ] {
        assert_eq!(detected(files).expect("detected"), format, "{files:?}");
    }
}

#[test]
fn a_folder_of_no_format_or_of_several_is_refused_with_the_fix() {
    let reason = match detected(&["README.md"]) {
        Err(e @ Error::Project { .. }) if e.code() == crate::ErrorCode::ProjectInvalid => e.to_string(),
        other => panic!("not refused as no project: {other:?}"),
    };
    assert!(
        reason
            .contains("it holds no Compose file (compose.yaml, compose.yml, docker-compose.yaml, docker-compose.yml), Vagrantfile or Terraform module (*.tf)"),
        "every file a format takes is named: {reason}"
    );
    let both = detected(&["compose.yaml", "Vagrantfile"]).expect_err("two formats");
    assert!(both.to_string().contains("compose, vagrant"), "{both}");
    assert!(both.to_string().contains("name the one to run"), "{both}");
}

#[test]
fn compose_reads_one_file_in_its_own_order_of_precedence() {
    let order = ["compose.yaml", "compose.yml", "docker-compose.yaml", "docker-compose.yml"];
    for (i, first) in order.iter().enumerate() {
        for later in &order[i + 1..] {
            let dir = folder(&[later, first]);
            let files = Format::Compose.files_in(dir.path()).expect("listed");
            assert_eq!(files, vec![dir.path().join(first)], "{first} before {later}");
        }
    }
}

#[test]
fn a_named_format_runs_a_folder_that_holds_several() {
    let dir = folder(&["compose.yaml", "Vagrantfile"]);
    let project = Project::open(dir.path(), Some(Format::Vagrant)).expect("opened");
    assert_eq!(project.format, Format::Vagrant);
    assert_eq!(project.files, vec![dir.path().join("Vagrantfile")]);
    let wrong = Project::open(dir.path(), Some(Format::Terraform)).expect_err("no module");
    assert!(wrong.to_string().contains("no terraform project"), "{wrong}");
}

#[test]
fn the_digest_follows_the_files_and_only_them() {
    let dir = folder(&["main.tf", ".terraform.lock.hcl", "notes.txt"]);
    let project = Project::open(dir.path(), None).expect("opened");
    assert_eq!(project.files.len(), 2, "{:?}", project.files);
    let before = project.digest().expect("digest");
    std::fs::write(dir.path().join("notes.txt"), "changed").expect("written");
    assert_eq!(project.digest().expect("digest"), before, "a file the format doesn't read");
    std::fs::write(dir.path().join("main.tf"), "resource \"x\" \"y\" {}").expect("written");
    assert_ne!(project.digest().expect("digest"), before);
}

#[test]
fn a_missing_folder_is_no_project() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let e = Project::open(&dir.path().join("missing"), None).expect_err("missing");
    assert_eq!(e.code(), crate::ErrorCode::ProjectInvalid);
}

#[test]
fn every_format_reads_back_from_its_name() {
    for f in Format::ALL {
        assert_eq!(Format::named(f.as_str()), Some(f));
        assert_eq!(serde_json::to_value(f).expect("json"), f.as_str());
    }
}

/// Regression: a folder that couldn't be listed read as holding no Terraform file, so a module
/// the runner may not read was refused as no project, not as the error it is.
#[cfg(unix)]
#[test]
fn a_folder_that_cant_be_listed_is_an_io_error_never_an_empty_folder() {
    use std::os::unix::fs::PermissionsExt;
    let dir = folder(&["main.tf"]);
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).expect("a folder");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("locked");
    let listed = terraform_files(&locked);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).expect("unlocked");
    if std::fs::read_dir(dir.path().join("locked")).is_ok() && listed.is_ok() && nix_root() {
        return; // root reads any folder: nothing to refuse
    }
    assert!(matches!(listed, Err(Error::Io { .. })), "{listed:?}");
    assert_eq!(
        terraform_files(&dir.path().join("missing")).expect("a missing folder"),
        Vec::<std::path::PathBuf>::new()
    );
}

/// Whether the tests run as root, who reads a folder whatever its mode.
#[cfg(unix)]
fn nix_root() -> bool {
    std::fs::read_to_string("/proc/self/status").is_ok_and(|s| s.lines().any(|l| l.starts_with("Uid:") && l.split_whitespace().nth(1) == Some("0")))
}
