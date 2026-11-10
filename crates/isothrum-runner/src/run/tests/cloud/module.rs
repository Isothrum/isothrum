//! A run's own Terraform module (cloud-services): copied into the run's state folder at `up`,
//! destroyed by `down` as it was applied, and forgotten after it.

use super::*;

/// The fixture as cloud-services': its module the project's `terraform/`, copied by the run
/// (every time at `up`, only when missing at `down`); no `.tf` beside the manifest.
fn own_module() -> String {
    let mut m: serde_json::Value = serde_json::from_str(&without_guest_delivery()).unwrap();
    let copy = |when: Option<&str>| {
        let mut with = serde_json::json!({"from": "../../../terraform"});
        if let Some(w) = when {
            with["when"] = serde_json::json!(w);
        }
        serde_json::json!({"id": "copy-module", "action": "terraform.copy_module", "with": with})
    };
    m["lifecycle"]["up"].as_array_mut().unwrap().insert(0, copy(None));
    m["lifecycle"]["down"].as_array_mut().unwrap().insert(0, copy(Some("missing")));
    m.to_string()
}

/// The account, the project's module written, nothing beside the manifest.
fn with_own_module() -> (Account, PathBuf) {
    let a = account_of(&own_module(), 0);
    std::fs::remove_file(a.folder.target_dir().join("main.tf")).unwrap();
    let module = a.folder.target_dir().join("../../../terraform");
    std::fs::create_dir_all(&module).unwrap();
    std::fs::write(module.join("main.tf"), "resource \"aws_s3_bucket\" \"site\" {}").unwrap();
    std::fs::write(module.join("old.tf"), "output \"x\" {}").unwrap();
    (a, module)
}

/// The start of `a`'s run, refused as `manifest_invalid` before anything runs: its error.
async fn refused_start(a: &Account) -> crate::Error {
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let err = run.up(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::ManifestInvalid, "{err}");
    err
}

#[tokio::test(start_paused = true)]
async fn the_labs_own_module_is_copied_into_the_run_and_destroyed_as_applied() {
    let (a, module) = with_own_module();
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    run.up(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap();
    let copy = a.runner.store.run_dir(run.name()).unwrap().join("module");
    assert!(copy.join("main.tf").is_file() && copy.join("old.tf").is_file(), "the start copied the module");
    // The project changes after the apply: the destroy takes the module as it was applied.
    std::fs::write(module.join("main.tf"), "changed").unwrap();
    run.down(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap();
    let destroyed = a.cloud.destroyed_module.lock().unwrap().clone();
    assert_eq!(destroyed.as_deref(), Some("resource \"aws_s3_bucket\" \"site\" {}"));
    assert!(!copy.exists(), "a successful down forgets the copy");
}

#[tokio::test(start_paused = true)]
async fn a_new_start_takes_the_module_as_it_is_now() {
    let (a, module) = with_own_module();
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    run.up(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap();
    std::fs::remove_file(module.join("old.tf")).unwrap();
    std::fs::write(module.join("main.tf"), "changed").unwrap();
    let again = opts("eu-west-3", "demo").with_occupied(crate::Occupied::Replace);
    run.up(&again, &Recorder::new()).await.unwrap();
    let copy = a.runner.store.run_dir(run.name()).unwrap().join("module");
    assert_eq!(std::fs::read_to_string(copy.join("main.tf")).unwrap(), "changed");
    assert!(!copy.join("old.tf").exists(), "a file removed from the module isn't applied any more");
}

#[tokio::test(start_paused = true)]
async fn a_module_that_is_missing_refuses_the_start_before_anything_runs() {
    let (a, module) = with_own_module();
    std::fs::remove_dir_all(&module).unwrap();
    let err = refused_start(&a).await;
    assert_eq!(
        err.to_string(),
        "copy-module.with.from: `../../../terraform` isn't a folder inside the run's project"
    );
    assert!(a.fake.last("terraform").is_none(), "nothing ran");
}

/// Regression: the module checked was the generated one beside the manifest, whatever the copy
/// named, so a module without a Terraform file was reported at the wrong folder.
#[tokio::test(start_paused = true)]
async fn a_module_without_a_terraform_file_is_named_where_it_is() {
    let (a, module) = with_own_module();
    for file in ["main.tf", "old.tf"] {
        std::fs::remove_file(module.join(file)).unwrap();
    }
    let err = refused_start(&a).await;
    let checked = std::fs::canonicalize(&module).unwrap();
    assert!(err.to_string().starts_with(&format!("the Terraform module {}:", checked.display())), "{err}");
}

/// Regression: a `with.from` that couldn't be filled was dropped, and the run was told its
/// generated files had no Terraform file.
#[tokio::test(start_paused = true)]
async fn a_copy_whose_folder_cant_be_filled_says_so() {
    let mut m: serde_json::Value = serde_json::from_str(&own_module()).unwrap();
    m["lifecycle"]["up"][0]["with"]["from"] = "${params.AWS_PROFILE}".into();
    let a = account_of(&m.to_string(), 0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let mut no_profile = opts("eu-west-3", "demo");
    no_profile.params.remove("AWS_PROFILE");
    let err = run.up(&no_profile, &Recorder::new()).await.unwrap_err();
    assert!(matches!(err, crate::Error::Unbound { .. }), "{err}");
    assert!(err.to_string().contains("copy-module.with.from"), "{err}");
    assert!(a.fake.last("terraform").is_none(), "nothing ran");
}

#[tokio::test(start_paused = true)]
async fn a_copy_with_a_when_it_doesnt_know_is_a_malformed_manifest() {
    let mut m: serde_json::Value = serde_json::from_str(&own_module()).unwrap();
    m["lifecycle"]["up"][0]["with"]["when"] = "always".into();
    let a = account_of(&m.to_string(), 0);
    let module = a.folder.target_dir().join("../../../terraform");
    std::fs::create_dir_all(&module).unwrap();
    std::fs::write(module.join("main.tf"), "").unwrap();
    let err = refused_start(&a).await;
    assert_eq!(err.to_string(), "copy-module.with.when: `always`: only `missing` is a `when`");
    assert!(a.fake.last("terraform apply").is_none(), "nothing applied");
}
