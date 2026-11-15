//! The elevated session through a stand-in broker: a task that answers the session folder's
//! requests as the PowerShell broker does, so the protocol is tested on every host. The real
//! broker is tested by hand on a Windows host (an ignored test).

mod cut_short;
use cut_short::{started_then, within_5s};
use std::path::Path;

use super::*;
#[cfg(windows)]
use crate::events::Stream;
use crate::process::fake::{FakeProcesses, Reply};

/// The session folder the runner opened under `root` (its name is the session's token).
async fn session_dir(root: &Path) -> PathBuf {
    loop {
        let opened = std::fs::read_dir(root)
            .ok()
            .and_then(|mut e| e.find_map(|d| d.ok().map(|d| d.path()).filter(|p| p.is_dir())));
        if let Some(dir) = opened {
            return dir;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Plays the broker as the PowerShell one does, in the session the runner opens under `root`:
/// once `ready`, each request of the session (its token, its turn) answered with `lines` and
/// `code`, its files read and deleted first; any other request deleted unrun. A request whose
/// turn comes with its `cancel-<n>` there is skipped unrun; one whose first argument is `hang`
/// runs until its `cancel-<n>` comes, then gets no answer; one whose first argument is `slow` is
/// answered after 600 ms.
async fn broker(root: PathBuf, lines: &'static str, code: i32) {
    broker_after(root, |_| {}, lines, code).await;
}

/// [`broker`], once `obstacle` is set in the session's folder.
async fn broker_after(root: PathBuf, obstacle: fn(&Path), lines: &'static str, code: i32) {
    let dir = session_dir(&root).await;
    obstacle(&dir);
    let token = dir.file_name().unwrap().to_string_lossy().into_owned();
    std::fs::write(dir.join(protocol::READY), "").unwrap();
    let mut n = 1;
    let over = |d: &Path| !d.exists() || d.join(protocol::DONE).exists();
    while !over(&dir) {
        let req = dir.join(protocol::request(n));
        let cancel = dir.join(protocol::cancel(n));
        if cancel.exists() {
            for f in [&req, &dir.join(protocol::variables(n)), &cancel] {
                crate::store::remove_path(f).unwrap();
            }
            n += 1;
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&req) {
            std::fs::remove_file(&req).unwrap();
            let r: serde_json::Value = serde_json::from_str(&text).unwrap();
            if r["session"] != token.as_str() || r["seq"] != n {
                continue;
            }
            let env_file = dir.join(protocol::variables(n));
            let e: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&env_file).unwrap()).unwrap();
            std::fs::remove_file(&env_file).unwrap();
            match r["args"][0].as_str().unwrap() {
                "hang" => {
                    while !cancel.exists() && !over(&dir) {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    crate::store::remove_path(&cancel).unwrap();
                    n += 1;
                    continue;
                }
                "slow" => tokio::time::sleep(Duration::from_millis(600)).await,
                _ => {}
            }
            let env: Vec<String> = e["env"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| format!("{}={}", p["k"].as_str().unwrap(), p["v"].as_str().unwrap()))
                .collect();
            let out = format!(
                "{} {}\r\n{lines}\nenv {}",
                r["program"].as_str().unwrap(),
                r["args"][0].as_str().unwrap(),
                env.join(";")
            );
            std::fs::write(dir.join(protocol::output(n)), out).unwrap();
            std::fs::write(dir.join(protocol::exit(n)), code.to_string()).unwrap();
            n += 1;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn session(groups: &'static str, start: Reply) -> (tempfile::TempDir, Arc<FakeProcesses>, Elevated) {
    let home = tempfile::tempdir().unwrap();
    let script = FakeProcesses::script()
        .allow("whoami /groups", Reply::ok(groups))
        .allow("powershell", start)
        .allow("ssh", Reply::ok("as the account"));
    let fake = Arc::new(script.allow("vagrant status", Reply::ok("direct")).build());
    let own = crate::host_env::HostEnv::of([
        ("USERPROFILE", "C:\\Users\\ada"),
        ("API_TOKEN", "x"),
        ("HTTPS_PROXY", "http://proxy.corp:3128"),
        ("no_proxy", ".corp"),
        ("SSL_CERT_FILE", "C:\\certs\\corp.pem"),
    ]);
    let forwarded = own.for_elevation();
    let e = Elevated::new(crate::testing::processes(&fake), home.path().join("elevated"), forwarded);
    (home, fake, e)
}

/// The names of the files left in a session's folder `dir`.
fn files_left(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|f| f.file_name().to_string_lossy().into_owned())
        .collect()
}

async fn lines_of(e: &Elevated, spec: &ProcessSpec) -> (Result<Exit, ProcessError>, Vec<String>) {
    let mut lines = Vec::new();
    let exit = e.run(spec, &mut |_, l| lines.push(l.to_owned())).await;
    (exit, lines)
}

#[tokio::test]
async fn an_account_that_may_manage_hyperv_runs_the_tools_as_they_are() {
    let (_home, fake, e) = session(
        "\"BUILTIN\\Hyper-V Administrators\",\"Alias\",\"S-1-5-32-578\",\"Enabled group\"",
        Reply::ok(""),
    );
    let (exit, lines) = lines_of(&e, &ProcessSpec::new("vagrant", ["status"])).await;
    assert!(exit.unwrap().success());
    assert_eq!(lines, ["direct"]);
    assert!(fake.made("powershell").is_empty(), "no prompt: {:?}", fake.lines());
}

#[tokio::test]
async fn one_prompt_serves_every_command_of_an_operation_through_the_broker() {
    let (_home, fake, e) = session("\"Everyone\",\"Well-known group\",\"S-1-1-0\",\"Enabled group\"", Reply::ok(""));
    let root = e.root.clone();
    let broker = tokio::spawn(broker(root.clone(), "==> dc01: Booting VM...", 0));
    let mut up = ProcessSpec::new("vagrant", ["up", "--provider", "hyperv"]);
    up.env = vec![("DEMO_SECRET".into(), "token-x".into())];
    up.cwd = Some(PathBuf::from("C:\\work\\ad\\.isoloom\\vagrant"));
    let (exit, lines) = lines_of(&e, &up).await;
    assert!(exit.unwrap().success());
    assert_eq!(lines[..2], ["vagrant up".to_owned(), "==> dc01: Booting VM...".to_owned()]);
    let env = &lines[2];
    assert!(env.contains("VAGRANT_HOME=C:\\Users\\ada\\.vagrant.d") && !env.contains("PATH="), "{env}");
    assert!(env.contains("DEMO_SECRET=token-x") && !env.contains("API_TOKEN"), "only what it needs: {env}");
    assert!(
        env.contains("HTTPS_PROXY=http://proxy.corp:3128") && env.contains("no_proxy=.corp") && env.contains("SSL_CERT_FILE=C:\\certs\\corp.pem"),
        "the network the way this account reaches it: {env}"
    );
    let (halted, _) = lines_of(&e, &ProcessSpec::new("vagrant", ["halt", "isoloom-controller"])).await;
    assert!(halted.unwrap().success());
    assert_eq!(fake.lines().iter().filter(|l| l.contains("Start-Process")).count(), 1, "one prompt");
    let dir = session_dir(&root).await;
    let left = files_left(&dir);
    assert_eq!(left, [protocol::READY], "every request's files are gone once answered");
    drop(e);
    assert!(!root.exists(), "the session's folder goes with it");
    broker.await.unwrap();
}

#[test]
fn a_request_file_holds_no_value_and_names_its_session_and_turn() {
    let mut spec = ProcessSpec::new("vagrant", ["up"]);
    spec.env = vec![("DEMO_SECRET".into(), "token-x".into())];
    let forwarded = [("HTTPS_PROXY".to_owned(), "http://user:pw@proxy:3128".to_owned())];
    let req: serde_json::Value = serde_json::from_str(&request_file("t0k3n", 7, &spec)).unwrap();
    assert_eq!((req["session"].as_str(), req["seq"].as_u64(), req.get("env")), (Some("t0k3n"), Some(7), None));
    assert!(!req.to_string().contains("token-x") && !req.to_string().contains("pw@"));
    let env: serde_json::Value = serde_json::from_str(&env_file("t0k3n", 7, &spec, &forwarded)).unwrap();
    assert_eq!((env["session"].as_str(), env["seq"].as_u64()), (Some("t0k3n"), Some(7)));
    assert_eq!(env["env"][1], serde_json::json!({ "k": "DEMO_SECRET", "v": "token-x" }));
}

#[tokio::test]
async fn what_an_earlier_session_left_is_cleared_never_read_as_this_ones() {
    // Regression: a session read the `ready`, `exit-1` and `out-1.log` an earlier operation left
    // in the folder as its own: it took the broker for started and the first command for done.
    let (_home, _fake, e) = session("", Reply::ok(""));
    let root = e.root.clone();
    for stale in [root.clone(), root.join("an-earlier-session")] {
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join(protocol::READY), "").unwrap();
        std::fs::write(stale.join(protocol::output(1)), "an earlier command's output").unwrap();
        std::fs::write(stale.join(protocol::exit(1)), "0").unwrap();
        std::fs::write(stale.join(protocol::variables(2)), r#"{"env":[{"k":"DEMO_SECRET","v":"token-old"}]}"#).unwrap();
    }
    let broker = tokio::spawn(broker(root.clone(), "this session's", 4));
    let (exit, lines) = lines_of(&e, &ProcessSpec::new("vagrant", ["status"])).await;
    assert_eq!(exit.unwrap().code, Some(4), "{lines:?}");
    assert_eq!(lines[1], "this session's");
    let names: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().map(|f| f.file_name()).collect();
    assert_eq!(names.len(), 1, "only this session's folder: {names:?}");
    drop(e);
    broker.await.unwrap();
}

#[tokio::test]
async fn the_broker_runs_no_request_of_another_session() {
    let (_home, _fake, e) = session("", Reply::ok(""));
    let root = e.root.clone();
    let broker = tokio::spawn(broker(root.clone(), "ran", 0));
    let (first, _) = lines_of(&e, &ProcessSpec::new("vagrant", ["status"])).await;
    assert!(first.unwrap().success());
    let dir = session_dir(&root).await;
    // A request planted in turn 2 without the session's token is deleted unrun; the runner's own
    // request 2 then runs.
    std::fs::write(
        dir.join(protocol::request(2)),
        r#"{"session":"guess","seq":2,"program":"cmd","args":["/c","whoami"],"cwd":"."}"#,
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !dir.join(protocol::request(2)).exists() && !dir.join(protocol::exit(2)).exists(),
        "deleted unrun"
    );
    let (second, lines) = lines_of(&e, &ProcessSpec::new("vagrant", ["halt"])).await;
    assert!(second.unwrap().success());
    assert_eq!(lines[0], "vagrant halt");
    drop(e);
    broker.await.unwrap();
}

#[tokio::test]
async fn a_declined_prompt_fails_every_call_plainly() {
    // Regression: the prompt declined was known by Windows' English wording; a French Windows
    // says `L'opération a été annulée par l'utilisateur`.
    let declined = Reply::fail(
        super::script::ERROR_CANCELLED,
        "Start-Process : Impossible d'exécuter cette commande en raison de l'erreur : L'opération a été annulée par l'utilisateur.",
    );
    let (_home, _fake, e) = session("", declined);
    let (exit, _) = lines_of(&e, &ProcessSpec::new("vagrant", ["up"])).await;
    let err = exit.unwrap_err();
    assert!(matches!(err, ProcessError::Elevation { .. }));
    assert_eq!(err.to_string(), "elevation: the Windows permission prompt was declined");
}

/// Regression: a launch that failed saying nothing on its error output failed with no reason at
/// all; it says why as every failed tool does ([`crate::Captured::reason`]).
#[tokio::test]
async fn a_launch_that_fails_says_why_as_every_failed_tool_does() {
    let quiet = Reply {
        stdout: vec!["Start-Process: the broker script is blocked".into()],
        code: 5,
        ..Reply::default()
    };
    let (_home, _fake, e) = session("", quiet);
    let (exit, _) = lines_of(&e, &ProcessSpec::new("vagrant", ["up"])).await;
    assert_eq!(
        exit.unwrap_err().to_string(),
        "elevation: the elevated session didn't start: Start-Process: the broker script is blocked (exited with code 5)"
    );
}

#[tokio::test]
async fn a_failed_command_keeps_its_exit_code_and_what_the_broker_cant_carry_is_refused() {
    let (_home, _fake, e) = session("", Reply::ok(""));
    let broker = tokio::spawn(broker(e.root.clone(), "boom", 3));
    let (exit, lines) = lines_of(&e, &ProcessSpec::new("vagrant", ["destroy", "--force"])).await;
    assert_eq!(exit.unwrap().code, Some(3));
    assert_eq!(lines[1], "boom");
    let (refused, _) = lines_of(&e, &ProcessSpec::new("vagrant", ["ssh", "-c", "echo \"hi\""])).await;
    assert!(matches!(refused, Err(ProcessError::Refused { .. })));
    // A standard input the broker wouldn't pass on is refused, not dropped.
    let mut piped = ProcessSpec::new("vagrant", ["ssh", "web", "-c", "sh"]);
    piped.stdin = Some(e.root.join("script.sh"));
    let (no_input, _) = lines_of(&e, &piped).await;
    assert!(
        matches!(&no_input, Err(ProcessError::Refused { why, .. }) if why.contains("standard input")),
        "{no_input:?}"
    );
    let dir = session_dir(&e.root).await;
    assert!(
        !dir.join(protocol::request(3)).exists() && !dir.join(protocol::request(2)).exists(),
        "no request made"
    );
    drop(e);
    broker.await.unwrap();
}

#[tokio::test]
async fn an_answer_that_cant_be_read_fails_the_request() {
    // Regression: any error reading `exit-<n>` read as "not there yet", so a request whose
    // answer can't be read waited on.
    let (_home, _fake, e) = session("", Reply::ok(""));
    let broker = tokio::spawn(started_then(e.root.clone(), |dir| {
        std::fs::create_dir_all(dir.join(protocol::exit(1)).join("in-the-way")).unwrap();
    }));
    let err = within_5s(&e, &ProcessSpec::new("vagrant", ["up"])).await.unwrap_err();
    assert!(matches!(err, ProcessError::Io { .. }), "{err}");
    broker.await.unwrap();
}

/// Regression: an `exit-<n>` that didn't hold a number read as a program killed by a signal.
#[tokio::test]
async fn an_answer_that_isnt_an_exit_code_fails_the_request() {
    let (_home, _fake, e) = session("", Reply::ok(""));
    let broker = tokio::spawn(started_then(e.root.clone(), |dir| {
        std::fs::write(dir.join(protocol::exit(1)), "oops\r\n").unwrap();
    }));
    let err = within_5s(&e, &ProcessSpec::new("vagrant", ["up"])).await.unwrap_err();
    assert_eq!(err.to_string(), "`vagrant`: the elevated broker answered `oops`, not an exit code");
    broker.await.unwrap();
}

#[tokio::test]
async fn a_whoami_that_cant_say_is_traced_and_the_broker_starts() {
    let home = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeProcesses::expect(&[("whoami /groups", Reply::missing()), ("powershell", Reply::ok(""))]));
    let e = Elevated::new(crate::testing::processes(&fake), home.path().join("elevated"), Vec::new());
    let broker = tokio::spawn(broker(e.root.clone(), "ran", 0));
    let ((exit, _), traced) = crate::testing::traced(lines_of(&e, &ProcessSpec::new("vagrant", ["status"]))).await;
    assert!(exit.unwrap().success());
    assert!(traced.contains("WARN") && traced.contains("`whoami /groups` didn't run"), "{traced}");
    drop(e);
    broker.await.unwrap();
}

#[test]
fn a_session_token_is_random_hex() {
    let (a, b) = (token().unwrap(), token().unwrap());
    assert_eq!(a.len(), 32);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) && a != b, "{a} {b}");
}

#[tokio::test]
async fn whether_the_account_may_manage_hyperv_is_whoamis_answer_or_unknown() {
    let whoami = |groups: &str| FakeProcesses::expect(&[("whoami /groups /fo csv /nh", Reply::ok(groups))]);
    let elevated = whoami("\"Mandatory Label\\High Mandatory Level\",\"Label\",\"S-1-16-12288\",");
    assert_eq!(may_manage_hyperv(&elevated).await, HyperVRights::Granted);
    let member = whoami("\"BUILTIN\\Hyper-V Administrators\",\"Alias\",\"S-1-5-32-578\",");
    assert_eq!(may_manage_hyperv(&member).await, HyperVRights::Granted);
    let neither = whoami("\"Mandatory Label\\Medium Mandatory Level\",\"Label\",\"S-1-16-8192\",");
    assert_eq!(may_manage_hyperv(&neither).await, HyperVRights::Missing);
    let failing = FakeProcesses::expect(&[("whoami /groups", Reply::fail(1, "ERROR: access denied"))]);
    let (answer, traced) = crate::testing::traced(may_manage_hyperv(&failing)).await;
    assert_eq!(answer, HyperVRights::Unknown, "a failed whoami is no answer, not a no");
    assert!(traced.contains("`whoami /groups` failed") && traced.contains("access denied"), "{traced}");
}

#[test]
fn the_groups_that_may_manage_hyperv_are_read_by_sid() {
    assert!(may_manage("\"Mandatory Label\\High Mandatory Level\",\"Label\",\"S-1-16-12288\","));
    assert!(!may_manage("\"Mandatory Label\\Medium Mandatory Level\",\"Label\",\"S-1-16-8192\","));
    assert!(!may_manage(""));
}

mod real;

/// Only Hyper-V's programs go through the broker: another program of the operation runs as the
/// account, and what a request is handed never sets the elevated program's search path or loader.
#[tokio::test]
async fn only_hyperv_s_programs_are_elevated_and_never_with_a_loader_variable() {
    let (_home, fake, e) = session("", Reply::ok(""));
    let root = e.root.clone();
    let broker = tokio::spawn(broker(root.clone(), "ran", 0));
    let (direct, lines) = lines_of(&e, &ProcessSpec::new("ssh", ["dc01"])).await;
    assert!(direct.unwrap().success());
    assert_eq!(lines, ["as the account"]);
    assert_eq!(fake.made("ssh").len(), 1, "ran as the account: {:?}", fake.lines());
    let mut up = ProcessSpec::new("vagrant", ["up"]);
    up.env = vec![
        ("RUBYOPT".into(), "-rC:\\Users\\ada\\x.rb".into()),
        ("PATH".into(), "C:\\Users\\ada\\bin".into()),
        ("VAGRANT_INSTALLER_EMBEDDED_DIR".into(), "C:\\Users\\ada\\e".into()),
        ("DEMO_SECRET".into(), "token-x".into()),
    ];
    let (exit, up_lines) = lines_of(&e, &up).await;
    assert!(exit.unwrap().success());
    let env = &up_lines[2];
    assert!(env.contains("DEMO_SECRET=token-x"), "{env}");
    for dropped in ["RUBYOPT", "PATH=", "VAGRANT_INSTALLER_EMBEDDED_DIR"] {
        assert!(!env.contains(dropped), "{dropped}: {env}");
    }
    drop(e);
    broker.await.unwrap();
}
