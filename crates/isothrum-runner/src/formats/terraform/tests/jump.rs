//! The SOCKS proxy through the run's jump host, around `apply` and `destroy`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

/// The jump host's key, as `ssh-keyscan` prints it.
const NODE_KEY: &str = "node.stack ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

/// The loopback port of a proxy `ssh` call, from its `-D 127.0.0.1:<port>`.
fn proxy_port(call: &Made) -> Option<u16> {
    let at = call.args.iter().position(|a| a == "-D")?;
    call.args.get(at + 1)?.strip_prefix("127.0.0.1:")?.parse().ok()
}

/// The tools of a run through a jump host: `ssh -D` listens on its port until it is ended, as `ssh`
/// does once the host let it in (the listeners kept in `listening`); every Terraform call answers.
fn through_a_node(listening: Arc<Mutex<Vec<std::net::TcpListener>>>) -> FakeProcesses {
    FakeProcesses::model(move |call| match call.program.as_str() {
        "ssh-keyscan" => Reply::ok(NODE_KEY),
        "ssh" => {
            let port = proxy_port(call).expect("a proxy's port");
            listening
                .lock()
                .expect("listeners")
                .push(std::net::TcpListener::bind(("127.0.0.1", port)).expect("the proxy's port"));
            Reply::ok("").after(Duration::from_secs(3600))
        }
        _ => Reply::ok(""),
    })
}

#[tokio::test]
async fn an_apply_and_a_destroy_go_through_a_proxy_to_the_runs_jump_host_and_nothing_else_does() {
    let stack = stack().with_state();
    let fake = through_a_node(Arc::default());
    let given = [("ssh_jump", "root@node.stack"), ("ssh_jump_key_file", "/keys/node")];
    with_cx_given(&stack, &fake, &given, async |cx| Terraform.up(cx).await).await.expect("up");
    with_cx_given(&stack, &fake, &given, async |cx| Terraform.down(cx).await).await.expect("down");
    let calls = fake.calls();
    let proxies: Vec<&Made> = calls.iter().filter(|c| c.program == "ssh").collect();
    assert_eq!(proxies.len(), 2, "one proxy per step");
    for proxy in &proxies {
        assert_eq!(proxy.args.last().map(String::as_str), Some("root@node.stack"));
        assert!(
            proxy.args.windows(2).any(|w| w == ["-i", "/keys/node"]),
            "the run's key for it: {:?}",
            proxy.args
        );
        assert!(proxy.args.iter().any(|a| a == "StrictHostKeyChecking=yes"), "its key pinned: {:?}", proxy.args);
        assert!(proxy.args.iter().any(|a| a == "-N"), "{:?}", proxy.args);
    }
    let applied: Vec<&Made> = calls
        .iter()
        .filter(|c| c.program == "terraform" && matches!(c.args.first().map(String::as_str), Some("apply" | "destroy")))
        .collect();
    assert_eq!(applied.len(), 2, "{calls:?}");
    for (step, proxy) in applied.iter().zip(&proxies) {
        assert_eq!(step.env("TF_VAR_ssh_proxy_host"), Some("127.0.0.1"));
        assert_eq!(step.env("TF_VAR_ssh_proxy_port"), Some(proxy_port(proxy).expect("a port").to_string().as_str()));
    }
    let init = calls
        .iter()
        .find(|c| c.program == "terraform" && c.args.first().map(String::as_str) == Some("init"))
        .expect("init");
    assert_eq!(init.env("TF_VAR_ssh_proxy_port"), None, "only the steps that reach the machines");
    assert_eq!(calls.iter().filter(|c| c.program == "ssh-keyscan").count(), 1, "the host's key pinned once");
}

#[tokio::test]
async fn a_run_with_no_jump_host_opens_no_proxy() {
    let stack = stack();
    let fake = FakeProcesses::model(|_| Reply::ok(""));
    with_cx(&stack, &fake, async |cx| Terraform.up(cx).await).await.expect("up");
    assert!(fake.calls().iter().all(|c| c.program == "terraform"), "{:?}", fake.calls());
    assert!(fake.calls().iter().all(|c| c.env("TF_VAR_ssh_proxy_host").is_none()));
}

#[tokio::test]
async fn a_proxy_the_host_refuses_fails_the_step_in_ssh_s_words_before_it_runs() {
    let stack = stack();
    let fake = FakeProcesses::model(|call| match call.program.as_str() {
        "ssh-keyscan" => Reply::ok(NODE_KEY),
        "ssh" => Reply::fail(255, "root@node.stack: Permission denied (publickey)."),
        _ => Reply::ok(""),
    });
    let e = with_cx_given(&stack, &fake, &[("ssh_jump", "root@node.stack")], async |cx| Terraform.up(cx).await)
        .await
        .expect_err("refused");
    assert_eq!(e.code(), ErrorCode::ToolFailed);
    assert!(
        e.to_string().contains("Permission denied (publickey)") && e.to_string().contains("root@node.stack"),
        "{e}"
    );
    let proxy = fake.calls().into_iter().find(|c| c.program == "ssh").expect("the proxy");
    assert!(
        proxy.args.windows(2).any(|w| w == ["-i", "/nowhere/id_ed25519"]),
        "the runner's own key: {:?}",
        proxy.args
    );
    assert!(
        !fake.calls().iter().any(|c| c.args.first().map(String::as_str) == Some("apply")),
        "no apply without its proxy"
    );
}

#[tokio::test]
async fn a_jump_host_that_isnt_a_login_is_refused_before_anything_runs_through_it() {
    let stack = stack();
    let fake = FakeProcesses::model(|_| Reply::ok(""));
    let e = with_cx_given(&stack, &fake, &[("ssh_jump", "node.stack")], async |cx| Terraform.up(cx).await)
        .await
        .expect_err("refused");
    assert!(e.to_string().contains("isn't `user@host`"), "{e}");
    assert!(
        fake.calls()
            .iter()
            .all(|c| c.program == "terraform" && c.args.first().map(String::as_str) == Some("init")),
        "{:?}",
        fake.calls()
    );
}

#[tokio::test]
async fn a_jump_host_whose_known_hosts_the_run_gives_is_checked_against_that_file_and_never_scanned() {
    let stack = stack().with_state();
    let fake = through_a_node(Arc::default());
    let known = tempfile::tempdir().expect("a folder");
    let file = known.path().join("nodes_known_hosts");
    std::fs::write(&file, format!("{NODE_KEY}\n")).expect("the file");
    let path = file.to_string_lossy().into_owned();
    let given = [("ssh_jump", "root@node.stack"), ("ssh_jump_known_hosts_file", path.as_str())];
    with_cx_given(&stack, &fake, &given, async |cx| Terraform.up(cx).await).await.expect("up");
    let calls = fake.calls();
    assert!(!calls.iter().any(|c| c.program == "ssh-keyscan"), "never scanned: {calls:?}");
    let proxy = calls.iter().find(|c| c.program == "ssh").expect("the proxy");
    assert!(proxy.args.contains(&format!("UserKnownHostsFile=\"{path}\"")), "{:?}", proxy.args);
    assert!(proxy.args.iter().any(|a| a == "StrictHostKeyChecking=yes"), "{:?}", proxy.args);
    assert!(!stack.state.path().join("known_hosts").exists(), "nothing pinned in the run's");
    assert_eq!(std::fs::read_to_string(&file).expect("the file"), format!("{NODE_KEY}\n"));
}

#[tokio::test]
async fn a_jump_hosts_known_hosts_file_not_there_is_refused_before_the_apply() {
    let stack = stack();
    let fake = through_a_node(Arc::default());
    let given = [("ssh_jump", "root@node.stack"), ("ssh_jump_known_hosts_file", "/nowhere/known_hosts")];
    let e = with_cx_given(&stack, &fake, &given, async |cx| Terraform.up(cx).await)
        .await
        .expect_err("refused");
    assert_eq!(e.code(), ErrorCode::ToolFailed);
    assert!(e.to_string().contains("/nowhere/known_hosts"), "{e}");
    assert!(
        !fake
            .calls()
            .iter()
            .any(|c| c.program == "ssh" || c.args.first().map(String::as_str) == Some("apply")),
        "{:?}",
        fake.calls()
    );
}
