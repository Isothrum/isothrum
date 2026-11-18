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
    let stack = stack().with_state();
    let fake = script();
    let console = with_cx_given(&stack, &fake, &[("ssh_private_key_file", "/keys/stack")], async |cx| {
        Terraform.console(cx, "web", None).await
    })
    .await
    .expect("a console");
    let spec = crate::process::ProcessSpec::new(&console.program, console.args.clone());
    assert_eq!(console.program, "ssh");
    assert_eq!(spec.flag("-i"), Some("/keys/stack"), "the run's own key: {:?}", console.args);
    assert!(
        spec.has_run(&["-o", "BatchMode=yes"]) && spec.has_run(&["-o", "StrictHostKeyChecking=yes"]),
        "{:?}",
        console.args
    );
    assert_eq!(console.args.last().map(String::as_str), Some("admin@203.0.113.7"));
    assert!(spec.has("-t"), "with a terminal: {:?}", console.args);
}

#[tokio::test]
async fn a_console_without_a_run_key_signs_in_with_the_runners_and_becomes_the_user_asked_for() {
    let stack = stack().with_state();
    let fake = script();
    let console = with_cx(&stack, &fake, async |cx| Terraform.console(cx, "web", Some("root")).await)
        .await
        .expect("a console");
    let spec = crate::process::ProcessSpec::new(&console.program, console.args.clone());
    assert_eq!(spec.flag("-i"), Some("/nowhere/id_ed25519"), "the runner's own key");
    let n = console.args.len();
    assert_eq!(
        &console.args[n - 2..],
        ["admin@203.0.113.7".to_owned(), crate::console::shells::login_shell_as("root")],
        "as the module's user, then a login shell as the one asked for"
    );
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
    let stack = stack().with_state();
    let fake = FakeProcesses::script()
        .expect("terraform show -json -no-color", Reply::ok(&dc_beside_web()))
        .expect("terraform output -json -no-color", Reply::ok(WINDOWS_OUTPUTS))
        .build();
    let console = with_cx_given(&stack, &fake, &[("ssh_private_key_file", "/keys/stack")], async |cx| {
        Terraform.reach_graphical(cx, "dc").await
    })
    .await
    .expect("a screen");
    assert_eq!(console.protocol, crate::graphical::ConsoleProtocol::Rdp);
    assert_eq!(console.user.as_deref(), Some("Administrator"), "no `windows_user` output");
    assert_eq!(console.password.as_ref().map(crate::secrets::SecretValue::expose), Some("Pa55w0rd!"));
    let to = console.through.expect("through a tunnel");
    assert_eq!((to.via.target().as_str(), to.host.as_str(), to.port), ("admin@203.0.113.7", "10.9.0.20", 3389));
    assert_eq!(to.key.as_deref(), Some(std::path::Path::new("/keys/stack")), "the run's own key");
}

#[tokio::test]
async fn a_vm_whose_module_names_no_screen_has_none_saying_why_and_a_machine_not_in_the_state_is_unknown() {
    let stack = stack().with_state();
    let fake = FakeProcesses::script()
        .expect_times("terraform show -json -no-color", 2, Reply::ok(&dc_beside_web()))
        .expect("terraform output -json -no-color", Reply::ok(WINDOWS_OUTPUTS))
        .build();
    let web = with_cx(&stack, &fake, async |cx| Terraform.reach_graphical(cx, "web").await)
        .await
        .expect_err("no screen for a Linux VM");
    assert!(
        matches!(&web, Error::NoConsole { machine, why } if machine == "web" && why.starts_with("a `aws_instance` gives no screen")),
        "{web:?}"
    );
    let nope = with_cx(&stack, &fake, async |cx| Terraform.reach_graphical(cx, "nope").await)
        .await
        .expect_err("no such machine");
    assert_eq!(nope.code(), ErrorCode::MachineUnknown, "{nope}");
}

/// A state of a Proxmox VE deployment: its router, at 192.0.2.10 on the node's internal bridge, and
/// `web`, with no address (no guest agent).
fn web_behind_a_router() -> String {
    serde_json::json!({ "format_version": "1.0", "values": { "root_module": { "resources": [
        { "address": "proxmox_virtual_environment_vm.router", "mode": "managed", "type": "proxmox_virtual_environment_vm",
          "name": "router", "values": { "id": "100", "ipv4_addresses": [["127.0.0.1"], ["192.0.2.10"]] } },
        { "address": "proxmox_virtual_environment_vm.web", "mode": "managed", "type": "proxmox_virtual_environment_vm",
          "name": "web", "values": { "id": "101" } }
    ]}}})
    .to_string()
}

/// A module's outputs: its router's address, its user, and the machines behind it.
const ROUTER_OUTPUTS: &str = r#"{"ip":{"sensitive":false,"type":"string","value":"192.0.2.10"},"ssh_user":{"sensitive":false,"type":"string","value":"ops"},"machines":{"sensitive":false,"type":["object",{"web":"string"}],"value":{"web":"10.60.0.10"}}}"#;

/// The node every SSH of the run goes through first.
const NODE_KEY: &str = "node.stack ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

/// The values a run on a node's internal bridge is given: the module's key, and the node to go
/// through first.
const THROUGH_THE_NODE: &[(&str, &str)] = &[("ssh_private_key_file", "/keys/stack"), ("ssh_jump", "root@node.stack")];

/// Scripted tools for a deployment behind its router: the state, the outputs, the node's keys, and `ssh`
/// answering `ssh`.
fn behind_a_router(ssh: Reply) -> FakeProcesses {
    FakeProcesses::script()
        .expect("terraform show -json -no-color", Reply::ok(&web_behind_a_router()))
        .expect("terraform output -json -no-color", Reply::ok(ROUTER_OUTPUTS))
        .expect("ssh-keyscan -T 10 node.stack", Reply::ok(NODE_KEY))
        .allow("ssh -F none", ssh)
        .build()
}

#[tokio::test]
async fn a_command_runs_in_a_vm_behind_the_modules_router_through_the_runs_jump_first() {
    let stack = stack().with_state();
    let fake = behind_a_router(Reply::ok("web: ok"));
    let argv = ["sh".to_owned(), "-c".to_owned(), "echo 'web: ok'".to_owned()];
    let ran = with_cx_given(&stack, &fake, THROUGH_THE_NODE, async |cx| Terraform.exec(cx, "web", &argv).await)
        .await
        .expect("it ran");
    assert_eq!((ran.exit, ran.stdout.as_str()), (0, "web: ok"));
    let ssh = fake.last("ssh -F none").expect("an ssh");
    assert_eq!(ssh.flag("-i"), Some("/keys/stack"), "the run's own key: {}", ssh.line());
    // Only the node is reached from here, and pinned first; the router and web behind it are
    // pinned as the call first meets them.
    assert!(ssh.has_run(&["-o", "StrictHostKeyChecking=accept-new"]), "{}", ssh.line());
    let proxy = ssh.args.iter().find_map(|a| a.strip_prefix("ProxyCommand=")).expect("a ProxyCommand");
    assert!(proxy.ends_with(" -W %h:%p ops@192.0.2.10"), "web through its router: {proxy}");
    assert!(
        proxy.contains("-W %%h:%%p root@node.stack"),
        "the router through the node, a level down: {proxy}"
    );
    assert!(
        proxy.contains("StrictHostKeyChecking=yes") && proxy.contains("StrictHostKeyChecking=accept-new"),
        "{proxy}"
    );
    let n = ssh.args.len();
    assert_eq!(
        &ssh.args[n - 2..],
        ["ops@10.60.0.10", "sh -c 'echo '\\''web: ok'\\'''"],
        "as the module's user, the command quoted as the machine's shell reads it back"
    );
    let known = std::fs::read_to_string(stack.state.path().join(crate::ssh::KNOWN_HOSTS)).expect("the node's key pinned");
    assert_eq!(known.trim(), NODE_KEY);
}

#[tokio::test]
async fn a_command_through_a_jump_whose_known_hosts_the_run_gives_checks_it_there_and_scans_nothing() {
    let stack = stack().with_state();
    let known = tempfile::tempdir().expect("a folder");
    let file = known.path().join("nodes_known_hosts");
    std::fs::write(&file, format!("{NODE_KEY}\n")).expect("the file");
    let path = file.to_string_lossy().into_owned();
    let named = ssh_named(&file);
    let fake = FakeProcesses::script()
        .expect("terraform show -json -no-color", Reply::ok(&web_behind_a_router()))
        .expect("terraform output -json -no-color", Reply::ok(ROUTER_OUTPUTS))
        .allow("ssh -F none", Reply::ok("web: ok"))
        .build();
    let mut given = THROUGH_THE_NODE.to_vec();
    given.push(("ssh_jump_known_hosts_file", path.as_str()));
    with_cx_given(&stack, &fake, &given, async |cx| Terraform.exec(cx, "web", &["true".to_owned()]).await)
        .await
        .expect("it ran");
    let ssh = fake.last("ssh -F none").expect("an ssh");
    let proxy = ssh.args.iter().find_map(|a| a.strip_prefix("ProxyCommand=")).expect("a ProxyCommand");
    let (router, node) = proxy.split_once("ProxyCommand=").expect("the node a level down");
    assert!(
        node.contains(&named) && node.contains("StrictHostKeyChecking=yes"),
        "the node against the file: {proxy}"
    );
    assert!(!router.contains(&named), "the router against the run's own: {proxy}");
    assert!(!stack.state.path().join(crate::ssh::KNOWN_HOSTS).exists(), "nothing pinned for the node");
}

#[tokio::test]
async fn a_command_that_fails_is_its_exit_status_and_one_ssh_couldnt_run_an_error() {
    let stack = stack().with_state();
    let failed = behind_a_router(Reply {
        code: 1,
        ..Reply::ok("web: nothing listens on 80")
    });
    let ran = with_cx_given(&stack, &failed, THROUGH_THE_NODE, async |cx| {
        Terraform.exec(cx, "web", &["true".to_owned()]).await
    })
    .await
    .expect("it ran");
    assert_eq!(ran.exit, 1);
    let fresh = super::stack().with_state();
    let refused = behind_a_router(Reply {
        code: 255,
        stderr: vec!["root@node.stack: Permission denied (publickey).".to_owned()],
        ..Reply::default()
    });
    let e = with_cx_given(&fresh, &refused, THROUGH_THE_NODE, async |cx| {
        Terraform.exec(cx, "web", &["true".to_owned()]).await
    })
    .await
    .expect_err("ssh couldn't run it");
    assert_eq!(e.code(), ErrorCode::ToolFailed);
    assert!(e.to_string().contains("Permission denied (publickey)"), "{e}");
}

#[tokio::test]
async fn a_console_into_a_vm_behind_the_router_goes_the_same_way_and_a_windows_vm_has_none() {
    let stack = stack().with_state();
    let fake = behind_a_router(Reply::ok(""));
    let console = with_cx_given(&stack, &fake, THROUGH_THE_NODE, async |cx| Terraform.console(cx, "web", None).await)
        .await
        .expect("a console");
    assert_eq!(console.args.last().map(String::as_str), Some("ops@10.60.0.10"));
    assert!(
        console
            .args
            .iter()
            .any(|a| a.starts_with("ProxyCommand=") && a.ends_with("-W %h:%p ops@192.0.2.10")),
        "{:?}",
        console.args
    );
    let windows = FakeProcesses::script()
        .expect("terraform show -json -no-color", Reply::ok(&dc_beside_web()))
        .build();
    let e = with_cx(&stack, &windows, async |cx| Terraform.exec(cx, "dc", &["true".to_owned()]).await)
        .await
        .expect_err("no way into a Windows VM");
    assert_eq!(e.code(), ErrorCode::Unsupported, "{e}");
    assert!(e.to_string().contains("runs Windows"), "{e}");
}
