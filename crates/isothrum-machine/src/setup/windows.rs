//! What a setup does on Windows beyond installs: this account joining "Hyper-V Administrators"
//! through one UAC prompt ([`join_hyperv_admins`]), so Vagrant drives Hyper-V for it without an
//! elevated token from its next sign-in on; and the restart that finishes turning a Windows
//! feature on ([`schedule_restart`], [`super::Action::Restart`]).

use std::time::Duration;

use crate::context::Context;
use crate::exec::all_lines;
use crate::install::{Step, StepFailed, run_step, step};

/// Adds the account running it to "Hyper-V Administrators" (`S-1-5-32-578`, by SID: the group's
/// name is translated), elevated through the UAC prompt (`-Verb RunAs`). The account is read
/// before the prompt (an administrator approving it for a standard account runs the elevated half
/// as themselves), and handed to the elevated half inside its own encoded script. One already in
/// the group is fine; a declined prompt exits 1223 (`ERROR_CANCELLED`), which reads as cancelled.
const JOIN_HYPERV_ADMINS: &str = "$a = [Security.Principal.WindowsIdentity]::GetCurrent().Name -replace '''', ''''''; \
$s = '$ErrorActionPreference = ''Stop''; try { Add-LocalGroupMember -SID ''S-1-5-32-578'' -Member ''' + $a + ''' } catch [Microsoft.PowerShell.Commands.MemberExistsException] { }'; \
$e = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($s)); \
try { $p = Start-Process -FilePath powershell.exe -ArgumentList '-NoProfile','-NonInteractive','-EncodedCommand',$e -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode } catch { exit 1223 }";

/// The step that adds this account to "Hyper-V Administrators" through one UAC prompt: the
/// membership applies from the account's next sign-in.
#[must_use]
pub fn join_hyperv_admins_step() -> Step {
    step("powershell", &["-NoProfile", "-NonInteractive", "-Command", JOIN_HYPERV_ADMINS])
}

/// Adds this account to "Hyper-V Administrators" ([`join_hyperv_admins_step`]), each line it
/// prints handed to `on_line`.
///
/// # Errors
///
/// [`StepFailed`]: a declined prompt is [`StepFailed::Exited`] with
/// [`crate::install::exit_codes::Known::Cancelled`].
pub async fn join_hyperv_admins(cx: &Context, on_line: &mut (dyn FnMut(&str) + Send)) -> Result<(), StepFailed> {
    run_step(cx, &join_hyperv_admins_step(), on_line).await.map(drop)
}

/// The longest note a restart shows while Windows counts down (`shutdown.exe` takes 512).
pub const RESTART_NOTE_MAX: usize = 200;

/// `shutdown.exe`'s refusal because a shutdown is already scheduled (`ERROR_SHUTDOWN_IS_SCHEDULED`).
const ERROR_SHUTDOWN_IS_SCHEDULED: u32 = 1190;

/// How a restart was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restart {
    /// Windows counts down to it.
    Scheduled,
    /// One was scheduled already (another window's, the user's own): the same restart.
    AlreadyScheduled,
}

/// Why a restart wasn't scheduled.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RestartFailed {
    /// The note isn't one line of at most [`RESTART_NOTE_MAX`] characters.
    Note,
    /// `shutdown.exe` didn't run, or refused: why.
    Refused(String),
}

impl std::fmt::Display for RestartFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RestartFailed::Note => write!(f, "a restart's note is one line of at most {RESTART_NOTE_MAX} characters"),
            RestartFailed::Refused(why) => write!(f, "the restart couldn't be scheduled: {why}"),
        }
    }
}

impl std::error::Error for RestartFailed {}

/// Asks Windows to restart in `after` (`shutdown /r /t <s> /c <note>`), its countdown showing
/// `note`. A restart scheduled already is [`Restart::AlreadyScheduled`], not a failure.
///
/// # Errors
///
/// [`RestartFailed`]: the note isn't one, or `shutdown.exe` didn't run or refused.
pub async fn schedule_restart(cx: &Context, after: Duration, note: &str) -> Result<Restart, RestartFailed> {
    let note = note.trim();
    if note.is_empty() || note.chars().count() > RESTART_NOTE_MAX || note.chars().any(char::is_control) {
        return Err(RestartFailed::Note);
    }
    let seconds = after.as_secs().to_string();
    let captured = cx
        .probe("shutdown", &["/r", "/t", &seconds, "/c", note])
        .await
        .map_err(|e| RestartFailed::Refused(e.to_string()))?;
    if captured.success() {
        Ok(Restart::Scheduled)
    } else if win32_error(&all_lines(&captured)) == Some(ERROR_SHUTDOWN_IS_SCHEDULED) {
        Ok(Restart::AlreadyScheduled)
    } else {
        Err(RestartFailed::Refused(captured.reason()))
    }
}

/// The Win32 error `said` ends with: Windows' own tools end their message with it in
/// parentheses (`A system shutdown has already been scheduled.(1190)`) in every display language.
fn win32_error(said: &str) -> Option<u32> {
    let said = said.trim_end().trim_end_matches('.');
    let (_, number) = said.strip_suffix(')')?.rsplit_once('(')?;
    number.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use isothrum_runner::fake::{FakeProcesses, Reply};

    use super::*;
    use crate::install::exit_codes::Known;
    use crate::testing::answered_by;

    #[test]
    fn joining_hyperv_administrators_is_one_uac_prompt_by_the_groups_sid() {
        let step = join_hyperv_admins_step();
        assert_eq!(step.program, "powershell");
        let script = step.args.last().unwrap();
        for part in [
            "Add-LocalGroupMember -SID ''S-1-5-32-578''",
            "MemberExistsException",
            "WindowsIdentity]::GetCurrent().Name",
            "-Verb RunAs",
            "-Wait -PassThru",
            "catch { exit 1223 }",
        ] {
            assert!(script.contains(part), "{part} in {script}");
        }
        // Quoted for PowerShell alone: a double quote would be escaped for the C runtime instead.
        assert!(!script.contains('"'), "{script}");
    }

    #[tokio::test]
    async fn a_declined_prompt_to_join_reads_as_cancelled() {
        let cx = answered_by(FakeProcesses::script().expect("powershell *", Reply::fail(1223, "")).build());
        let err = join_hyperv_admins(&cx, &mut |_| {}).await.unwrap_err();
        assert!(
            matches!(
                err,
                StepFailed::Exited {
                    known: Some(Known::Cancelled),
                    ..
                }
            ),
            "{err:?}"
        );
        let joined = answered_by(FakeProcesses::script().expect("powershell *", Reply::ok("")).build());
        assert_eq!(join_hyperv_admins(&joined, &mut |_| {}).await, Ok(()));
    }

    #[tokio::test]
    async fn a_restart_is_scheduled_with_its_note_or_found_scheduled_already() {
        let fake = std::sync::Arc::new(FakeProcesses::script().expect("shutdown /r /t 5 /c", Reply::ok("")).build());
        let cx = crate::testing::shared(std::sync::Arc::<FakeProcesses>::clone(&fake));
        assert_eq!(
            schedule_restart(&cx, Duration::from_secs(5), " Restarting to finish WSL 2. ").await,
            Ok(Restart::Scheduled)
        );
        assert_eq!(fake.made("shutdown")[0].args.last().map(String::as_str), Some("Restarting to finish WSL 2."));

        let french = "Un arrêt du système a déjà été planifié.(1190)\r";
        let scheduled = answered_by(FakeProcesses::script().expect("shutdown *", Reply::fail(1, french)).build());
        assert_eq!(
            schedule_restart(&scheduled, Duration::from_secs(5), "note").await,
            Ok(Restart::AlreadyScheduled)
        );

        let denied = answered_by(FakeProcesses::script().expect("shutdown *", Reply::fail(1, "Access is denied.(5)")).build());
        assert!(
            matches!(schedule_restart(&denied, Duration::from_secs(5), "note").await, Err(RestartFailed::Refused(why)) if why.contains("Access is denied"))
        );
    }

    #[tokio::test]
    async fn a_restart_note_is_one_short_line() {
        let cx = answered_by(FakeProcesses::script().build());
        for note in ["", "two\nlines", &"x".repeat(RESTART_NOTE_MAX + 1)] {
            assert_eq!(schedule_restart(&cx, Duration::from_secs(5), note).await, Err(RestartFailed::Note), "{note:?}");
        }
    }

    #[test]
    fn the_win32_error_is_read_from_the_end_whatever_the_language() {
        assert_eq!(win32_error("A system shutdown has already been scheduled.(1190)\r\n"), Some(1190));
        assert_eq!(win32_error("listening on port 1190"), None);
        assert_eq!(win32_error(""), None);
    }
}
