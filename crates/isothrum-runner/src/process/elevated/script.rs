//! The elevated broker's PowerShell (Windows PowerShell 5.1, which every Windows has): it runs
//! the session's requests one after the other, each `req-<n>.json` (program, arguments, folder,
//! environment) read and deleted, its output lines appended to `out-<n>.log` (UTF-8) and its exit
//! code written to `exit-<n>`. A watcher job ends the broker's whole tree when the session folder
//! gets a `cancel` file, when the runner's process is gone, or at the deadline; a `done` file ends
//! it between two requests.

use std::path::Path;
use std::time::Duration;

/// The longest a session may last: a big run's first boot takes a couple of hours at most.
pub(super) const DEADLINE: Duration = Duration::from_secs(6 * 3600);

/// `s` as a PowerShell single-quoted string; the typographic single quotes PowerShell also takes
/// as quotes are doubled too.
pub(super) fn ps_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// Whether Windows PowerShell 5.1 passes `arg` to a native program intact: it drops an empty one,
/// mangles embedded double quotes, and stops parsing at `--%`.
pub(super) fn passes_intact(arg: &str) -> bool {
    !(arg.is_empty() || arg.contains(['"', '\0', '\n', '\r']) || arg == "--%")
}

/// The broker for the session in `dir`, ended with the runner's process `runner`.
pub(super) fn broker(dir: &Path, runner: u32) -> String {
    let dir = ps_quote(&dir.display().to_string());
    let secs = DEADLINE.as_secs();
    format!(
        "$ErrorActionPreference = 'Continue'\n\
         $ProgressPreference = 'SilentlyContinue'\n\
         try {{ [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding -ArgumentList $false }} catch {{}}\n\
         $d = {dir}\n\
         $null = Start-Job -ArgumentList $PID, {runner}, $d, {secs} -ScriptBlock {{\n\
         \x20 param($main, $runnerId, $d, $secs)\n\
         \x20 $runner = Get-Process -Id $runnerId -ErrorAction SilentlyContinue\n\
         \x20 $end = (Get-Date).AddSeconds($secs)\n\
         \x20 while (-not (Test-Path -LiteralPath (Join-Path $d 'done'))) {{\n\
         \x20   if ((Test-Path -LiteralPath (Join-Path $d 'cancel')) -or $null -eq $runner -or $runner.HasExited -or (Get-Date) -gt $end) {{\n\
         \x20     & taskkill.exe /T /F /PID $main | Out-Null; return\n\
         \x20   }}\n\
         \x20   Start-Sleep -Milliseconds 500\n\
         \x20 }}\n\
         }}\n\
         [System.IO.File]::WriteAllText((Join-Path $d 'ready'), '')\n\
         $n = 1\n\
         while (-not (Test-Path -LiteralPath (Join-Path $d 'done'))) {{\n\
         \x20 $req = Join-Path $d \"req-$n.json\"\n\
         \x20 if (-not (Test-Path -LiteralPath $req)) {{ Start-Sleep -Milliseconds 200; continue }}\n\
         \x20 $r = [System.IO.File]::ReadAllText($req) | ConvertFrom-Json\n\
         \x20 Remove-Item -LiteralPath $req -Force\n\
         \x20 $f = [System.IO.File]::Open((Join-Path $d \"out-$n.log\"), [System.IO.FileMode]::Append, [System.IO.FileAccess]::Write, [System.IO.FileShare]::ReadWrite)\n\
         \x20 $log = New-Object System.IO.StreamWriter -ArgumentList $f, (New-Object System.Text.UTF8Encoding -ArgumentList $false)\n\
         \x20 $log.AutoFlush = $true\n\
         \x20 foreach ($p in $r.env) {{ [System.Environment]::SetEnvironmentVariable($p.k, $p.v, 'Process') }}\n\
         \x20 $rc = 1\n\
         \x20 try {{\n\
         \x20   Set-Location -LiteralPath $r.cwd -ErrorAction Stop\n\
         \x20   $global:LASTEXITCODE = 0\n\
         \x20   & $r.program @($r.args) 2>&1 | ForEach-Object {{ $log.WriteLine([string]$_) }}\n\
         \x20   $rc = $LASTEXITCODE\n\
         \x20 }} catch {{ $log.WriteLine([string]$_) }}\n\
         \x20 foreach ($p in $r.env) {{ [System.Environment]::SetEnvironmentVariable($p.k, $null, 'Process') }}\n\
         \x20 $log.Close()\n\
         \x20 [System.IO.File]::WriteAllText((Join-Path $d \"exit-$n\"), [string]$rc)\n\
         \x20 $n++\n\
         }}\n"
    )
}

/// The arguments of `powershell` running `script`, encoded (UTF-16LE in base64), so no shell
/// reads anything into it.
pub(super) fn encoded(script: &str) -> Vec<String> {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand"]
        .into_iter()
        .map(str::to_owned)
        .chain(std::iter::once(crate::encoding::base64(&bytes)))
        .collect()
}

/// The command that starts `script` elevated: one UAC prompt, the broker hidden.
pub(super) fn launcher(script: &str) -> String {
    let args = encoded(script).iter().map(|a| ps_quote(a)).collect::<Vec<_>>().join(",");
    format!("Start-Process -FilePath powershell.exe -Verb RunAs -WindowStyle Hidden -ArgumentList {args}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_doubles_every_single_quote_powershell_reads() {
        assert_eq!(ps_quote("it's"), "'it''s'");
        assert_eq!(ps_quote("a\u{2019}b"), "'a\u{2019}\u{2019}b'");
        assert_eq!(ps_quote("C:\\Users\\ada"), "'C:\\Users\\ada'");
    }

    #[test]
    fn only_arguments_powershell_passes_intact_are_taken() {
        assert!(passes_intact("up") && passes_intact("--provider") && passes_intact("C:\\a b"));
        for bad in ["", "a\"b", "--%", "a\nb"] {
            assert!(!passes_intact(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_broker_follows_its_session_folder() {
        let s = broker(Path::new("C:\\Users\\ada\\runs\\r\\elevated"), 4242);
        for part in [
            "'C:\\Users\\ada\\runs\\r\\elevated'",
            "req-$n.json",
            "out-$n.log",
            "exit-$n",
            "'ready'",
            "'cancel'",
            "'done'",
            "taskkill.exe /T /F /PID $main",
            "4242",
        ] {
            assert!(s.contains(part), "{part}: {s}");
        }
    }

    #[test]
    fn the_script_travels_encoded() {
        let args = encoded("echo 1");
        assert_eq!(args.last().map(String::as_str), Some("ZQBjAGgAbwAgADEA"));
        assert!(launcher("echo 1").starts_with("Start-Process -FilePath powershell.exe -Verb RunAs -WindowStyle Hidden -ArgumentList '-NoProfile',"));
    }
}
