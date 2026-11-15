//! The elevated broker's PowerShell (Windows PowerShell 5.1, which every Windows has): it runs
//! the session's requests one after the other. Request `n` is `req-<n>.json` (program,
//! arguments, folder) and `env-<n>.json` (its variables), each read and deleted before the
//! program starts; one that doesn't carry the session's token and `n` is deleted unrun. The
//! program runs in a job of its own (a child process whose id the broker reads first), its
//! variables set there only; its output lines are appended to `out-<n>.log` (UTF-8) and its exit
//! code is written to `exit-<n>` whole (renamed into place).
//!
//! What it runs is its own choice, not the request's: only Vagrant and Windows PowerShell, each
//! found by the broker at the path an administrator installed it to and owned by an administrator
//! (the file and every folder above it), with Windows' own folders as its `PATH`. A request naming
//! any other program, or setting a variable that chooses which code the program loads (its search
//! path, Ruby's load path and options, an unlisted `VAGRANT_*`), is refused unrun; those of them
//! the broker itself inherited (Ruby's and its gems' settings, an unlisted `VAGRANT_*`) are
//! dropped before it takes a request, so its programs never inherit one either.
//!
//! A request is cancelled on its own, never the session: a `cancel-<n>` file ends request `n`'s
//! process tree if it runs, or skips it if its turn hasn't come (its files deleted unrun), and
//! the broker moves on to `n + 1`. A cancelled request gets no `exit-<n>`: nobody waits for it.
//! A watcher job ends the broker's whole tree when the session folder is gone or the runner's
//! process is; a `done` file (or the folder gone) ends it, the request in flight cancelled. A
//! session lasts as long as the operation that opened it: no deadline of its own cuts a long one
//! short (each request is held to its step's limit by the runner). A broker that stops other
//! than at `done` says why in an `ended` file, so the request waiting on it fails at once instead
//! of waiting for an answer that won't come.

use std::path::Path;

use super::protocol::{self, DONE, ENDED, READY};

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

/// The broker for the session in `dir`, taking only requests that carry `token`, ended with the
/// runner's process `runner`: its settings, its watcher, the job a request runs in, and its loop.
pub(super) fn broker(dir: &Path, token: &str, runner: u32) -> String {
    let [dir, token, done, ended] = [&*dir.display().to_string(), token, DONE, ENDED].map(ps_quote);
    let preamble = format!(
        "$ErrorActionPreference = 'Continue'\n\
         $ProgressPreference = 'SilentlyContinue'\n\
         try {{ [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding -ArgumentList $false }} catch {{}}\n\
         $d = {dir}\n\
         $token = {token}\n\
         function Write-Ended($why) {{ if (-not (Test-Path -LiteralPath (Join-Path $d {done}))) {{ try {{ [System.IO.File]::WriteAllText((Join-Path $d {ended}), $why) }} catch {{}} }} }}\n\
         function Remove-Quietly($paths) {{ foreach ($p in $paths) {{ Remove-Item -LiteralPath $p -Force -ErrorAction SilentlyContinue }} }}\n\
         function Test-Over {{ (-not (Test-Path -LiteralPath $d)) -or (Test-Path -LiteralPath (Join-Path $d {done})) }}\n"
    );
    let watcher = format!(
        "$null = Start-Job -ArgumentList $PID, {runner}, $d -ScriptBlock {{\n\
         \x20 param($main, $runnerId, $d)\n\
         \x20 $runner = Get-Process -Id $runnerId -ErrorAction SilentlyContinue\n\
         \x20 while (-not (Test-Path -LiteralPath (Join-Path $d {done}))) {{\n\
         \x20   $why = $null\n\
         \x20   if (-not (Test-Path -LiteralPath $d)) {{ $why = 'its folder is gone' }}\n\
         \x20   elseif ($null -eq $runner -or $runner.HasExited) {{ $why = 'the runner is gone' }}\n\
         \x20   if ($null -ne $why) {{\n\
         \x20     try {{ [System.IO.File]::WriteAllText((Join-Path $d {ended}), $why) }} catch {{}}\n\
         \x20     & taskkill.exe /T /F /PID $main | Out-Null; return\n\
         \x20   }}\n\
         \x20   Start-Sleep -Milliseconds 500\n\
         \x20 }}\n\
         }}\n"
    );
    [preamble.as_str(), &allowed(), watcher.as_str(), REQUEST_JOB, &request_loop()].concat()
}

/// The account owning a file the broker runs, or a folder holding one: Administrators, SYSTEM,
/// or `TrustedInstaller`. Anything an administrator installed is owned so; a file or folder any
/// other account made is not.
const TRUSTED_OWNERS: [&str; 3] = ["S-1-5-32-544", "S-1-5-18", "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"];

/// What the broker allows: each program it runs, found by the broker at paths an administrator
/// installed it to, and owned (with every folder above it) by an administrator, never on a
/// `PATH` ([`protocol::PROGRAMS`]); the `PATH` it runs them with, Windows' own folders after the
/// program's; the variables a request may set ([`protocol::passes`]); and the broker's own
/// environment rid of those that choose which code its programs load ([`protocol::loads_code`]).
fn allowed() -> String {
    let list = |items: &[&str]| items.iter().map(|i| ps_quote(i)).collect::<Vec<_>>().join(", ");
    let [owners, vagrant_vars, system_vars, loaders] = [
        &TRUSTED_OWNERS[..],
        &protocol::VAGRANT_VARS[..],
        &protocol::SYSTEM_VARS[..],
        &protocol::LOADER_PREFIXES[..],
    ]
    .map(list);
    format!(
        "$sys = [System.Environment]::SystemDirectory\n\
         $programFiles = [System.Environment]::GetFolderPath('ProgramFiles')\n\
         $programs = @{{\n\
         \x20 'vagrant' = @((Join-Path $programFiles 'Vagrant\\bin\\vagrant.exe'), (Join-Path $env:SystemDrive 'HashiCorp\\Vagrant\\bin\\vagrant.exe'))\n\
         \x20 'powershell' = @((Join-Path $sys 'WindowsPowerShell\\v1.0\\powershell.exe'))\n\
         }}\n\
         $systemPath = @($sys, (Join-Path $sys 'Wbem'), (Join-Path $sys 'WindowsPowerShell\\v1.0'), (Join-Path $sys 'OpenSSH'), [System.Environment]::GetFolderPath('Windows'))\n\
         $modulePath = @((Join-Path $sys 'WindowsPowerShell\\v1.0\\Modules'), (Join-Path $programFiles 'WindowsPowerShell\\Modules')) -join ';'\n\
         [System.Environment]::SetEnvironmentVariable('PSModulePath', $modulePath, 'Process')\n\
         $trustedOwners = @({owners})\n\
         function Test-Owned($path) {{\n\
         \x20 $p = $path\n\
         \x20 while ($p -and $p -ne [System.IO.Path]::GetPathRoot($p)) {{\n\
         \x20   try {{ $owner = (Get-Acl -LiteralPath $p).GetOwner([System.Security.Principal.SecurityIdentifier]).Value }} catch {{ return $false }}\n\
         \x20   if ($trustedOwners -notcontains $owner) {{ return $false }}\n\
         \x20   $p = Split-Path -Parent $p\n\
         \x20 }}\n\
         \x20 $true\n\
         }}\n\
         function Resolve-Program($name) {{\n\
         \x20 if ($name -isnot [string] -or -not $programs.ContainsKey($name)) {{ return $null }}\n\
         \x20 foreach ($p in $programs[$name]) {{ if ((Test-Path -LiteralPath $p -PathType Leaf) -and (Test-Owned $p)) {{ return $p }} }}\n\
         \x20 $null\n\
         }}\n\
         $vagrantVars = @({vagrant_vars})\n\
         $systemVars = @({system_vars})\n\
         $loaderPrefixes = @({loaders})\n\
         function Test-Loader($k) {{\n\
         \x20 $u = $k.ToUpperInvariant()\n\
         \x20 if ($u.StartsWith('VAGRANT_')) {{ return $vagrantVars -notcontains $u }}\n\
         \x20 foreach ($p in $loaderPrefixes) {{ if ($u.StartsWith($p)) {{ return $true }} }}\n\
         \x20 $false\n\
         }}\n\
         function Test-Var($k) {{\n\
         \x20 if ($k -isnot [string] -or $k -eq '' -or $k.Contains('=')) {{ return $false }}\n\
         \x20 -not (($systemVars -contains $k.ToUpperInvariant()) -or (Test-Loader $k))\n\
         }}\n\
         foreach ($v in @(Get-ChildItem Env:)) {{ if (Test-Loader $v.Name) {{ [System.Environment]::SetEnvironmentVariable($v.Name, $null, 'Process') }} }}\n"
    )
}

/// The job a request runs in, a process of its own: its id first (what a cancel ends, its tree),
/// its variables set there only, its output lines, then its exit code.
const REQUEST_JOB: &str = "$run = {\n\
         \x20 param($r, $e, $exe, $path, $modules)\n\
         \x20 [pscustomobject]@{ isothrumPid = $PID }\n\
         \x20 try { [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding -ArgumentList $false } catch {}\n\
         \x20 [System.Environment]::SetEnvironmentVariable('PATH', $path, 'Process')\n\
         \x20 [System.Environment]::SetEnvironmentVariable('PSModulePath', $modules, 'Process')\n\
         \x20 foreach ($p in $e.env) { [System.Environment]::SetEnvironmentVariable($p.k, $p.v, 'Process') }\n\
         \x20 try {\n\
         \x20   Set-Location -LiteralPath $r.cwd -ErrorAction Stop\n\
         \x20   $global:LASTEXITCODE = 0\n\
         \x20   & $exe @($r.args) 2>&1 | ForEach-Object { [string]$_ }\n\
         \x20   [pscustomobject]@{ isothrumExit = $LASTEXITCODE }\n\
         \x20 } catch { [string]$_; [pscustomobject]@{ isothrumExit = 1 } }\n\
         }\n";

/// The broker's loop over the session's requests, in turn: each skipped when its `cancel-<n>`
/// came first, else run in a [`REQUEST_JOB`] ([`WATCH_REQUEST`]) watched for its `cancel-<n>` (or the session's
/// end), answered in `exit-<n>` unless it was cancelled. A job whose tree was ended is let go,
/// never stopped or removed: PowerShell waits a minute on a job whose process is gone, and the
/// next request would wait with it.
fn request_loop() -> String {
    let ready = ps_quote(READY);
    let [request, variables, cancel, output, exit] =
        [protocol::request, protocol::variables, protocol::cancel, protocol::output, protocol::exit].map(|name| name("$n"));
    format!(
        "function Read-Once($path) {{ $text = [System.IO.File]::ReadAllText($path); Remove-Item -LiteralPath $path -Force; $text | ConvertFrom-Json }}\n\
         [System.IO.File]::WriteAllText((Join-Path $d {ready}), '')\n\
         $n = 1\n\
         try {{\n\
         while (-not (Test-Over)) {{\n\
         \x20 $req = Join-Path $d \"{request}\"\n\
         \x20 $envFile = Join-Path $d \"{variables}\"\n\
         \x20 $cancel = Join-Path $d \"{cancel}\"\n\
         \x20 if (Test-Path -LiteralPath $cancel) {{ Remove-Quietly @($req, $envFile, $cancel); $n++; continue }}\n\
         \x20 if (-not (Test-Path -LiteralPath $req -PathType Leaf)) {{ Start-Sleep -Milliseconds 200; continue }}\n\
         \x20 $r = Read-Once $req\n\
         \x20 if ($r.session -ne $token -or $r.seq -ne $n) {{ continue }}\n\
         \x20 $e = $null\n\
         \x20 if (Test-Path -LiteralPath $envFile) {{ $e = Read-Once $envFile }}\n\
         \x20 $out = Join-Path $d \"{output}\"\n\
         \x20 $f = [System.IO.File]::Open($out, [System.IO.FileMode]::Append, [System.IO.FileAccess]::Write, [System.IO.FileShare]::ReadWrite)\n\
         \x20 $log = New-Object System.IO.StreamWriter -ArgumentList $f, (New-Object System.Text.UTF8Encoding -ArgumentList $false)\n\
         \x20 $log.AutoFlush = $true\n\
         \x20 $rc = 1\n\
         \x20 $cancelled = $false\n\
         \x20 $exe = Resolve-Program $r.program\n\
         \x20 if ($null -eq $e -or $e.session -ne $token -or $e.seq -ne $n) {{\n\
         \x20   $log.WriteLine('isothrum: refused: the request came without its own variables')\n\
         \x20 }} elseif ($null -eq $exe) {{\n\
         \x20   $log.WriteLine('isothrum: refused: the elevated session runs only Vagrant and Windows PowerShell, from where an administrator installed them')\n\
         \x20 }} elseif (@($e.env | Where-Object {{ -not (Test-Var $_.k) }}).Count -gt 0) {{\n\
         \x20   $log.WriteLine('isothrum: refused: the request sets a variable an elevated program is never given')\n\
         \x20 }} else {{\n\
         \x20   $path = (@((Split-Path -Parent $exe)) + $systemPath) -join ';'\n\
         {WATCH_REQUEST}\
         \x20 }}\n\
         \x20 $log.Close()\n\
         \x20 if ($cancelled) {{\n\
         \x20   Remove-Quietly @($out, $cancel)\n\
         \x20 }} else {{\n\
         \x20   $exit = Join-Path $d \"{exit}\"\n\
         \x20   [System.IO.File]::WriteAllText(\"$exit.tmp\", [string]$rc)\n\
         \x20   Move-Item -LiteralPath \"$exit.tmp\" -Destination $exit -Force\n\
         \x20 }}\n\
         \x20 $n++\n\
         }}\n\
         }} finally {{ Write-Ended 'the elevated broker stopped' }}\n"
    )
}

/// A request's job, watched: its output logged, its id and exit code read, and its tree ended
/// when its `cancel-<n>` (or the session's end) comes first.
const WATCH_REQUEST: &str = "\x20   $job = Start-Job -ScriptBlock $run -ArgumentList $r, $e, $exe, $path, $modulePath\n\
         \x20   $child = $null\n\
         \x20   while ($true) {\n\
         \x20     foreach ($o in @(Receive-Job -Job $job)) {\n\
         \x20       if ($o -is [string]) { $log.WriteLine($o) }\n\
         \x20       elseif ($null -ne $o.isothrumPid) { $child = $o.isothrumPid }\n\
         \x20       elseif ($null -ne $o.isothrumExit) { $rc = $o.isothrumExit }\n\
         \x20       else { $log.WriteLine([string]$o) }\n\
         \x20     }\n\
         \x20     if ($job.State -in 'Completed', 'Failed', 'Stopped') { if (-not $job.HasMoreData) { break }; continue }\n\
         \x20     if ((Test-Path -LiteralPath $cancel) -or (Test-Over)) {\n\
         \x20       $cancelled = $true\n\
         \x20       for ($i = 0; $null -eq $child -and $i -lt 50; $i++) { foreach ($o in @(Receive-Job -Job $job)) { if ($null -ne $o.isothrumPid) { $child = $o.isothrumPid } }; Start-Sleep -Milliseconds 100 }\n\
         \x20       if ($null -ne $child) { & taskkill.exe /T /F /PID $child | Out-Null; $job = $null } else { Stop-Job -Job $job }\n\
         \x20       break\n\
         \x20     }\n\
         \x20     Start-Sleep -Milliseconds 100\n\
         \x20   }\n\
         \x20   if ($null -ne $job) { Remove-Job -Job $job -Force }\n";

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

/// The command that starts `script` elevated: one UAC prompt, the broker hidden. A start Windows
/// refuses exits with the Win32 error's own code (the prompt declined is
/// [`ERROR_CANCELLED`]), found among the exception's causes, not with its message, which Windows
/// words in its display language.
pub(super) fn launcher(script: &str) -> String {
    let args = encoded(script).iter().map(|a| ps_quote(a)).collect::<Vec<_>>().join(",");
    format!(
        "try {{ Start-Process -FilePath powershell.exe -Verb RunAs -WindowStyle Hidden -ErrorAction Stop -ArgumentList {args} }} \
         catch {{ $e = $_.Exception; while ($e -and -not ($e -is [System.ComponentModel.Win32Exception])) {{ $e = $e.InnerException }}; \
         if ($e) {{ exit $e.NativeErrorCode }}; throw }}"
    )
}

/// The Win32 error a declined UAC prompt fails `Start-Process -Verb RunAs` with
/// (`ERROR_CANCELLED`), which [`launcher`] exits with.
pub(super) const ERROR_CANCELLED: i32 = 1223;

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
        let s = broker(Path::new("C:\\Users\\ada\\runs\\r\\elevated\\0f1e"), "0f1e2d", 4242);
        for part in [
            "$d = 'C:\\Users\\ada\\runs\\r\\elevated\\0f1e'",
            "$token = '0f1e2d'",
            "req-$n.json",
            "env-$n.json",
            "out-$n.log",
            "exit-$n",
            "'ready'",
            "cancel-$n",
            "'done'",
            "taskkill.exe /T /F /PID $main",
            "taskkill.exe /T /F /PID $child",
            "4242",
        ] {
            assert!(s.contains(part), "{part}: {s}");
        }
    }

    #[test]
    fn a_broker_that_stops_other_than_at_done_says_why_and_has_no_deadline() {
        let s = broker(Path::new("C:\\e\\t"), "t", 1);
        // The watcher writes why before it ends the tree; the broker's own loop, ended any other
        // way than `done`, says so too (`Write-Ended` writes nothing once `done` is there).
        let why = s.find("WriteAllText((Join-Path $d 'ended'), $why)").unwrap();
        assert!(why < s.find("taskkill.exe").unwrap(), "{s}");
        assert!(s.contains("} finally { Write-Ended 'the elevated broker stopped' }"), "{s}");
        assert!(
            s.contains("function Write-Ended($why) { if (-not (Test-Path -LiteralPath (Join-Path $d 'done')))"),
            "{s}"
        );
        // A session lasts as long as its operation: no clock of its own ends it.
        assert!(!s.contains("AddSeconds") && !s.contains("Get-Date"), "{s}");
    }

    #[test]
    fn the_broker_runs_only_this_sessions_next_request_and_keeps_no_values() {
        let s = broker(Path::new("C:\\e\\t"), "t", 1);
        // A request of another session, or out of turn, is deleted unrun.
        assert!(
            s.contains("$r = Read-Once $req\n  if ($r.session -ne $token -or $r.seq -ne $n) { continue }"),
            "{s}"
        );
        // The variables are read and deleted before the program starts, and refused unless theirs.
        let read = s.find("$e = Read-Once $envFile").unwrap();
        assert!(read < s.find("$job = Start-Job -ScriptBlock $run").unwrap(), "{s}");
        assert!(s.contains("if ($null -eq $e -or $e.session -ne $token -or $e.seq -ne $n)"));
        assert!(s.contains("Remove-Item -LiteralPath $path -Force"));
        // The variables are set in the request's own process, never the broker's (which only
        // clears the loaders' from its own).
        let job = s.find("$run = {").unwrap();
        for set in [
            "SetEnvironmentVariable('PATH', $path, 'Process')",
            "SetEnvironmentVariable($p.k, $p.v, 'Process')",
        ] {
            assert!(s.find(set).unwrap() > job, "{set}: {s}");
        }
        // The exit code appears whole.
        assert!(s.contains("Move-Item -LiteralPath \"$exit.tmp\" -Destination $exit -Force"));
    }

    /// Windows PowerShell loads the modules a script asks for (Vagrant's Hyper-V provider's
    /// `Hyper-V`) from `PSModulePath`, which holds the user's own Documents folder: a module the
    /// user's other processes put there would load as administrator. The session and each
    /// request's process look in the system's module folders only.
    #[test]
    fn the_elevated_programs_load_modules_from_the_systems_folders_only() {
        let s = broker(Path::new("C:\\e\\t"), "t", 1);
        let pinned = s.find("SetEnvironmentVariable('PSModulePath', $modulePath, 'Process')").unwrap();
        assert!(pinned < s.find("$run = {").unwrap(), "the broker's own first: {s}");
        assert!(
            s.contains(
                "$modulePath = @((Join-Path $sys 'WindowsPowerShell\\v1.0\\Modules'), (Join-Path $programFiles 'WindowsPowerShell\\Modules')) -join ';'"
            ),
            "{s}"
        );
        assert!(!s.contains("Documents") && !s.contains("$env:PSModulePath"), "never the user's: {s}");
        let job = s.find("$run = {").unwrap();
        assert!(
            s.find("SetEnvironmentVariable('PSModulePath', $modules, 'Process')").unwrap() > job,
            "the request's too: {s}"
        );
        assert!(s.contains("-ArgumentList $r, $e, $exe, $path, $modulePath"), "{s}");
    }

    #[test]
    fn a_cancel_ends_its_request_only_and_the_broker_takes_the_next() {
        let s = broker(Path::new("C:\\e\\t"), "t", 1);
        // A request cancelled before its turn is skipped, its files deleted unrun.
        assert!(
            s.contains("if (Test-Path -LiteralPath $cancel) { Remove-Quietly @($req, $envFile, $cancel); $n++; continue }"),
            "{s}"
        );
        // One in flight: its own tree ended (the id its job gave first), the broker kept.
        let kill = s.find("taskkill.exe /T /F /PID $child").unwrap();
        assert!(s.find("[pscustomobject]@{ isothrumPid = $PID }").unwrap() < s.find("& $exe").unwrap());
        assert!(s.find("$cancelled = $true").unwrap() < kill, "{s}");
        assert!(s.contains("if ($cancelled) {\n    Remove-Quietly @($out, $cancel)"), "{s}");
        // The watcher no longer ends the session on a request's cancel.
        assert!(!s.contains("'cancel'") && !s.contains("the runner cancelled it"), "{s}");
    }

    #[test]
    fn the_broker_runs_only_what_it_found_itself_never_what_a_request_names() {
        let s = broker(Path::new("C:\\e\\t"), "t", 1);
        // The request's program is a name looked up in the broker's own list, never run as given.
        assert!(s.contains("& $exe @($r.args)") && !s.contains("& $r.program"), "{s}");
        assert!(s.contains("$exe = Resolve-Program $r.program"), "{s}");
        for program in protocol::PROGRAMS {
            assert!(s.contains(&format!("'{program}' = @(")), "{program}: {s}");
        }
        // Found at an administrator's paths, owned by an administrator up to the drive's root.
        assert!(s.contains("[System.Environment]::GetFolderPath('ProgramFiles')") && s.contains("[System.Environment]::SystemDirectory"));
        for sid in TRUSTED_OWNERS {
            assert!(s.contains(sid), "{sid}");
        }
        assert!(s.contains("(Test-Path -LiteralPath $p -PathType Leaf) -and (Test-Owned $p)"), "{s}");
        // Its PATH is the broker's, set before the request's variables, which can't name it.
        let path = s.find("SetEnvironmentVariable('PATH', $path, 'Process')").unwrap();
        assert!(path < s.find("foreach ($p in $e.env)").unwrap(), "{s}");
        assert!(s.contains("Where-Object { -not (Test-Var $_.k) }"), "{s}");
        for refused in protocol::SYSTEM_VARS.iter().chain(&protocol::LOADER_PREFIXES) {
            assert!(s.contains(&format!("'{refused}'")), "{refused}");
        }
        // What the broker inherits that would choose the code its programs load is dropped
        // before any request runs: its jobs inherit the rest.
        let scrub = s
            .find("if (Test-Loader $v.Name) { [System.Environment]::SetEnvironmentVariable($v.Name, $null, 'Process') }")
            .unwrap();
        assert!(scrub < s.find("$job = Start-Job").unwrap(), "{s}");
        // A refusal runs nothing: the job starts only in the last branch.
        let refused = s.find("isothrum: refused: the elevated session runs only").unwrap();
        assert!(refused < s.find("$job = Start-Job").unwrap(), "{s}");
    }

    /// The broker is unverified on a real Windows beyond what it is now: any change to the
    /// PowerShell it generates must be deliberate. `broker.snapshot.ps1` is its text for fixed
    /// inputs, compared byte for byte; `ISOTHRUM_WRITE_BROKER=1` rewrites it after such a change.
    #[test]
    fn the_broker_is_its_snapshot_byte_for_byte() {
        let s = broker(Path::new("C:\\Users\\ada\\runs\\r\\elevated\\0f1e"), "0f1e2d", 4242);
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/process/elevated/broker.snapshot.ps1");
        if std::env::var_os("ISOTHRUM_WRITE_BROKER").is_some() {
            std::fs::write(&file, &s).unwrap();
        }
        let snapshot = std::fs::read_to_string(&file).unwrap_or_default();
        assert!(
            s == snapshot,
            "the broker's PowerShell changed: review it, then `ISOTHRUM_WRITE_BROKER=1 cargo test -p isothrum-runner broker_is_its_snapshot`"
        );
    }

    #[test]
    fn the_script_travels_encoded() {
        let args = encoded("echo 1");
        assert_eq!(args.last().map(String::as_str), Some("ZQBjAGgAbwAgADEA"));
        let start = launcher("echo 1");
        assert!(
            start.starts_with("try { Start-Process -FilePath powershell.exe -Verb RunAs -WindowStyle Hidden -ErrorAction Stop -ArgumentList '-NoProfile',"),
            "{start}"
        );
        assert!(
            start.contains("[System.ComponentModel.Win32Exception]") && start.contains("exit $e.NativeErrorCode"),
            "{start}"
        );
    }
}
