use std::path::Path;

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
    let none = detected(&["README.md"]).expect_err("nothing to run");
    assert_eq!(none.code(), crate::ErrorCode::ProjectInvalid);
    assert!(none.to_string().contains("no Compose file"), "{none}");
    let both = detected(&["compose.yaml", "Vagrantfile"]).expect_err("two formats");
    assert!(both.to_string().contains("compose, vagrant"), "{both}");
    assert!(both.to_string().contains("name the one to run"), "{both}");
}

#[test]
fn compose_reads_one_file_in_its_own_order_of_precedence() {
    let dir = folder(&["docker-compose.yml", "compose.yml"]);
    let files = Format::Compose.files_in(dir.path()).expect("listed");
    assert_eq!(files, vec![dir.path().join("compose.yml")]);
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
    let e = Project::open(Path::new("/no/such/folder/anywhere"), None).expect_err("missing");
    assert_eq!(e.code(), crate::ErrorCode::ProjectInvalid);
}

#[test]
fn every_format_reads_back_from_its_name() {
    for f in Format::ALL {
        assert_eq!(Format::named(f.as_str()), Some(f));
        assert_eq!(serde_json::to_value(f).expect("json"), f.as_str());
    }
}
