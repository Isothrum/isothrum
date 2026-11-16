//! A console into a module's VM: `ssh` at the address its state says, as the user its outputs
//! name, with the run's key (else the runner's), its host key pinned first.

use super::*;
use crate::process::fake::CallView as _;

/// A state of one AWS VM, `web`, at 203.0.113.7.
fn web_at_its_address() -> String {
    serde_json::json!({ "format_version": "1.0", "values": { "root_module": { "resources": [{
        "address": "aws_instance.web", "mode": "managed", "type": "aws_instance", "name": "web",
        "values": { "id": "i-0web", "public_ip": "203.0.113.7" }
    }]}}})
    .to_string()
}

/// The module's outputs: web's address and its user.
const OUTPUTS: &str =
    r#"{"address_web":{"sensitive":false,"type":"string","value":"203.0.113.7"},"ssh_user_web":{"sensitive":false,"type":"string","value":"admin"}}"#;

fn script() -> FakeProcesses {
    FakeProcesses::script()
        .expect("terraform show -json -no-color", Reply::ok(&web_at_its_address()))
        .expect("terraform output -json -no-color", Reply::ok(OUTPUTS))
        .expect(
            "ssh-keyscan -T 10 203.0.113.7",
            Reply::ok("203.0.113.7 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl"),
        )
        .build()
}

#[tokio::test]
async fn a_vms_console_is_ssh_as_the_user_the_outputs_name_with_the_runs_key_and_its_host_pinned() {
    let lab = lab().with_state();
    let fake = script();
    let console = with_cx_given(&lab, &fake, &[("ssh_private_key_file", "/keys/lab")], async |cx| {
        Terraform.console(cx, "web", None).await
    })
    .await
    .expect("a console");
    let spec = crate::process::ProcessSpec::new(&console.program, console.args.clone());
    assert_eq!(console.program, "ssh");
    assert_eq!(spec.flag("-i"), Some("/keys/lab"), "the run's own key: {:?}", console.args);
    assert!(
        spec.has_run(&["-o", "BatchMode=yes"]) && spec.has_run(&["-o", "StrictHostKeyChecking=yes"]),
        "{:?}",
        console.args
    );
    assert_eq!(console.args.last().map(String::as_str), Some("admin@203.0.113.7"));
    assert!(spec.has("-t"), "with a terminal: {:?}", console.args);
}

#[tokio::test]
async fn a_console_without_a_run_key_signs_in_with_the_runners_and_a_user_asked_wins() {
    let lab = lab().with_state();
    let fake = script();
    let console = with_cx(&lab, &fake, async |cx| Terraform.console(cx, "web", Some("root")).await)
        .await
        .expect("a console");
    let spec = crate::process::ProcessSpec::new(&console.program, console.args.clone());
    assert_eq!(spec.flag("-i"), Some("/nowhere/id_ed25519"), "the runner's own key");
    assert_eq!(console.args.last().map(String::as_str), Some("root@203.0.113.7"));
}

/// A state of an Azure Windows VM, `dc`, at 10.9.0.20 on the module's network, and of a Linux one,
/// `web`, with no console the runner reaches.
fn dc_beside_web() -> String {
    serde_json::json!({ "format_version": "1.0", "values": { "root_module": { "resources": [
        { "address": "azurerm_windows_virtual_machine.dc", "mode": "managed", "type": "azurerm_windows_virtual_machine",
          "name": "dc", "values": { "id": "/vm/dc", "private_ip_address": "10.9.0.20" } },
        { "address": "aws_instance.web", "mode": "managed", "type": "aws_instance",
          "name": "web", "values": { "id": "i-0web", "public_ip": "203.0.113.7" } }
    ]}}})
    .to_string()
}

/// The outputs of a module giving its Windows password, and its Linux machine as where to stand.
const WINDOWS_OUTPUTS: &str = r#"{"ip":{"sensitive":false,"type":"string","value":"203.0.113.7"},"address_web":{"sensitive":false,"type":"string","value":"203.0.113.7"},"ssh_user_web":{"sensitive":false,"type":"string","value":"admin"},"windows_password":{"sensitive":true,"type":"string","value":"Pa55w0rd!"}}"#;

#[tokio::test]
async fn a_windows_vms_screen_is_remote_desktop_tunnelled_through_the_modules_linux_machine() {
    let lab = lab().with_state();
    let fake = FakeProcesses::script()
        .expect("terraform show -json -no-color", Reply::ok(&dc_beside_web()))
        .expect("terraform output -json -no-color", Reply::ok(WINDOWS_OUTPUTS))
        .build();
    let console = with_cx_given(&lab, &fake, &[("ssh_private_key_file", "/keys/lab")], async |cx| {
        Terraform.reach_graphical(cx, "dc").await
    })
    .await
    .expect("a screen");
    assert_eq!(console.protocol, crate::graphical::ConsoleProtocol::Rdp);
    assert_eq!(console.user.as_deref(), Some("Administrator"), "no `windows_user` output");
    assert_eq!(console.password.as_ref().map(crate::secrets::SecretValue::expose), Some("Pa55w0rd!"));
    let to = console.through.expect("through a tunnel");
    assert_eq!((to.via.target().as_str(), to.host.as_str(), to.port), ("admin@203.0.113.7", "10.9.0.20", 3389));
    assert_eq!(to.key.as_deref(), Some(std::path::Path::new("/keys/lab")), "the run's own key");
}

#[tokio::test]
async fn a_vm_whose_module_names_no_screen_has_none_saying_why_and_a_machine_not_in_the_state_is_unknown() {
    let lab = lab().with_state();
    let fake = FakeProcesses::script()
        .expect_times("terraform show -json -no-color", 2, Reply::ok(&dc_beside_web()))
        .expect("terraform output -json -no-color", Reply::ok(WINDOWS_OUTPUTS))
        .build();
    let web = with_cx(&lab, &fake, async |cx| Terraform.reach_graphical(cx, "web").await)
        .await
        .expect_err("no screen for a Linux VM");
    assert!(
        matches!(&web, Error::NoConsole { machine, why } if machine == "web" && why.starts_with("a `aws_instance` gives no screen")),
        "{web:?}"
    );
    let nope = with_cx(&lab, &fake, async |cx| Terraform.reach_graphical(cx, "nope").await)
        .await
        .expect_err("no such machine");
    assert_eq!(nope.code(), ErrorCode::MachineUnknown, "{nope}");
}
