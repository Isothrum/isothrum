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
            code: Some(3)
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
            code: Some(126)
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
            code: Some(1)
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
            code: Some(1)
        })
    );
    let tools_3 = FakeProcesses::script().expect("pkexec", said(NOT_SERVED, "")).build();
    assert_eq!(
        run(tools_3, &[step("pkexec", &["sh", "-c", "x"])]).await.0,
        Err(StepFailed::Exited {
            program: "pkexec".into(),
            code: Some(NOT_SERVED)
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
            code: Some(1)
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
            code: None
        }
        .to_string(),
        "`pkexec` was ended by a signal"
    );
}
