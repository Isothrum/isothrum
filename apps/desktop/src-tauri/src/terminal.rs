//! Opening the system's terminal on a console, as GNS3's consoles do.

use std::process::{Command, Stdio};

use crate::console::Console;
use crate::runner::RunError;

/// Opens the system's terminal running `console`, titled `title`.
pub(crate) fn open_terminal(title: &str, console: &Console) -> Result<(), RunError> {
    let mut c = terminal(title, console)?;
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c.spawn()
        .map(|_| ())
        .map_err(|e| RunError::Io(format!("couldn't open a terminal: {e}")))
}

/// Opens `url` (`vnc://...`, `spice://...`) with the system's handler for it: Screen Sharing for
/// VNC on macOS, the desktop's default viewer elsewhere.
pub(crate) fn open_url(url: &str) -> Result<(), RunError> {
    let mut c = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        Command::new("xdg-open")
    };
    c.arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| RunError::Io(format!("couldn't open {url}: {e}")))
}

/// macOS: Terminal.app, through `AppleScript`.
#[cfg(target_os = "macos")]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the Linux variant can fail; all three share one signature so open_terminal calls \
              them the same way"
)]
fn terminal(_title: &str, console: &Console) -> Result<Command, RunError> {
    let line = console.sh_line().replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!("tell application \"Terminal\"\nactivate\ndo script \"{line}\"\nend tell");
    let mut c = Command::new("osascript");
    c.args(["-e", &script]);
    Ok(c)
}

/// Windows: a new `cmd` window. The line is passed raw: Rust's quoting is for programs, not cmd.
#[cfg(target_os = "windows")]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the Linux variant can fail; all three share one signature so open_terminal calls \
              them the same way"
)]
fn terminal(title: &str, console: &Console) -> Result<Command, RunError> {
    use std::os::windows::process::CommandExt;
    let title: String = title.chars().filter(|c| !"\"&|<>^%".contains(*c)).collect();
    let mut c = Command::new("cmd");
    c.raw_arg(format!(
        "/C start \"{title}\" cmd /K {}",
        console.cmd_line()
    ));
    Ok(c)
}

/// Linux and the other Unixes: `$TERMINAL` when the user set one (it takes `-e`), else the usual
/// desktops' terminals, the generic alternative last but one (it may point anywhere).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn terminal(title: &str, console: &Console) -> Result<Command, RunError> {
    use crate::tools::has;
    let terminals: [(&str, &[&str]); 6] = [
        ("gnome-terminal", &["--title", title, "--"]),
        ("konsole", &["-p", "tabtitle", "-e"]),
        ("xfce4-terminal", &["-T", title, "-x"]),
        ("alacritty", &["-t", title, "-e"]),
        ("x-terminal-emulator", &["-T", title, "-e"]),
        ("xterm", &["-T", title, "-e"]),
    ];
    let preferred = std::env::var("TERMINAL").ok().filter(|t| has(t));
    let mut c = match preferred {
        Some(t) => {
            let mut c = Command::new(t);
            c.arg("-e");
            c
        }
        None => {
            let (program, args) = terminals.iter().find(|(p, _)| has(p)).ok_or_else(|| {
                RunError::Io(
                    "no terminal found (install xterm, GNOME Terminal or Konsole, or set $TERMINAL)"
                        .into(),
                )
            })?;
            let mut c = Command::new(program);
            c.args(*args);
            c
        }
    };
    c.args(["sh", "-c", &console.sh_line()]);
    Ok(c)
}
