//! The real broker: on Windows, a command run elevated by the session's own PowerShell; elsewhere,
//! the broker's script under `pwsh` (ignored unless asked for).

use super::*;

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
    let script = "echo hello; whoami /groups /fo csv /nh | findstr S-1-16-12288; exit 3";
    let (exit, lines) = lines_of(&e, &ProcessSpec::new("powershell", ["-NoProfile", "-Command", script])).await;
    assert_eq!(exit.unwrap().code, Some(3), "{lines:?}");
    assert!(lines.iter().any(|l| l.trim() == "hello"), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("S-1-16-12288")), "high integrity: {lines:?}");
    drop(e);
    assert!(!home.path().join("elevated").exists());
}

/// The real processes, the broker's launch run by PowerShell 7 (`pwsh`) unelevated and the
/// broker given a stand-in `taskkill.exe` (it ends a process and its children) on `path` and a
/// stand-in `vagrant` (it runs its arguments) in `bin`, so the broker's own PowerShell runs here;
/// `whoami` says the account may not manage Hyper-V.
#[cfg(unix)]
struct Pwsh {
    system: crate::process::SystemProcesses,
    path: String,
    bin: PathBuf,
}

/// The broker's launch `launcher` with its script made to run here: Vagrant is the stand-in in
/// `bin`, owned as an administrator's install would be, and run with this host's folders as its
/// `PATH`. What the broker refuses and drops is unchanged.
#[cfg(unix)]
fn here(launcher: &str, bin: &Path) -> String {
    let marker = "'-EncodedCommand','";
    let start = launcher.find(marker).unwrap() + marker.len();
    let end = start + launcher[start..].find('\'').unwrap();
    let units: Vec<u16> = unbase64(&launcher[start..end]).chunks(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
    let script = String::from_utf16(&units).unwrap();
    let vagrant = super::script::ps_quote(&bin.join("vagrant").display().to_string());
    let stand_ins = format!("$programs = @{{ 'vagrant' = @({vagrant}) }}\n$systemPath = @()\nfunction Test-Owned($path) {{ $true }}\n$vagrantVars = @(");
    let script = script.replacen("$vagrantVars = @(", &stand_ins, 1);
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    format!("{}{}{}", &launcher[..start], crate::encoding::base64(&bytes), &launcher[end..])
}

/// Standard base64 (RFC 4648, padded) read back.
#[cfg(unix)]
fn unbase64(text: &str) -> Vec<u8> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let sextets: Vec<u32> = text
        .bytes()
        .filter(|b| *b != b'=')
        .map(|b| u32::try_from(TABLE.iter().position(|t| *t == b).unwrap()).unwrap())
        .collect();
    sextets
        .chunks(4)
        .flat_map(|c| {
            let n = c.iter().fold(0, |n, s| (n << 6) | s) << (6 * (4 - c.len()));
            n.to_be_bytes()[1..c.len()].to_vec()
        })
        .collect()
}

#[cfg(unix)]
impl ProcessRunner for Pwsh {
    fn run<'a>(&'a self, spec: &'a ProcessSpec, on_line: OnLine<'a>) -> BoxFuture<'a, Result<Exit, ProcessError>> {
        if spec.program == "whoami" {
            return Box::pin(async { Ok(Exit::new(Some(0))) });
        }
        let mut spec = spec.clone();
        if spec.program == "powershell" {
            spec.program = "pwsh".into();
            let launcher = here(&spec.args[3], &self.bin);
            spec.args[3] = launcher.replacen("-FilePath powershell.exe -Verb RunAs -WindowStyle Hidden", "-FilePath pwsh", 1);
            spec.env.push(("PATH".into(), self.path.clone()));
        }
        Box::pin(async move { self.system.run(&spec, on_line).await })
    }
}

/// The PowerShell broker itself, run by `pwsh`: a request answered with its variables, output
/// and exit code; a request cut short ended alone (its process tree), the session going on with
/// the next; the session ended with its operation. By hand where `pwsh` is on PATH:
/// `cargo test -p isothrum-runner pwsh_broker -- --ignored`.
#[cfg(unix)]
#[tokio::test]
#[ignore = "runs the PowerShell broker: needs pwsh (PowerShell 7) on PATH"]
async fn a_pwsh_broker_cancels_a_request_cut_short_and_takes_the_next() {
    let home = tempfile::tempdir().unwrap();
    let bin = home.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // Written by a child process, as every program a test runs.
    isothrum_test_support::write_executable(
        &bin.join("taskkill.exe"),
        "#!/bin/sh\neval pid=\\${$#}\nend() { for c in $(pgrep -P \"$1\"); do end \"$c\"; done; kill -9 \"$1\" 2>/dev/null; }\nend \"$pid\"\n",
    )
    .unwrap();
    // Only Vagrant and PowerShell are elevated: the requests are commands the stand-in runs.
    isothrum_test_support::write_executable(&bin.join("vagrant"), "#!/bin/sh\nPATH=/usr/bin:/bin exec \"$@\"\n").unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let processes = Arc::new(Pwsh {
        system: crate::process::SystemProcesses::default(),
        path,
        bin: bin.clone(),
    });
    let e = Elevated::new(processes, home.path().join("elevated"), Vec::new());
    let mut first = ProcessSpec::new("vagrant", ["sh", "-c", "echo $DEMO; exit 3"]);
    first.env = vec![("DEMO".into(), "from-the-request".into())];
    let (exit, lines) = lines_of(&e, &first).await;
    assert_eq!(exit.unwrap().code, Some(3), "{lines:?}");
    assert_eq!(lines, ["from-the-request"]);
    let started = std::time::Instant::now();
    let cut = tokio::time::timeout(Duration::from_secs(4), lines_of(&e, &ProcessSpec::new("vagrant", ["sleep", "300"]))).await;
    assert!(cut.is_err(), "a request cut short");
    let dir = session_dir(&e.root).await;
    let (after, after_lines) = within_30s(&e, &ProcessSpec::new("vagrant", ["sh", "-c", "echo after; echo ${DEMO:-unset}"])).await;
    assert!(after.unwrap().success(), "{after_lines:?}");
    assert_eq!(after_lines, ["after", "unset"], "the next request ran, without the earlier one's variables");
    assert!(started.elapsed() < Duration::from_secs(60), "the cut request didn't run to its end");
    assert!(!dir.join("ended").exists() && !dir.join("cancel-2").exists() && !dir.join("out-2.log").exists());
    drop(e);
    assert!(!home.path().join("elevated").exists());
}

/// What a call through `e` gives within 30 s (a call that waits longer fails the test).
#[cfg(unix)]
async fn within_30s(e: &Elevated, spec: &ProcessSpec) -> (Result<Exit, ProcessError>, Vec<String>) {
    tokio::time::timeout(Duration::from_secs(30), lines_of(e, spec))
        .await
        .expect("the call waited for an answer that won't come")
}
