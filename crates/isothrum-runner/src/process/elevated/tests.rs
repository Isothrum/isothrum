//! The elevated session through a stand-in broker: a task that answers the session folder's
//! requests as the PowerShell broker does, so the protocol is tested on every host. The real
//! broker is tested by hand on a Windows host (an ignored test).

use std::path::Path;

use super::*;
use crate::process::fake::{FakeProcesses, Reply};

/// Plays the broker in `dir`: once `ready`, each request answered with `lines` and `code`.
async fn broker(dir: PathBuf, lines: &'static str, code: i32) {
    std::fs::write(dir.join("ready"), "").unwrap();
    let mut n = 1;
    while !dir.join("done").exists() {
        let req = dir.join(format!("req-{n}.json"));
        if let Ok(text) = std::fs::read_to_string(&req) {
            let r: serde_json::Value = serde_json::from_str(&text).unwrap();
            std::fs::remove_file(&req).unwrap();
            let env: Vec<String> = r["env"]
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
    let own = [
        ("Path".to_owned(), "C:\\HashiCorp\\Vagrant\\bin".to_owned()),
        ("USERPROFILE".to_owned(), "C:\\Users\\ada".to_owned()),
        ("SECRET".to_owned(), "x".to_owned()),
    ];
    let e = Elevated::new(crate::testing::processes(&fake), home.path().join("elevated"), own);
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
    let dir = e.dir.clone();
    let broker = tokio::spawn(broker(dir.clone(), "==> dc01: Booting VM...", 0));
    let mut up = ProcessSpec::new("vagrant", ["up", "--provider", "hyperv"]);
    up.env = vec![("CTF_FLAG_MAIN".into(), "flag{x}".into())];
    up.cwd = Some(PathBuf::from("C:\\labs\\ad\\.isoloom\\vagrant"));
    let (exit, lines) = lines_of(&e, &up).await;
    assert!(exit.unwrap().success());
    assert_eq!(lines[..2], ["vagrant up".to_owned(), "==> dc01: Booting VM...".to_owned()]);
    assert!(
        lines[2].contains("Path=C:\\HashiCorp\\Vagrant\\bin") && lines[2].contains("VAGRANT_HOME=C:\\Users\\ada\\.vagrant.d"),
        "{}",
        lines[2]
    );
    assert!(
        lines[2].contains("CTF_FLAG_MAIN=flag{x}") && !lines[2].contains("SECRET"),
        "only what it needs: {}",
        lines[2]
    );
    let (halted, _) = lines_of(&e, &ProcessSpec::new("vagrant", ["halt", "isoloom-controller"])).await;
    assert!(halted.unwrap().success());
    assert_eq!(fake.lines().iter().filter(|l| l.contains("Start-Process")).count(), 1, "one prompt");
    assert!(!dir.join("req-1.json").exists(), "the request (and its values) is gone");
    drop(e);
    assert!(dir.join("done").exists());
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
    let broker = tokio::spawn(broker(e.dir.clone(), "boom", 3));
    let (exit, lines) = lines_of(&e, &ProcessSpec::new("vagrant", ["destroy", "--force"])).await;
    assert_eq!(exit.unwrap().code, Some(3));
    assert_eq!(lines[1], "boom");
    let (refused, _) = lines_of(&e, &ProcessSpec::new("vagrant", ["ssh", "-c", "echo \"hi\""])).await;
    assert!(matches!(refused, Err(ProcessError::Refused { .. })));
    // A standard input the broker wouldn't pass on is refused, not dropped.
    let mut piped = ProcessSpec::new("vagrant", ["ssh", "web", "-c", "sh"]);
    piped.stdin = Some(e.dir.join("script.sh"));
    let (no_input, _) = lines_of(&e, &piped).await;
    assert!(
        matches!(&no_input, Err(ProcessError::Refused { why, .. }) if why.contains("standard input")),
        "{no_input:?}"
    );
    assert!(!e.dir.join("req-3.json").exists() && !e.dir.join("req-2.json").exists(), "no request made");
    drop(e);
    broker.await.unwrap();
}

#[tokio::test]
async fn a_request_cut_short_ends_the_broker() {
    let (_home, _fake, e) = session("", Reply::ok(""));
    let dir = e.dir.clone();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("ready"), "").unwrap();
    let spec = ProcessSpec::new("vagrant", ["up"]);
    let cut = tokio::time::timeout(Duration::from_millis(300), lines_of(&e, &spec)).await;
    assert!(cut.is_err(), "nobody answered");
    assert!(dir.join("cancel").exists());
    let (exit, _) = lines_of(&e, &spec).await;
    assert!(matches!(exit, Err(ProcessError::Elevation { .. })));
}

#[test]
fn the_groups_that_may_manage_hyperv_are_read_by_sid() {
    assert!(may_manage("\"Mandatory Label\\High Mandatory Level\",\"Label\",\"S-1-16-12288\","));
    assert!(!may_manage("\"Mandatory Label\\Medium Mandatory Level\",\"Label\",\"S-1-16-8192\","));
    assert!(!may_manage(""));
    let _ = Path::new("");
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
    let e = Elevated::new(Arc::new(NotAdmin(crate::process::SystemProcesses::default())), home.path().join("elevated"), []);
    let script = "echo hello & whoami /groups /fo csv /nh | findstr S-1-16-12288 & exit 3";
    let (exit, lines) = lines_of(&e, &ProcessSpec::new("cmd", ["/c", script])).await;
    assert_eq!(exit.unwrap().code, Some(3), "{lines:?}");
    assert!(lines.iter().any(|l| l.trim() == "hello"), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("S-1-16-12288")), "high integrity: {lines:?}");
    drop(e);
    assert!(home.path().join("elevated").join("done").exists());
}
