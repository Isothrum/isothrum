use std::sync::Arc;

use isothrum_runner::fake::{FakeProcesses, Reply};
use isothrum_runner::{HostOs, ProcessRunner};

use std::path::PathBuf;

use super::*;
use crate::testing::{StuckOn, linux_host};

fn cmd(program: &str, args: &[&str], cwd: &str) -> ShellCommand {
    ShellCommand::new(program, args.iter().copied(), PathBuf::from(cwd))
}

/// A shell into a Vagrant machine: `vagrant` needs the run's state folder to find it.
fn in_vm(cwd: &str) -> ShellCommand {
    cmd("vagrant", &["ssh", "default"], cwd).with_env(vec![("VAGRANT_DOTFILE_PATH".into(), "/runs/x y/vagrant".into())])
}

/// A context on `os` whose tools `runner` answers.
fn on(os: HostOs, runner: Arc<dyn ProcessRunner>) -> Context {
    let mut host = linux_host(Path::new("/nonexistent/isothrum-machine-test"));
    host.os = os;
    Context::new(runner, host)
}

#[test]
fn the_posix_line_runs_the_command_in_its_folder_with_its_variables() {
    assert_eq!(
        posix_line(&in_vm("/projects/x/.state")),
        "cd /projects/x/.state && exec env DOCKER_CLI_HINTS=false 'VAGRANT_DOTFILE_PATH=/runs/x y/vagrant' vagrant ssh default"
    );
    assert_eq!(
        posix_line(&cmd(
            "docker",
            &["compose", "--file", "/runs/x y/compose.yml", "exec", "it's", "bash"],
            "/runs/x y"
        )),
        "cd '/runs/x y' && exec env DOCKER_CLI_HINTS=false docker compose --file '/runs/x y/compose.yml' exec 'it'\\''s' bash"
    );
}

#[test]
fn the_windows_line_quotes_words_cmd_would_read_as_its_own() {
    assert_eq!(
        windows_line(&in_vm("C:\\projects\\x")),
        "cd /d C:\\projects\\x && set \"VAGRANT_DOTFILE_PATH=/runs/x y/vagrant\" && vagrant ssh default"
    );
    // A profile folder named `Tom&Jerry` split the line at the `&` when only spaces were quoted.
    assert_eq!(
        windows_line(&cmd("docker", &["exec", "a(b)", "x|y", ""], "C:\\Users\\Tom&Jerry\\run")),
        "cd /d \"C:\\Users\\Tom&Jerry\\run\" && docker exec \"a(b)\" \"x|y\" \"\""
    );
}

/// Regression (a Windows 11 VM): a quote in an argument was doubled (`""`), which PowerShell
/// doesn't read back, so `powershell -Command "... ""a"" ..."` never ran in the window.
#[test]
fn a_quote_in_an_argument_reaches_the_program_and_cmd_specials_stay_escaped() {
    let ps = cmd("powershell", &["-Command", "\"$env:X\" | Out-File 'C:\\a b\\said'"], "C:\\run");
    assert_eq!(
        windows_line(&ps),
        "cd /d C:\\run && powershell -Command \"\\\"$env:X\\\" | Out-File 'C:\\a b\\said'\""
    );
    // cmd.exe reads `x|y` outside its quotes here (each `\"` toggles them): the `|` is escaped.
    assert_eq!(
        windows_line(&cmd("sh", &["-c", "echo \"x|y\""], "C:\\run")),
        "cd /d C:\\run && sh -c \"echo \\\"x^|y\\\"\""
    );
    // A backslash before a quote is doubled, as the C runtime reads it back.
    assert_eq!(windows_line(&cmd("p", &["a\\\"b", "c\\"], "C:\\run")), "cd /d C:\\run && p \"a\\\\\\\"b\" c\\");
}

#[test]
fn the_terminal_script_escapes_the_line_and_the_title() {
    let script = terminal_script("Shell \"one\"", &cmd("echo", &["a\\b"], "/tmp"));
    assert!(
        script.contains("do script \" clear; cd /tmp && exec env DOCKER_CLI_HINTS=false echo 'a\\\\b'\""),
        "{script}"
    );
    assert!(script.contains("set custom title of t to \"Shell \\\"one\\\"\""), "{script}");
}

#[tokio::test(start_paused = true)]
async fn macos_opens_terminal_through_osascript() {
    let fake = Arc::new(FakeProcesses::script().expect("osascript -e", Reply::ok("")).build());
    open(
        &on(HostOs::Macos, Arc::<FakeProcesses>::clone(&fake)),
        "Shell",
        &cmd("bash", &[], "/tmp"),
        Path::new("/unused"),
    )
    .await
    .unwrap();
    let call = fake.last("osascript").unwrap();
    assert!(call.args[1].contains("tell application \"Terminal\""), "{call:?}");
}

#[tokio::test(start_paused = true)]
async fn a_refused_automation_permission_is_said_as_such() {
    let denied = Reply::fail(1, "execution error: Not authorized to send Apple events to Terminal. (-1743)");
    let fake = Arc::new(FakeProcesses::script().expect("osascript", denied).build());
    let err = open(&on(HostOs::Macos, fake), "Shell", &cmd("bash", &[], "/tmp"), Path::new("/unused"))
        .await
        .unwrap_err();
    assert!(matches!(err, NotOpened::Automation { ref said } if said.contains("Not authorized")), "{err:?}");

    let failing = Arc::new(FakeProcesses::script().expect("osascript", Reply::fail(1, "boom")).build());
    let failed = open(&on(HostOs::Macos, failing), "Shell", &cmd("bash", &[], "/tmp"), Path::new("/unused"))
        .await
        .unwrap_err();
    assert!(
        matches!(failed, NotOpened::Failed { ref program, ref said } if program == "osascript" && said.contains("boom")),
        "{failed:?}"
    );
}

#[test]
fn a_linux_terminal_runs_the_command_through_env_titled() {
    let args = linux_args("xterm", &["-T"], &["-e"], "My shell", &in_vm("/runs/a b"));
    assert_eq!(
        args,
        [
            "-w",
            "xterm",
            "-T",
            "My shell",
            "-e",
            "env",
            "-C",
            "/runs/a b",
            "DOCKER_CLI_HINTS=false",
            "VAGRANT_DOTFILE_PATH=/runs/x y/vagrant",
            "vagrant",
            "ssh",
            "default"
        ]
    );
    assert_eq!(linux_args("konsole", &["-p"], &["-e"], "T", &cmd("sh", &[], "/"))[2..4], ["-p", "tabtitle=T"]);
}

#[tokio::test(start_paused = true)]
async fn linux_tries_terminals_until_one_starts() {
    let fake = Arc::new(
        FakeProcesses::script()
            .expect("setsid -w x-terminal-emulator", Reply::missing())
            .expect("setsid -w gnome-terminal", Reply::fail(1, "no display"))
            .expect("setsid -w konsole", Reply::ok(""))
            .build(),
    );
    open(
        &on(HostOs::Linux, Arc::<FakeProcesses>::clone(&fake)),
        "Shell",
        &cmd("bash", &[], "/tmp"),
        Path::new("/unused"),
    )
    .await
    .unwrap();
    assert_eq!(fake.calls().len(), 3);
}

#[tokio::test(start_paused = true)]
async fn a_linux_terminal_still_running_after_the_grace_is_open() {
    let rest = FakeProcesses::script().expect("setsid -w x-terminal-emulator", Reply::missing()).build();
    let line = "setsid -w gnome-terminal --title Shell -- env -C /tmp DOCKER_CLI_HINTS=false bash";
    let runner = Arc::new(StuckOn::new(line, rest));
    open(&on(HostOs::Linux, runner), "Shell", &cmd("bash", &[], "/tmp"), Path::new("/unused"))
        .await
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn no_linux_terminal_says_the_line_to_run() {
    let fake = Arc::new(FakeProcesses::script().expect_times("setsid", 6, Reply::missing()).build());
    let err = open(&on(HostOs::Linux, fake), "Shell", &cmd("bash", &[], "/a b"), Path::new("/unused"))
        .await
        .unwrap_err();
    let NotOpened::NoTerminal { tried, line } = err else { panic!("{err:?}") };
    assert_eq!(tried.len(), 6);
    assert_eq!(line, "cd '/a b' && bash");
}

#[tokio::test(start_paused = true)]
async fn windows_starts_a_console_on_a_script_named_by_its_plain_name() {
    let dir = tempfile::tempdir().unwrap();
    let scripts = dir.path().join("Tom&Jerry (temp)");
    let fake = Arc::new(FakeProcesses::script().expect("cmd /c start cmd /c", Reply::ok("")).build());
    let command = cmd("docker", &["exec", "box", "bash"], "C:\\Users\\Tom&Jerry\\run");
    open(&on(HostOs::Windows, Arc::<FakeProcesses>::clone(&fake)), "Shell & co 100%", &command, &scripts)
        .await
        .unwrap();

    let call = fake.last("cmd").unwrap();
    let name = call.args.last().unwrap();
    assert!(
        name.starts_with(SCRIPT_PREFIX) && Path::new(name).extension().is_some_and(|x| x == "cmd") && !name.contains(['&', ' ']),
        "{name}"
    );
    assert_eq!(call.cwd.as_deref(), Some(scripts.as_path()));
    assert_eq!(call.env("DOCKER_CLI_HINTS"), Some("false"));

    let written = std::fs::read_to_string(scripts.join(name)).unwrap();
    assert_eq!(
        written,
        "@echo off\r\nchcp 65001 >nul\r\ntitle Shell ^& co 100%%\r\ncls\r\n\
         set \"DOCKER_CLI_HINTS=false\" && cd /d \"C:\\Users\\Tom&Jerry\\run\" && docker exec box bash\r\n\
         (goto) 2>nul & del \"%~f0\"\r\n"
    );
}

#[test]
fn old_console_scripts_are_cleared_and_others_kept() {
    let dir = tempfile::tempdir().unwrap();
    for name in [format!("{SCRIPT_PREFIX}1.cmd"), "other.cmd".to_owned(), format!("{SCRIPT_PREFIX}2.txt")] {
        std::fs::write(dir.path().join(name), "").unwrap();
    }
    clear_old_scripts(dir.path(), Duration::from_secs(3600));
    assert_eq!(scripts_in(dir.path()).len(), 3, "a young script is kept");
    clear_old_scripts(dir.path(), Duration::ZERO);
    let mut left: Vec<String> = scripts_in(dir.path())
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, ["isothrum-terminal-2.txt", "other.cmd"]);
}
