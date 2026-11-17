use proptest::prelude::*;

use super::*;

const PREFIX: &str = "Installer failed with exit code:";

/// winget's code `c` as the process exit code it comes as, when one came.
fn code(c: u32) -> Option<i32> {
    i32::try_from(c).ok().or(Some(c.cast_signed()))
}

#[test]
fn a_declined_uac_prompt_reads_as_cancelled() {
    // Docker Desktop's installer exits 0xFFFFFFFB, winget then 0x8A150006.
    let installer = installer_exit_code("Installer failed with exit code: 4294967291");
    assert_eq!(installer, Some(0xFFFF_FFFB));
    assert_eq!(winget_failure(code(0x8A15_0006), installer), Some(Known::Cancelled));
    // An MSI its user backed out of, and winget's own "cancelled by user".
    assert_eq!(winget_failure(code(0x8A15_0006), Some(1602)), Some(Known::Cancelled));
    assert_eq!(winget_failure(code(0x8A15_010C), None), Some(Known::Cancelled));
}

#[test]
fn an_installer_error_reads_as_failed() {
    assert_eq!(winget_failure(code(0x8A15_0006), Some(1603)), Some(Known::InstallerError));
    // Not English output: winget's code alone still says it failed.
    assert_eq!(winget_failure(code(0x8A15_0006), None), Some(Known::InstallerError));
    assert_eq!(winget_failure(code(0x8A15_0105), None), Some(Known::NoDiskSpace));
}

#[test]
fn an_up_to_date_package_or_a_pending_restart_is_done_not_failed() {
    // `winget install` of a package already there exits 0x8A15002B, and Docker Desktop's or
    // WSL's install 0x8A150109 (restart to finish): both read as failed, so the step never
    // counted as done.
    assert_eq!(winget_done(code(0x8A15_002B)), Some(Done::AlreadyInstalled));
    assert_eq!(winget_done(code(0x8A15_0109)), Some(Done::RestartToFinish));
    // A restart needed before installing is still a failure, as is anything else.
    assert_eq!(winget_done(code(0x8A15_010A)), None);
    assert_eq!(winget_failure(code(0x8A15_010A), None), Some(Known::RestartFirst));
    assert_eq!(winget_done(code(0x8A15_0006)), None);
    assert_eq!(winget_done(None), None);
}

#[test]
fn unknown_codes_mean_nothing_known() {
    assert_eq!(winget_failure(Some(1), None), None);
    assert_eq!(winget_failure(None, None), None);
    assert_eq!(installer_failure(Some(1)), None);
    assert_eq!(installer_failure(None), None);
    assert_eq!(installer_failure(Some(1223)), Some(Known::Cancelled));
    assert_eq!(installer_failure(Some(-5)), Some(Known::Cancelled));
}

#[test]
fn winget_codes_at_the_edges_map_without_overflow() {
    assert_eq!(winget_failure(Some(i32::MIN), None), None);
    assert_eq!(winget_failure(Some(-1), None), None);
    // winget's own "cancelled" wins whatever the installer said; an installer cancel only counts
    // under winget's "installer failed" code, and 0 is never a cancel.
    assert_eq!(winget_failure(code(0x8A15_010C), Some(1603)), Some(Known::Cancelled));
    assert_eq!(winget_failure(code(0x8A15_0105), Some(1602)), Some(Known::NoDiskSpace));
    assert_eq!(winget_failure(code(0x8A15_0006), Some(0)), Some(Known::InstallerError));
}

#[test]
fn installer_codes_parse_in_each_form_and_at_the_edges() {
    let parsed = |rest: &str| installer_exit_code(&format!("{PREFIX} {rest}"));
    assert_eq!(parsed("1602"), Some(1602));
    assert_eq!(installer_exit_code("  Installer failed with exit code: -5 "), Some(0xFFFF_FFFB));
    assert_eq!(parsed("0x80070643"), Some(0x8007_0643));
    assert_eq!(parsed("0x80000000"), Some(0x8000_0000));
    assert_eq!(parsed("-2147483648"), Some(0x8000_0000));
    assert_eq!(parsed("4294967295"), Some(u32::MAX));
    assert_eq!(parsed("0X10"), Some(16));
    for bad in ["4294967296", "-2147483649", "0x", "0xFFFFFFFFF", "1602 (cancelled)", "+", "", "soon"] {
        assert_eq!(parsed(bad), None, "{bad:?}");
    }
    assert_eq!(installer_exit_code("Starting package install..."), None);
}

proptest! {
    #[test]
    fn any_line_reads_without_a_panic(line in "(?s).{0,80}", rest in "[0-9a-fA-FxX+ -]{0,14}") {
        let read: [Option<u32>; 3] = [
            installer_exit_code(&line),
            installer_exit_code(&format!("{PREFIX}{rest}")),
            installer_exit_code(&format!("{PREFIX} {line}")),
        ];
        prop_assert_eq!(read.len(), 3);
    }

    #[test]
    fn the_exit_code_reads_back_in_every_form_winget_prints(
        exit in any::<u32>(),
        ws in "[ \t]{0,3}",
        upper in any::<bool>(),
        crlf in any::<bool>(),
    ) {
        let end = if crlf { "\r" } else { "" };
        let hex = if upper { format!("0X{exit:X}") } else { format!("0x{exit:x}") };
        for v in [exit.to_string(), hex, format!("0x{exit:08X}")] {
            prop_assert_eq!(installer_exit_code(&format!("{ws}{PREFIX}{ws}{v}{ws}{end}")), Some(exit), "{}", v);
        }
        // A negative HRESULT as a signed number is the same code.
        prop_assert_eq!(installer_exit_code(&format!("{PREFIX} {}", exit.cast_signed())), Some(exit));
    }

    #[test]
    fn a_code_out_of_range_or_another_line_is_none(big in (u64::from(u32::MAX) + 1)..u64::MAX, word in "[a-zA-Z ]{1,20}") {
        prop_assert_eq!(installer_exit_code(&format!("{PREFIX} {big}")), None);
        prop_assert_eq!(installer_exit_code(&format!("{PREFIX} 0x{big:x}")), None);
        prop_assume!(!word.trim().is_empty());
        prop_assert_eq!(installer_exit_code(&format!("{PREFIX} {word}")), None);
    }

    /// A known meaning only for codes known to mean something, and a cancelled installer is a
    /// cancel whatever winget's own code around it.
    #[test]
    fn meanings_are_only_for_known_codes(
        exit in prop_oneof![any::<i32>(), Just(WINGET_INSTALLER_FAILED.cast_signed()), Just(0x8A15_010C_u32.cast_signed())],
        installer in proptest::option::of(prop_oneof![any::<u32>(), Just(1223u32), Just(1602u32)]),
    ) {
        let got = winget_failure(Some(exit), installer);
        let cancelled = installer.is_some_and(installer_cancelled) && exit.cast_unsigned() == WINGET_INSTALLER_FAILED;
        prop_assert_eq!(got.is_some(), cancelled || winget_summary(exit.cast_unsigned()).is_some());
        if cancelled {
            prop_assert_eq!(got, Some(Known::Cancelled));
        }
        prop_assert_eq!(winget_failure(None, installer), None);
        prop_assert_eq!(installer_failure(Some(exit)).is_some(), installer_cancelled(exit.cast_unsigned()));
    }
}
