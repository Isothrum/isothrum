use std::sync::Arc;

use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::testing::linux_host;

/// Steps run by `tools`, which the test keeps.
fn on(tools: FakeProcesses) -> (Context, Arc<FakeProcesses>) {
    let tools = Arc::new(tools);
    let runner: Arc<dyn isothrum_runner::ProcessRunner> = Arc::<FakeProcesses>::clone(&tools);
    (Context::new(runner, linux_host(std::path::Path::new("/nonexistent"))), tools)
}

/// A reply that prints `stderr` on standard error and exits `code`.
fn said(code: i32, stderr: &str) -> Reply {
    Reply::fail(code, stderr)
}

/// Runs `steps` on `tools`, keeping every line.
async fn run(tools: FakeProcesses, steps: &[Step]) -> (Result<(), StepFailed>, Vec<String>) {
    let (cx, _tools) = on(tools);
    let mut lines = Vec::new();
    let ran = run_steps(&cx, steps, &mut |l: &str| lines.push(l.to_owned())).await;
    (ran, lines)
}

#[tokio::test(start_paused = true)]
async fn steps_stream_both_outputs_in_order_and_stop_at_the_first_failure() {
    let mut out = Reply::ok("out-1\nout-2");
    out.stderr = vec!["warn-1".to_owned()];
    let tools = FakeProcesses::script().expect("first", out).expect("second", Reply::fail(3, "broken")).build();
    let (ran, lines) = run(tools, &[step("first", &["a"]), step("second", &[]), step("third", &[])]).await;
    assert_eq!(
        ran,
        Err(StepFailed::Exited {
            program: "second".into(),
            code: Some(3),
            known: None
        })
    );
    assert_eq!(lines, ["$ first a", "out-1", "out-2", "warn-1", "$ second ", "broken"]);
}

#[tokio::test(start_paused = true)]
async fn pkexecs_own_refusals_name_their_cause() {
    let refused = "Error executing command as another user: Not authorized";
    let tools = FakeProcesses::script().expect("pkexec", said(126, refused)).build();
    assert_eq!(run(tools, &[step("pkexec", &["sh", "-c", "x"])]).await.0, Err(StepFailed::PromptRefused));
    let no_agent = "Error executing command as another user: No authentication agent found.";
    let tools_2 = FakeProcesses::script().expect("pkexec", said(127, no_agent)).build();
    assert_eq!(run(tools_2, &[step("pkexec", &["sh", "-c", "x"])]).await.0, Err(StepFailed::NoPolkitAgent));
    let no_tty =
        "Error creating textual authentication agent: Error opening current controlling terminal for the process (`/dev/tty'): No such device or address";
    let tools_3 = FakeProcesses::script().expect("pkexec", said(127, no_tty)).build();
    assert_eq!(run(tools_3, &[step("pkexec", &["sh", "-c", "x"])]).await.0, Err(StepFailed::NoPolkitAgent));
}

#[tokio::test(start_paused = true)]
async fn a_script_exiting_126_or_127_is_its_own_failure_unless_pkexec_said_so() {
    // The script's own `command not found` (127): not pkexec's refusal.
    let tools = FakeProcesses::script().expect("pkexec", said(127, "sh: 1: gpg: not found")).build();
    let failed = Err(StepFailed::Exited {
        program: "pkexec".into(),
        code: Some(127),
        known: None,
    });
    assert_eq!(run(tools, &[step("pkexec", &["sh", "-c", "gpg"])]).await.0, failed);
    // pkexec's words, but from another program: never read.
    let tools_2 = FakeProcesses::script()
        .expect("sh", said(126, "Error executing command as another user: Not authorized"))
        .build();
    assert_eq!(
        run(tools_2, &[step("sh", &["-c", "x"])]).await.0,
        Err(StepFailed::Exited {
            program: "sh".into(),
            code: Some(126),
            known: None
        })
    );
    // Its words with another code: the script's failure.
    let tools_3 = FakeProcesses::script()
        .expect("pkexec", said(1, "Error executing command as another user: Not authorized"))
        .build();
    assert_eq!(
        run(tools_3, &[step("pkexec", &[])]).await.0,
        Err(StepFailed::Exited {
            program: "pkexec".into(),
            code: Some(1),
            known: None
        })
    );
}

#[tokio::test(start_paused = true)]
async fn a_script_stopping_where_it_doesnt_serve_says_which_only_when_its_step_says_it_may() {
    let docker = || step("pkexec", &["sh", "-c", "x"]).not_served(NotServed::DockerDistribution);
    let tools = FakeProcesses::script().expect("pkexec", said(NOT_SERVED, "")).build();
    assert_eq!(run(tools, &[docker()]).await.0, Err(StepFailed::NotServed(NotServed::DockerDistribution)));
    // Another code from that step, or that code from another step: the script's failure.
    let tools_2 = FakeProcesses::script().expect("pkexec", said(1, "")).build();
    assert_eq!(
        run(tools_2, &[docker()]).await.0,
        Err(StepFailed::Exited {
            program: "pkexec".into(),
            code: Some(1),
            known: None
        })
    );
    let tools_3 = FakeProcesses::script().expect("pkexec", said(NOT_SERVED, "")).build();
    assert_eq!(
        run(tools_3, &[step("pkexec", &["sh", "-c", "x"])]).await.0,
        Err(StepFailed::Exited {
            program: "pkexec".into(),
            code: Some(NOT_SERVED),
            known: None
        })
    );
}

#[tokio::test(start_paused = true)]
async fn an_unattended_sudo_that_needs_a_password_changed_nothing() {
    let tools = FakeProcesses::script().expect("sudo -n", said(1, "sudo: a password is required")).build();
    assert_eq!(
        run(tools, &[step("sudo", &["-n", "env", "sh", "-c", "x"])]).await.0,
        Err(StepFailed::SudoRefused)
    );
    // Another sudo line, or a sudo that asks: the script's failure.
    let tools_2 = FakeProcesses::script().expect("sudo", said(1, "sudo: a password is required")).build();
    assert_eq!(
        run(tools_2, &[step("sudo", &["env"])]).await.0,
        Err(StepFailed::Exited {
            program: "sudo".into(),
            code: Some(1),
            known: None
        })
    );
}

#[tokio::test(start_paused = true)]
async fn a_missing_program_says_so_and_pkexec_its_own_way() {
    let tools = FakeProcesses::script()
        .expect("pkexec", Reply::missing())
        .expect("vagrant", Reply::missing())
        .build();
    let (cx, _tools) = on(tools);
    let mut quiet = |_: &str| {};
    assert_eq!(run_step(&cx, &step("pkexec", &[]), &mut quiet).await, Err(StepFailed::NoPkexec));
    let missing = run_step(&cx, &step("vagrant", &["plugin", "install", "vagrant-qemu"]), &mut quiet).await;
    assert_eq!(missing, Err(StepFailed::Missing { program: "vagrant".into() }));
}

#[test]
fn a_command_line_reads_as_typed() {
    assert_eq!(
        command_line(&step("pkexec", &["env", "sh", "-c", "apt-get update"])),
        "$ pkexec env sh -c apt-get update"
    );
    assert_eq!(
        StepFailed::Exited {
            program: "pkexec".into(),
            code: None,
            known: None
        }
        .to_string(),
        "`pkexec` was ended by a signal"
    );
}

#[tokio::test(start_paused = true)]
async fn a_failing_brew_step_says_homebrews_own_trouble_not_its_exit_code() {
    let mut locked = Reply::fail(1, "Error: Another `brew update` process is already running.");
    locked.stderr.push("Error: unknown or unsupported macOS version: :dunno".to_owned());
    let tools = FakeProcesses::script().expect("/opt/homebrew/bin/brew install", locked).build();
    let (ran, _) = run(tools, &[step("/opt/homebrew/bin/brew", &["install", "qemu"])]).await;
    assert_eq!(ran, Err(StepFailed::Homebrew(crate::install::brew::Trouble::Locked)), "the first cause wins");
    // The same words from a step that succeeds, or that isn't Homebrew's: no trouble.
    let mut fine = Reply::ok("");
    fine.stderr = vec!["Error: unknown or unsupported macOS version: :dunno".to_owned()];
    let tools_ok = FakeProcesses::script().expect("/opt/homebrew/bin/brew", fine).build();
    assert_eq!(run(tools_ok, &[step("/opt/homebrew/bin/brew", &["install", "qemu"])]).await.0, Ok(()));
    let tools_apt = FakeProcesses::script()
        .expect("apt-get", Reply::fail(1, "Error: unknown or unsupported macOS version"))
        .build();
    assert_eq!(
        run(tools_apt, &[step("apt-get", &[])]).await.0,
        Err(StepFailed::Exited {
            program: "apt-get".into(),
            code: Some(1),
            known: None
        })
    );
}

#[tokio::test(start_paused = true)]
async fn a_step_with_a_note_is_not_announced_by_its_command_line() {
    let mut noted = step("sh", &["-c", "x"]);
    noted.note = Some(Note::QemuUpdatingHomebrewFirst);
    let tools = FakeProcesses::script().expect("sh", Reply::ok("done")).build();
    assert_eq!(run(tools, &[noted]).await.1, ["done"]);
}

#[tokio::test(start_paused = true)]
async fn winget_and_an_installer_through_powershell_end_by_their_own_codes() {
    use crate::install::exit_codes::{Done, Known};
    let installer_failed = 0x8A15_0006_u32.cast_signed();
    let mut cancelled = Reply::fail(installer_failed, "");
    cancelled.stdout = vec!["Installer failed with exit code: 1602".to_owned()];
    let tools = FakeProcesses::script().expect("winget", cancelled).build();
    let failed = Err(StepFailed::Exited {
        program: "winget".into(),
        code: Some(installer_failed),
        known: Some(Known::Cancelled),
    });
    assert_eq!(run(tools, &[step("winget", &["install"])]).await.0, failed);
    let tools_done = FakeProcesses::script().expect("winget", Reply::fail(0x8A15_002B_u32.cast_signed(), "")).build();
    let (cx, _tools) = on(tools_done);
    assert_eq!(
        run_step(&cx, &step("winget", &["install"]), &mut |_: &str| {}).await,
        Ok(Some(Done::AlreadyInstalled))
    );
    let tools_ps = FakeProcesses::script().expect("powershell", Reply::fail(1223, "")).build();
    let declined = run(tools_ps, &[step("powershell", &["-Command", "x"])]).await.0;
    assert_eq!(
        declined,
        Err(StepFailed::Exited {
            program: "powershell".into(),
            code: Some(1223),
            known: Some(Known::Cancelled)
        })
    );
}

/// Regression: on Windows a step ran with the `PATH` this process started with, so Vagrant's
/// plugin step right after Vagrant's install (in one setup, or a process started before it)
/// failed with `vagrant` missing: the installer had written its folder to the registry only.
#[tokio::test(start_paused = true)]
async fn on_windows_a_step_runs_with_the_path_installers_wrote_since_this_process_started() {
    use crate::host::tool_path::Env;
    let vagrant = tempfile::tempdir().unwrap();
    let installed = vagrant.path().to_string_lossy().into_owned();
    let registry = format!("HKEY_LOCAL_MACHINE\\...\n    Path    REG_SZ    {installed}\n");
    let tools = FakeProcesses::model(move |call| match (call.program.as_str(), call.args.get(1).map(String::as_str)) {
        ("reg", Some(key)) if key.starts_with("HKLM") => Reply::ok(&registry),
        ("reg", _) => Reply::fail(1, "ERROR: The system was unable to find the specified registry key or value."),
        _ => Reply::ok(""),
    });
    let tools = Arc::new(tools);
    let runner: Arc<dyn isothrum_runner::ProcessRunner> = Arc::<FakeProcesses>::clone(&tools);
    let mut host = linux_host(std::path::Path::new("/nonexistent"));
    host.os = crate::host::HostOs::Windows;
    let cx = Context::new(runner, host);
    let started = Env {
        path: vec![std::path::PathBuf::from(r"C:\Windows\system32")],
        ..Env::default()
    };
    let plugin = crate::install::vagrant_plugin("vagrant-qemu").unwrap();
    run_step_on(&cx, &plugin, &started, &mut |_: &str| {}).await.unwrap();
    let call = tools.made("vagrant plugin install").pop().unwrap();
    assert_eq!(call.env("PATH"), Some(format!(r"C:\Windows\system32;{installed}").as_str()));
}

/// Elsewhere a step runs with this process's own `PATH`: no installer there changes it behind a
/// running process, and nothing more is asked.
#[tokio::test(start_paused = true)]
async fn elsewhere_a_step_runs_with_the_process_own_path() {
    let tools = FakeProcesses::script().expect("vagrant plugin install vagrant-libvirt", Reply::ok("")).build();
    let (cx, tools) = on(tools);
    let plugin = crate::install::vagrant_plugin("vagrant-libvirt").unwrap();
    run_step(&cx, &plugin, &mut |_: &str| {}).await.unwrap();
    assert_eq!(tools.made("vagrant").pop().unwrap().env("PATH"), None);
}
