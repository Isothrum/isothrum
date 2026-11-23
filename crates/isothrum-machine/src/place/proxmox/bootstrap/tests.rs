use std::path::PathBuf;
use std::sync::Arc;

use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::testing::shared;

const PUBLIC: &str = "ssh-ed25519 AAAAC3second second-key";
/// The value pveum's fake answer carries (not a real one: the shape of one).
const PVEUM_VALUE: &str = "6f1d2c3b-aaaa-bbbb-cccc-0123456789ab";
const ANSWER: &str = r#"{"full-tokenid":"root@pam!setup-0a1b2c3d","info":{"privsep":"0"},"value":"6f1d2c3b-aaaa-bbbb-cccc-0123456789ab"}"#;

fn node() -> Target {
    Target::new(
        "10.0.0.50",
        "root",
        PathBuf::from("/home/me/.ssh/id_ed25519"),
        PathBuf::from("/data/known_hosts"),
    )
}

#[test]
fn the_token_is_named_after_the_key() {
    let name = token_name("setup", PUBLIC);
    assert!(name.starts_with("setup-") && name.len() == "setup-".len() + 8, "{name}");
    assert_eq!(token_name("setup", &format!("{PUBLIC}\n")), name, "stable for the same key");
    assert_ne!(token_name("setup", "ssh-ed25519 BBBB other"), name);
}

#[test]
fn the_node_commands_are_the_two_expected() {
    assert_eq!(
        token_command("root", "setup-0a1b2c3d"),
        "pveum user token remove 'root@pam' 'setup-0a1b2c3d' >/dev/null 2>&1; pveum user token add 'root@pam' 'setup-0a1b2c3d' --privsep 0 --output-format json"
    );
    let add = authorize_command(PUBLIC);
    assert!(add.contains("grep -qxF 'ssh-ed25519 AAAAC3second second-key' ~/.ssh/authorized_keys"), "{add}");
    assert!(add.contains("echo 'ssh-ed25519 AAAAC3second second-key' >> ~/.ssh/authorized_keys"), "{add}");
    // A quote in the key can't end the quoting.
    assert!(authorize_command("ssh-ed25519 AA'; rm -rf ~ #").contains(r"'ssh-ed25519 AA'\''; rm -rf ~ #'"));
}

#[test]
fn the_token_comes_from_pveums_json_and_its_secret_never_shows() {
    let token = parse_token(ANSWER).unwrap();
    assert_eq!((token.id.as_str(), token.secret.as_str()), ("root@pam!setup-0a1b2c3d", PVEUM_VALUE));
    assert!(!format!("{token:?}").contains(PVEUM_VALUE));
    assert_eq!(parse_token(&format!("warning: something\n{ANSWER}\n")).unwrap().id, "root@pam!setup-0a1b2c3d");
    assert_eq!(parse_token(""), Err("pveum gave no token: ".to_owned()));
    assert!(parse_token(r#"{"full-tokenid":"root@pam","value":"x"}"#).is_err());
    assert!(parse_token(r#"{"full-tokenid":"root@pam!n","value":"a b"}"#).is_err());
    assert!(parse_token("{not json").unwrap_err().contains("isn't JSON"));
}

#[tokio::test]
async fn bootstrap_lets_the_key_in_then_makes_the_token() {
    let fake = Arc::new(FakeProcesses::model(|call| {
        let line = call.line();
        if line.contains("authorized_keys") {
            Reply::ok("added\n")
        } else if line.contains("pveum user token") {
            Reply::ok(ANSWER)
        } else {
            Reply::fail(1, "unexpected")
        }
    }));
    let runner: Arc<dyn isothrum_runner::ProcessRunner> = Arc::<FakeProcesses>::clone(&fake);
    let ctx = shared(runner);
    let made = bootstrap(&ctx, &node(), PUBLIC, "setup-0a1b2c3d").await.unwrap();
    assert_eq!(
        (made.token.id.as_str(), made.token.secret.as_str(), made.key_added),
        ("root@pam!setup-0a1b2c3d", PVEUM_VALUE, true)
    );
    let calls: Vec<String> = fake.calls().iter().map(isothrum_runner::ProcessSpec::line).collect();
    assert_eq!(calls.len(), 2, "only the two node commands run: {calls:?}");
    assert!(
        calls
            .iter()
            .all(|c| c.starts_with("ssh -i /home/me/.ssh/id_ed25519 -p 22 ") && c.contains(" root@10.0.0.50 ")),
        "{calls:?}"
    );
    assert!(calls.iter().all(|c| c.contains("BatchMode=yes")), "never prompts");
}

#[tokio::test]
async fn a_key_already_there_is_not_added_twice() {
    let ctx = shared(Arc::new(FakeProcesses::model(|call| {
        if call.line().contains("authorized_keys") {
            Reply::ok("present")
        } else {
            Reply::ok(ANSWER)
        }
    })));
    assert!(!bootstrap(&ctx, &node(), PUBLIC, "setup-0a1b2c3d").await.unwrap().key_added);
}

#[tokio::test]
async fn each_failure_names_its_part() {
    async fn authorize_failing(stderr: &'static str) -> BootstrapError {
        let ctx = shared(Arc::new(FakeProcesses::script().expect("ssh", Reply::fail(255, stderr)).build()));
        bootstrap(&ctx, &node(), PUBLIC, "setup-0a1b2c3d").await.unwrap_err()
    }
    let refused = authorize_failing("root@10.0.0.50: Permission denied (publickey,password).").await;
    assert_eq!(refused.part, Part::Ssh);
    assert!(refused.detail.contains("Permission denied"), "{refused}");
    assert_eq!(
        authorize_failing("bash: /root/.ssh/authorized_keys: Read-only file system").await.part,
        Part::Key
    );
    let unknown = authorize_failing("No ED25519 host key is known for 10.0.0.50.\nHost key verification failed.").await;
    assert!(unknown.part == Part::Ssh && unknown.host_key_refused, "{unknown:?}");

    let ctx = shared(Arc::new(FakeProcesses::model(|call| {
        if call.line().contains("authorized_keys") {
            Reply::ok("present")
        } else {
            Reply::fail(2, "user 'root@pam' does not exist")
        }
    })));
    let token = bootstrap(&ctx, &node(), PUBLIC, "setup-0a1b2c3d").await.unwrap_err();
    assert_eq!(token.part, Part::Token);
    assert!(token.detail.contains("does not exist"), "{token}");
}
