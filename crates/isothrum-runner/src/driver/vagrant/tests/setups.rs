//! The fixture's manifest and options as the tests vary them: its published ports, a VM added,
//! a run on ESXi.

use super::*;

/// The fixture's manifest with `change` made to each published port's `publish`.
pub(super) fn each_publish(change: impl Fn(&mut serde_json::Map<String, serde_json::Value>)) -> String {
    let mut v: serde_json::Value = serde_json::from_str(WEB_VM).expect("the fixture parses");
    for machine in v["machines"].as_array_mut().into_iter().flatten() {
        for port in machine.get_mut("ports").and_then(serde_json::Value::as_array_mut).into_iter().flatten() {
            if let Some(publish) = port.get_mut("publish").and_then(serde_json::Value::as_object_mut) {
                change(publish);
            }
        }
    }
    serde_json::to_string(&v).expect("the manifest serializes")
}

/// The fixture's manifest with each published port fixed at its own number (`publish: { fixed:
/// true }` in the spec, or Isoloom before its ports were ephemeral): no port to pick.
pub(super) fn fixed_manifest() -> String {
    each_publish(|publish| {
        publish.insert("mode".to_owned(), serde_json::Value::from("fixed"));
    })
}

/// The fixture's manifest (its published port ephemeral, as Isoloom writes a VM's, and
/// `ISOLOOM_PUBLISH_FIXED` declared), with each published port's `preferred` number dropped — an
/// ephemeral port Isoloom emits with no `publish:` number of its own, so its host port can only
/// come from the runner's pick.
pub(super) fn ephemeral_no_preferred_manifest() -> String {
    each_publish(|publish| {
        publish.remove("preferred");
    })
}

/// The fixture with a VM added (a copy of `web` under another define), as Isoloom regenerates a
/// run with a machine added.
pub(super) fn with_vm_added(define: &str) -> String {
    let mut m: serde_json::Value = serde_json::from_str(WEB_VM).unwrap();
    let mut added = m["machines"][0].clone();
    added["name"] = define.into();
    added["native"]["vagrant"] = define.into();
    added["ports"] = serde_json::json!([]);
    m["machines"].as_array_mut().unwrap().push(added);
    m.to_string()
}

/// The run's main secret and the ESXi host's password.
pub(super) fn esxi_secrets() -> Secrets {
    let mut secrets = Secrets::new();
    secrets.insert("DEMO_SECRET", SecretValue::new(SECRET));
    secrets.insert("ESXI_PASSWORD", SecretValue::new("pw"));
    secrets
}

/// A test's runner on Linux whose Vagrant has the ESXi provider's plugin.
pub(super) fn with_esxi_plugin() -> super::Fixture {
    let f = super::fixture(HostOs::Linux);
    f.engine().plugins.push("vagrant-vmware-esxi".into());
    f
}

/// The options of a run on the ESXi host at 172.16.1.20, with `secrets`.
pub(super) fn on_esxi(secrets: Secrets) -> RunOptions {
    RunOptions::new()
        .with_provider(isoloom_manifest::Provider::VmwareEsxi)
        .with_param("ESXI_HOSTNAME", "172.16.1.20")
        .with_secrets(secrets)
}
