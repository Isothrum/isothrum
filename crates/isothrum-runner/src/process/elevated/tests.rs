//! The elevated session through a stand-in broker: a task that answers the session folder's
//! requests as the PowerShell broker does, so the protocol is tested on every host. The real
//! broker is tested by hand on a Windows host (an ignored test).

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
/// `code`, its files read and deleted first; any other request deleted unrun.
async fn broker(root: PathBuf, lines: &'static str, code: i32) {
    let dir = session_dir(&root).await;
    let token = dir.file_name().unwrap().to_string_lossy().into_owned();
    std::fs::write(dir.join("ready"), "").unwrap();
    let mut n = 1;
    while dir.exists() && !dir.join("done").exists() {
        let req = dir.join(format!("req-{n}.json"));
        if let Ok(text) = std::fs::read_to_string(&req) {
            std::fs::remove_file(&req).unwrap();
            let r: serde_json::Value = serde_json::from_str(&text).unwrap();
            if r["session"] != token.as_str() || r["seq"] != n {
                continue;
            }
            let env_file = dir.join(format!("env-{n}.json"));
            let e: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&env_file).unwrap()).unwrap();
            std::fs::remove_file(&env_file).unwrap();
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
            std::fs::write(dir.join(format!("out-{n}.log")), out).unwrap();
            std::fs::write(dir.join(format!("exit-{n}")), code.to_string()).unwrap();
            n += 1;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn session(groups: &'static str, start: Reply) -> (tempfile::TempDir, Arc<FakeProcesses>, Elevated) {
    let home = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeProcesses::rules(&[
        ("whoami", Reply::ok(groups)),
        ("powershell", start),
        ("vagrant status", Reply::ok("direct")),
    ]));
    let own = crate::host_env::HostEnv::of([
        ("USERPROFILE", "C:\\Users\\ada"),
        ("API_TOKEN", "x"),
        ("HTTPS_PROXY", "http://proxy.corp:3128"),
        ("no_proxy", ".corp"),
        ("SSL_CERT_FILE", "C:\\certs\\corp.pem"),
    ]);
    let forwarded = own.for_elevation(Some(std::ffi::OsStr::new("C:\\HashiCorp\\Vagrant\\bin")));
    let e = Elevated::new(crate::testing::processes(&fake), home.path().join("elevated"), forwarded);
    (home, fake, e)
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
    assert!(!fake.lines().iter().any(|l| l.starts_with("powershell")), "no prompt: {:?}", fake.lines());
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
    assert!(
        env.contains("PATH=C:\\HashiCorp\\Vagrant\\bin") && env.contains("VAGRANT_HOME=C:\\Users\\ada\\.vagrant.d"),
        "{env}"
    );
    assert!(env.contains("DEMO_SECRET=token-x") && !env.contains("API_TOKEN"), "only what it needs: {env}");
    assert!(
        env.contains("HTTPS_PROXY=http://proxy.corp:3128") && env.contains("no_proxy=.corp") && env.contains("SSL_CERT_FILE=C:\\certs\\corp.pem"),
        "the network the way this account reaches it: {env}"
    );
    let (halted, _) = lines_of(&e, &ProcessSpec::new("vagrant", ["halt", "isoloom-controller"])).await;
    assert!(halted.unwrap().success());
    assert_eq!(fake.lines().iter().filter(|l| l.contains("Start-Process")).count(), 1, "one prompt");
    let dir = session_dir(&root).await;
    let left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|f| f.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["ready"], "every request's files are gone once answered");
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
        std::fs::write(stale.join("ready"), "").unwrap();
        std::fs::write(stale.join("out-1.log"), "an earlier command's output").unwrap();
        std::fs::write(stale.join("exit-1"), "0").unwrap();
        std::fs::write(stale.join("env-2.json"), r#"{"env":[{"k":"DEMO_SECRET","v":"token-old"}]}"#).unwrap();
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
        dir.join("req-2.json"),
        r#"{"session":"guess","seq":2,"program":"cmd","args":["/c","whoami"],"cwd":"."}"#,
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!dir.join("req-2.json").exists() && !dir.join("exit-2").exists(), "deleted unrun");
    let (second, lines) = lines_of(&e, &ProcessSpec::new("vagrant", ["halt"])).await;
    assert!(second.unwrap().success());
    assert_eq!(lines[0], "vagrant halt");
    drop(e);
    broker.await.unwrap();
}

#[tokio::test]
async fn a_declined_prompt_fails_every_call_plainly() {
    let declined = Reply::fail(
        1,
        "Start-Process : This command cannot be run due to the error: The operation was canceled by the user.",
    );
    let (_home, _fake, e) = session("", declined);
    let (exit, _) = lines_of(&e, &ProcessSpec::new("vagrant", ["up"])).await;
    let err = exit.unwrap_err();
    assert!(matches!(err, ProcessError::Elevation { .. }));
    assert_eq!(err.to_string(), "elevation: the Windows permission prompt was declined");
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
    assert!(!dir.join("req-3.json").exists() && !dir.join("req-2.json").exists(), "no request made");
    drop(e);
    broker.await.unwrap();
}

#[tokio::test]
async fn a_request_cut_short_ends_the_broker() {
    let (_home, _fake, e) = session("", Reply::ok(""));
    let root = e.root.clone();
    // A broker that starts, then never answers.
    let started = tokio::spawn(async move { std::fs::write(session_dir(&root).await.join("ready"), "").unwrap() });
    let spec = ProcessSpec::new("vagrant", ["up"]);
    let cut = tokio::time::timeout(Duration::from_millis(600), lines_of(&e, &spec)).await;
    assert!(cut.is_err(), "nobody answered");
    started.await.unwrap();
    assert!(session_dir(&e.root).await.join("cancel").exists());
    let (exit, _) = lines_of(&e, &spec).await;
    assert!(matches!(exit, Err(ProcessError::Elevation { .. })));
    let folder = e.root.clone();
    drop(e);
    assert!(!folder.exists(), "the cut session's files (its values) are gone");
}

/// A broker that starts, then sets `obstacle` (given the session's folder) instead of answering.
async fn started_then(root: PathBuf, obstacle: fn(&Path)) {
    let dir = session_dir(&root).await;
    obstacle(&dir);
    std::fs::write(dir.join("ready"), "").unwrap();
}

/// What a call through `e` gives within 5 s (a call that waits longer fails the test).
async fn within_5s(e: &Elevated, spec: &ProcessSpec) -> Result<Exit, ProcessError> {
    let call = tokio::time::timeout(Duration::from_secs(5), lines_of(e, spec)).await;
    call.expect("the call waited for an answer that won't come").0
}

#[tokio::test]
async fn a_broker_that_ends_mid_request_fails_it_at_once_and_the_session_with_it() {
    // Regression: a request waited for its `exit-<n>` with no sign of a broker gone, until its
    // step's limit (a `timeout`, after up to hours).
    let (_home, _fake, e) = session("", Reply::ok(""));
    let root = e.root.clone();
    let broker = tokio::spawn(async move {
        let dir = session_dir(&root).await;
        std::fs::write(dir.join("ready"), "").unwrap();
        while !dir.join("req-1.json").exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        std::fs::write(dir.join("ended"), "the elevated broker stopped\r\n").unwrap();
    });
    let spec = ProcessSpec::new("vagrant", ["up"]);
    let err = within_5s(&e, &spec).await.unwrap_err();
    assert_eq!(err.to_string(), "elevation: the elevated session ended: the elevated broker stopped");
    broker.await.unwrap();
    let later = within_5s(&e, &spec).await.unwrap_err();
    assert!(
        matches!(&later, ProcessError::Elevation { reason } if reason.contains("broker stopped")),
        "{later}"
    );
}

#[tokio::test]
async fn a_request_that_cant_be_written_ends_the_session() {
    // Regression: request 1 failed to be written, and the broker went on waiting for it: every
    // later request (2, 3, …) waited for an answer that never came.
    let (_home, _fake, e) = session("", Reply::ok(""));
    let broker = tokio::spawn(started_then(e.root.clone(), |dir| {
        std::fs::create_dir_all(dir.join("req-1.json").join("in-the-way")).unwrap();
    }));
    let spec = ProcessSpec::new("vagrant", ["up"]);
    let first = within_5s(&e, &spec).await.unwrap_err();
    assert!(matches!(first, ProcessError::Io { .. }), "{first}");
    broker.await.unwrap();
    let second = within_5s(&e, &spec).await.unwrap_err();
    assert!(matches!(second, ProcessError::Elevation { .. }), "{second}");
    assert!(session_dir(&e.root).await.join("cancel").exists(), "the broker is told");
}

#[tokio::test]
async fn an_answer_that_cant_be_read_fails_the_request() {
    // Regression: any error reading `exit-<n>` read as "not there yet", so a request whose
    // answer can't be read waited on.
    let (_home, _fake, e) = session("", Reply::ok(""));
    let broker = tokio::spawn(started_then(e.root.clone(), |dir| {
        std::fs::create_dir_all(dir.join("exit-1").join("in-the-way")).unwrap();
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
        std::fs::write(dir.join("exit-1"), "oops\r\n").unwrap();
    }));
    let err = within_5s(&e, &ProcessSpec::new("vagrant", ["up"])).await.unwrap_err();
    assert_eq!(err.to_string(), "`vagrant`: the elevated broker answered `oops`, not an exit code");
    broker.await.unwrap();
}

#[tokio::test]
async fn a_whoami_that_cant_say_is_traced_and_the_broker_starts() {
    let home = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeProcesses::rules(&[("whoami", Reply::missing()), ("powershell", Reply::ok(""))]));
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

#[test]
fn the_groups_that_may_manage_hyperv_are_read_by_sid() {
    assert!(may_manage("\"Mandatory Label\\High Mandatory Level\",\"Label\",\"S-1-16-12288\","));
    assert!(!may_manage("\"Mandatory Label\\Medium Mandatory Level\",\"Label\",\"S-1-16-8192\","));
    assert!(!may_manage(""));
}

/// The real processes, except that `whoami` says the account may not manage Hyper-V: the broker
/// starts.
#[cfg(windows)]
struct NotAdmin(crate::process::SystemProcesses);

#[cfg(windows)]
impl ProcessRunner for NotAdmin {
    fn run<'a>(&'a self, spec: &'a ProcessSpec, on_line: OnLine<'a>) -> BoxFuture<'a, Result<Exit, ProcessError>> {
        if spec.program == "whoami" {
            return Box::pin(async move {
                on_line(Stream::Stdout, "\"Everyone\",\"Well-known group\",\"S-1-1-0\",\"Mandatory group\"");
                Ok(Exit { code: Some(0) })
            });
        }
        self.0.run(spec, on_line)
    }
}

/// The PowerShell broker itself, on a real Windows: one elevation (a UAC prompt, unless UAC is
/// off), a command run through it with its output and exit code read back, and the session ended.
/// By hand on a Windows host: `cargo test -p isothrum-runner real_broker -- --ignored`.
#[cfg(windows)]
#[tokio::test]
#[ignore = "starts an elevated PowerShell: a UAC prompt where UAC is on"]
async fn a_real_broker_runs_a_command_elevated_and_reads_back_its_exit() {
    let home = tempfile::tempdir().unwrap();
    let e = Elevated::new(
        Arc::new(NotAdmin(crate::process::SystemProcesses::default())),
        home.path().join("elevated"),
        Vec::new(),
    );
    let script = "echo hello & whoami /groups /fo csv /nh | findstr S-1-16-12288 & exit 3";
    let (exit, lines) = lines_of(&e, &ProcessSpec::new("cmd", ["/c", script])).await;
    assert_eq!(exit.unwrap().code, Some(3), "{lines:?}");
    assert!(lines.iter().any(|l| l.trim() == "hello"), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("S-1-16-12288")), "high integrity: {lines:?}");
    drop(e);
    assert!(!home.path().join("elevated").exists());
}
