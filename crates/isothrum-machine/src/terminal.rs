//! Opening a command in this computer's own terminal, in a window of its own titled as the caller
//! says: Terminal on macOS (`osascript`), the first terminal emulator that starts on Linux, a
//! console window on Windows (`cmd /c start`). The command is a [`ShellCommand`] (a console the
//! runner built, or the caller's own), run in its folder with its variables, quoted so its words
//! reach it as they are: a folder with a space, a `'`, or a Windows profile folder with `&`.
//!
//! Every program starts through the runner ([`Context::runner`]). A terminal keeps running after
//! this returns: on Linux it starts in a session of its own (`setsid`), so the runner ending the
//! starter once the terminal is up leaves the window open; on macOS and Windows the starter
//! (`osascript`, `cmd`) exits once the window is open, and what a tool started after exiting on
//! its own is left alone.

use std::path::Path;
use std::time::{Duration, SystemTime};

use isothrum_runner::{HostOs, ProcessSpec, ShellCommand, capture};

use crate::context::Context;

#[cfg(test)]
mod tests;

/// How long a Linux terminal gets to fail on its own (one that can't start, zutty without its
/// font, exits within a moment) before it counts as open.
pub const START_GRACE: Duration = Duration::from_millis(1500);

/// How long `osascript` or `cmd` may take to open the window.
const OPEN_LIMIT: Duration = Duration::from_secs(30);

/// The variable that turns off Docker's "What's next" hints when the command exits.
const QUIET_DOCKER: (&str, &str) = ("DOCKER_CLI_HINTS", "false");

/// The Linux terminal emulators tried, in order, each with the arguments that title its window
/// and those that run a command (its program and arguments after them).
const LINUX_TERMINALS: [(&str, &[&str], &[&str]); 6] = [
    ("x-terminal-emulator", &["-T"], &["-e"]),
    ("gnome-terminal", &["--title"], &["--"]),
    ("konsole", &["-p"], &["-e"]),
    ("xfce4-terminal", &["-T"], &["-x"]),
    ("alacritty", &["-t"], &["-e"]),
    ("xterm", &["-T"], &["-e"]),
];

/// The scripts the Windows console runs: `isothrum-terminal-<unique>.cmd`.
const SCRIPT_PREFIX: &str = "isothrum-terminal-";

/// How long a console's script is kept: `cmd` read it long before.
const SCRIPT_TTL: Duration = Duration::from_secs(10 * 60);

/// Why the terminal didn't open.
#[derive(Debug)]
#[non_exhaustive]
pub enum NotOpened {
    /// macOS: this app isn't allowed to control Terminal (the Apple-event refusal `-1743`): the
    /// Automation permission, in System Settings › Privacy & Security › Automation.
    Automation {
        /// What `osascript` said.
        said: String,
    },
    /// The program that opens the window failed, or didn't start.
    Failed {
        /// The program.
        program: String,
        /// What it said, or why it didn't start.
        said: String,
    },
    /// Linux: no terminal emulator started; `line` is the command to run by hand.
    NoTerminal {
        /// The terminals tried.
        tried: Vec<String>,
        /// The command as one line a POSIX shell runs.
        line: String,
    },
    /// Windows: the console's script couldn't be written.
    Script(std::io::Error),
}

impl std::fmt::Display for NotOpened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotOpened::Automation { said } => write!(f, "not allowed to control Terminal (Automation permission): {said}"),
            NotOpened::Failed { program, said } => write!(f, "{program} couldn't open a terminal: {said}"),
            NotOpened::NoTerminal { tried, line } => {
                write!(f, "no terminal started (tried {}); run this yourself: {line}", tried.join(", "))
            }
            NotOpened::Script(e) => write!(f, "couldn't write the console's script: {e}"),
        }
    }
}

impl std::error::Error for NotOpened {}

/// Opens `command` in this computer's terminal, its window titled `title`. `scripts` is the folder
/// the Windows console's script is written to (the temp folder; unused elsewhere): its path never
/// goes through `cmd`'s own parsing, so a profile folder with `&` or a space is safe.
///
/// # Errors
///
/// [`NotOpened`]: why no window opened.
pub async fn open(cx: &Context, title: &str, command: &ShellCommand, scripts: &Path) -> Result<(), NotOpened> {
    match cx.host.os {
        HostOs::Macos => opened_by(cx, &ProcessSpec::new("osascript", ["-e".to_owned(), terminal_script(title, command)])).await,
        HostOs::Linux => linux(cx, title, command).await,
        HostOs::Windows => {
            let name = write_script(scripts, title, command).map_err(NotOpened::Script)?;
            opened_by(cx, &start_spec(scripts, &name)).await
        }
    }
}

/// Runs `spec`, a program that opens the window and exits.
async fn opened_by(cx: &Context, spec: &ProcessSpec) -> Result<(), NotOpened> {
    let failed = |said: String| NotOpened::Failed {
        program: spec.program.clone(),
        said,
    };
    let captured = tokio::time::timeout(OPEN_LIMIT, capture(cx.runner.as_ref(), spec))
        .await
        .map_err(|_| failed(format!("no answer within {} s", OPEN_LIMIT.as_secs())))?
        .map_err(|e| failed(e.to_string()))?;
    if captured.success() {
        return Ok(());
    }
    let said = captured.reason();
    // Apple's "not authorized to send Apple events" (-1743), in any language.
    if said.contains("-1743") {
        return Err(NotOpened::Automation { said });
    }
    Err(failed(said))
}

/// Linux: the first of [`LINUX_TERMINALS`] that starts, in a session of its own.
async fn linux(cx: &Context, title: &str, command: &ShellCommand) -> Result<(), NotOpened> {
    let mut tried = Vec::new();
    for (terminal, titled, runs) in LINUX_TERMINALS {
        let spec = ProcessSpec::new("setsid", linux_args(terminal, titled, runs, title, command));
        tried.push(terminal.to_owned());
        match tokio::time::timeout(START_GRACE, capture(cx.runner.as_ref(), &spec)).await {
            // Still running: its window is up (dropping the run ends `setsid` alone).
            Err(_) => return Ok(()),
            // A terminal that hands the window to its server and exits (gnome-terminal).
            Ok(Ok(captured)) if captured.success() => return Ok(()),
            // Not installed, refused, or it failed at once: the next one.
            Ok(Ok(_) | Err(_)) => {}
        }
    }
    Err(NotOpened::NoTerminal {
        tried,
        line: command.sh_line(),
    })
}

/// `setsid`'s arguments for `terminal`: it waits for the terminal (`-w`, so a failed start is
/// seen), which runs the command through `env` in its folder (`-C`) with its variables: no shell
/// reads any of it.
fn linux_args(terminal: &str, titled: &[&str], runs: &[&str], title: &str, command: &ShellCommand) -> Vec<String> {
    let title = if terminal == "konsole" {
        format!("tabtitle={title}")
    } else {
        title.to_owned()
    };
    let mut args: Vec<String> = vec!["-w".into(), terminal.into()];
    args.extend(titled.iter().map(|s| (*s).to_owned()));
    args.push(title);
    args.extend(runs.iter().map(|s| (*s).to_owned()));
    args.extend(["env".to_owned(), "-C".to_owned(), command.cwd.display().to_string()]);
    args.push(format!("{}={}", QUIET_DOCKER.0, QUIET_DOCKER.1));
    args.extend(command.env.iter().map(|(k, v)| format!("{k}={v}")));
    args.push(command.program.clone());
    args.extend(command.args.iter().cloned());
    args
}

/// `s` single-quoted for a POSIX shell when it needs it.
fn sh_word(s: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_-.,/:@%+=".contains(c);
    if !s.is_empty() && s.chars().all(plain) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// The POSIX line that runs `command` in its folder and replaces the shell with it (leaving the
/// command ends the window's session): `cd <cwd> && exec env VAR=value… program args…`.
#[must_use]
pub fn posix_line(command: &ShellCommand) -> String {
    let quiet = format!("{}={}", QUIET_DOCKER.0, QUIET_DOCKER.1);
    let words = std::iter::once("env".to_owned())
        .chain(std::iter::once(quiet))
        .chain(command.env.iter().map(|(k, v)| sh_word(&format!("{k}={v}"))))
        .chain(std::iter::once(&command.program).chain(&command.args).map(|w| sh_word(w)));
    format!(
        "cd {} && exec {}",
        sh_word(&command.cwd.display().to_string()),
        words.collect::<Vec<_>>().join(" ")
    )
}

/// `s` as the text of a double-quoted string of Apple's scripting language.
fn applescript_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The script (Apple's scripting language) that opens a Terminal window on `command`, titled `title` alone (no size,
/// device or shell path). The screen is cleared first, and the leading space keeps the line out
/// of the shell's history where `HIST_IGNORE_SPACE` is on.
fn terminal_script(title: &str, command: &ShellCommand) -> String {
    let line = applescript_string(&format!(" clear; {}", posix_line(command)));
    let title = applescript_string(title);
    format!(
        "tell application \"Terminal\"\nactivate\nset t to do script \"{line}\"\nset custom title of t to \"{title}\"\n\
         set title displays custom title of t to true\nset title displays window size of t to false\n\
         set title displays device name of t to false\nset title displays shell path of t to false\n\
         set title displays file name of t to false\nend tell"
    )
}

/// The characters `cmd.exe` reads as its own outside double quotes.
const CMD_SPECIAL: [char; 7] = ['&', '|', '<', '>', '^', '(', ')'];

/// The `cmd.exe` line: `cd /d "<cwd>" && set "VAR=value" && program args`, each word
/// double-quoted when it holds a space, a quote or a character `cmd.exe` reads as its own (a
/// profile folder `C:\Users\Tom&Jerry`).
#[must_use]
pub fn windows_line(command: &ShellCommand) -> String {
    let quote = |w: &str| {
        if w.is_empty() || w.contains([' ', '"']) || w.contains(CMD_SPECIAL) {
            format!("\"{}\"", w.replace('"', "\"\""))
        } else {
            w.to_owned()
        }
    };
    let mut run = std::iter::once(&command.program)
        .chain(&command.args)
        .map(|w| windows_word(w))
        .collect::<Vec<_>>()
        .join(" ");
    for (name, value) in command.env.iter().rev() {
        run = format!("set \"{name}={value}\" && {run}");
    }
    format!("cd /d {} && {run}", quote(&command.cwd.display().to_string()))
}

/// `w` as one argument of the program a `cmd.exe` line starts: double-quoted when it holds a
/// space, a quote or a character `cmd.exe` reads as its own, as the C runtime (and PowerShell,
/// Rust's std) read an argument back: a `"` as `\"`, the backslashes before it doubled. `cmd.exe`
/// toggles its own quoting at every `"`, `\"` too, so each of its characters it would then read
/// outside quotes is escaped with `^`. Doubling a quote (`""`) kept `cmd.exe`'s quoting whole but
/// isn't how PowerShell reads one: `powershell -Command "... ""a"" ..."` failed with "The string
/// is missing the terminator", and no command holding a quote opened.
fn windows_word(w: &str) -> String {
    if !(w.is_empty() || w.contains([' ', '\t', '"']) || w.contains(CMD_SPECIAL)) {
        return w.to_owned();
    }
    let mut argv = String::from('"');
    let mut backslashes = 0;
    for c in w.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                argv.extend(std::iter::repeat_n('\\', backslashes + 1));
                backslashes = 0;
            }
            _ => backslashes = 0,
        }
        argv.push(c);
    }
    argv.extend(std::iter::repeat_n('\\', backslashes));
    argv.push('"');
    let mut line = String::with_capacity(argv.len());
    let mut quoted = false;
    for c in argv.chars() {
        if c == '"' {
            quoted = !quoted;
        } else if !quoted && CMD_SPECIAL.contains(&c) {
            line.push('^');
        }
        line.push(c);
    }
    line
}

/// The console's script: UTF-8 read as such (`chcp 65001`, or `C:\Users\José` arrives garbled),
/// the window titled, the screen cleared, the line run, then the script removed. A `%` starts a
/// variable in a script, so the line's and the title's are doubled; the title's `cmd` characters
/// are escaped with `^`.
fn script_text(title: &str, command: &ShellCommand) -> String {
    let title: String = title
        .chars()
        .filter(|c| !c.is_control())
        .flat_map(|c| if CMD_SPECIAL.contains(&c) { vec!['^', c] } else { vec![c] })
        .collect();
    let line = format!("set \"{}={}\" && {}", QUIET_DOCKER.0, QUIET_DOCKER.1, windows_line(command));
    format!(
        "@echo off\r\nchcp 65001 >nul\r\ntitle {}\r\ncls\r\n{}\r\n(goto) 2>nul & del \"%~f0\"\r\n",
        title.replace('%', "%%"),
        line.replace('%', "%%")
    )
}

/// Writes the console's script into `dir` under a name no other console uses, made of plain
/// characters alone, and clears the scripts earlier consoles left. Returns its file name.
fn write_script(dir: &Path, title: &str, command: &ShellCommand) -> std::io::Result<String> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    clear_old_scripts(dir, SCRIPT_TTL);
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = format!("{SCRIPT_PREFIX}{}-{n}-{nanos}.cmd", std::process::id());
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(&name), script_text(title, command))?;
    Ok(name)
}

/// `cmd /c start cmd /c <name>`, run in the script's folder `dir`: the script is named by its
/// plain file name, so no path `cmd` would split (`&`, a space) is ever on a command line.
fn start_spec(dir: &Path, name: &str) -> ProcessSpec {
    let mut spec = ProcessSpec::new("cmd", ["/c", "start", "cmd", "/c", name]);
    spec.cwd = Some(dir.to_path_buf());
    spec.env = vec![(QUIET_DOCKER.0.to_owned(), QUIET_DOCKER.1.to_owned())];
    spec
}

/// Removes the console scripts in `dir` older than `ttl` (a window closed mid-session never
/// removed its own).
fn clear_old_scripts(dir: &Path, ttl: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let ours = name.starts_with(SCRIPT_PREFIX) && Path::new(&name).extension().is_some_and(|x| x.eq_ignore_ascii_case("cmd"));
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= ttl);
        if ours && old {
            // One another process removed first is gone all the same.
            drop(std::fs::remove_file(e.path()));
        }
    }
}

/// The scripts a test finds in `dir`.
#[cfg(test)]
fn scripts_in(dir: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir).map(|it| it.flatten().map(|e| e.path()).collect()).unwrap_or_default()
}
