//! A command opened in this computer's own terminal, where a desktop session shows it: it runs
//! there, in its folder, with its variables. What the test opened it closes: the command's shell
//! by the process id it wrote, and on macOS the window by its title.

use std::path::Path;
use std::time::Duration;

use isothrum_machine::{Context, HostOs, terminal};
use isothrum_runner::ShellCommand;
use isothrum_test_support::{PidGuard, wait_until};

use crate::{answer, here, setting};

/// How long the command has to start in the window and write what it found.
const START: Duration = Duration::from_secs(60);

/// Why no desktop session shows a window here; none when one does.
async fn no_desktop(cx: &Context) -> Option<String> {
    match cx.host.os {
        HostOs::Linux if setting("DISPLAY").is_none() && setting("WAYLAND_DISPLAY").is_none() => {
            Some("no desktop session here (neither DISPLAY nor WAYLAND_DISPLAY is set)".into())
        }
        // Session 0 is the services': SSH's and WinRM's sessions run there, with no desktop.
        HostOs::Windows if windows_session(cx).await.as_deref() == Some("0") => {
            Some("this runs in Windows' session 0 (a service's, as over SSH or WinRM): no desktop to show a console on".into())
        }
        HostOs::Macos => {
            let uid = answer(cx, "id", &["-u"]).await.unwrap_or_default();
            let gui = answer(cx, "launchctl", &["print", &format!("gui/{}", uid.trim())]).await;
            gui.is_none().then(|| "no one is logged in to this Mac's desktop".into())
        }
        _ => None,
    }
}

/// The Windows session this process runs in, as PowerShell (a child, in the same session) says.
async fn windows_session(cx: &Context) -> Option<String> {
    let said = answer(
        cx,
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", "(Get-Process -Id $PID).SessionId"],
    )
    .await?;
    Some(said.trim().to_owned())
}

/// The command run in the window: a shell that writes its process id and the variable it was
/// given, then waits to be closed.
fn command(folder: &Path, out: &Path) -> ShellCommand {
    let out = out.display().to_string();
    let (program, args) = if cfg!(windows) {
        let script =
            format!("$PID | Out-File -Encoding ascii '{out}.pid'; \"$env:ISOTHRUM_E2E_MARK $PWD\" | Out-File -Encoding ascii '{out}'; Start-Sleep 600");
        ("powershell", vec!["-NoProfile".to_owned(), "-Command".to_owned(), script])
    } else {
        let script = format!("echo $$ > '{out}.pid'; echo \"$ISOTHRUM_E2E_MARK $(pwd -P)\" > '{out}'; sleep 600");
        ("sh", vec!["-c".to_owned(), script])
    };
    ShellCommand::new(program, args, folder).with_env(vec![("ISOTHRUM_E2E_MARK".into(), "marked".into())])
}

/// The command opens in a window of its own, runs in its folder with its variables, and is
/// closed again (Terminal quit too on macOS, when it wasn't running before).
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn a_command_opens_in_this_computers_terminal() {
    let cx = here().await;
    if let Some(why) = no_desktop(&cx).await {
        skip!("{why}")
    }
    let dir = tempfile::tempdir().expect("a folder");
    let folder = dir.path().join("a folder with a space");
    std::fs::create_dir(&folder).expect("the command's folder");
    let out = dir.path().join("said");
    let title = format!("isothrum-e2e-{}", std::process::id());
    let terminal_was_running = answer(&cx, "osascript", &["-e", "application \"Terminal\" is running"])
        .await
        .is_some_and(|s| s.trim() == "true");
    let opened = terminal::open(&cx, &title, &command(&folder, &out), dir.path()).await;
    if let Err(terminal::NotOpened::Automation { said }) = &opened {
        skip!("macOS doesn't let this session control Terminal (Automation permission): {said}")
    }
    opened.expect("opened");
    let pid_file = out.with_extension("pid");
    let started = wait_until(START, || out.is_file() && pid_file.is_file());
    let shell = PidGuard::from_file(&pid_file);
    let said = std::fs::read_to_string(&out).unwrap_or_default();
    drop(shell);
    if cx.host.os == HostOs::Macos {
        let close = format!("tell application \"Terminal\" to close (every window whose name contains \"{title}\")");
        answer(&cx, "osascript", &["-e", &close]).await;
        if !terminal_was_running {
            answer(&cx, "osascript", &["-e", "tell application \"Terminal\" to quit"]).await;
        }
    }
    assert!(started, "the command ran within {} s", START.as_secs());
    let said = said.trim();
    assert!(
        said.starts_with("marked ") && said.ends_with("a folder with a space"),
        "its variable and its folder: {said}"
    );
}
