//! The params of a Proxmox run that exists: replayed from its record, a param its manifest
//! gained since filled with its default.

use isoloom_manifest::HostOs;

use super::{opts, server};
use crate::Recorder;
use crate::testing::SOLO_WEB_PROXMOX;

#[tokio::test(start_paused = true)]
async fn an_update_from_a_manifest_with_a_new_param_gives_it_its_default() {
    // Regression: the params of a run that exists were the recorded ones only, so a param the
    // regenerated manifest adds took no value and `${params.<name>}` was unbound.
    let s = server(HostOs::Linux);
    let run = s.runner.open(&s.manifest, None).unwrap();
    run.up(&opts("pve-run"), &Recorder::new()).await.unwrap();
    let mut m: serde_json::Value = serde_json::from_str(SOLO_WEB_PROXMOX).unwrap();
    let params = m["params"].as_array_mut().unwrap();
    let mut added = params.iter().find(|p| p["name"] == "datastore").unwrap().clone();
    added["name"] = "vm_tags".into();
    added["default"] = "lab".into();
    added["deliver"] = serde_json::json!([{ "via": "tf_var", "var": "vm_tags" }]);
    params.push(added);
    std::fs::write(&s.manifest, m.to_string()).unwrap();
    let regenerated = s.runner.open(&s.manifest, None).unwrap();
    regenerated.update(&opts("pve-run"), &Recorder::new()).await.unwrap();
    let apply = s.fake.last("terraform apply").unwrap();
    assert_eq!(apply.env("TF_VAR_vm_tags"), Some("lab"));
}
