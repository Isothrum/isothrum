$ErrorActionPreference = 'Continue'
$ProgressPreference = 'SilentlyContinue'
try { [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding -ArgumentList $false } catch {}
$d = 'C:\Users\ada\runs\r\elevated\0f1e'
$token = '0f1e2d'
function Write-Ended($why) { if (-not (Test-Path -LiteralPath (Join-Path $d 'done'))) { try { [System.IO.File]::WriteAllText((Join-Path $d 'ended'), $why) } catch {} } }
function Remove-Quietly($paths) { foreach ($p in $paths) { Remove-Item -LiteralPath $p -Force -ErrorAction SilentlyContinue } }
function Test-Over { (-not (Test-Path -LiteralPath $d)) -or (Test-Path -LiteralPath (Join-Path $d 'done')) }
$null = Start-Job -ArgumentList $PID, 4242, $d -ScriptBlock {
  param($main, $runnerId, $d)
  $runner = Get-Process -Id $runnerId -ErrorAction SilentlyContinue
  while (-not (Test-Path -LiteralPath (Join-Path $d 'done'))) {
    $why = $null
    if (-not (Test-Path -LiteralPath $d)) { $why = 'its folder is gone' }
    elseif ($null -eq $runner -or $runner.HasExited) { $why = 'the runner is gone' }
    if ($null -ne $why) {
      try { [System.IO.File]::WriteAllText((Join-Path $d 'ended'), $why) } catch {}
      & taskkill.exe /T /F /PID $main | Out-Null; return
    }
    Start-Sleep -Milliseconds 500
  }
}
$run = {
  param($r, $e)
  [pscustomobject]@{ isothrumPid = $PID }
  try { [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding -ArgumentList $false } catch {}
  foreach ($p in $e.env) { [System.Environment]::SetEnvironmentVariable($p.k, $p.v, 'Process') }
  try {
    Set-Location -LiteralPath $r.cwd -ErrorAction Stop
    $global:LASTEXITCODE = 0
    & $r.program @($r.args) 2>&1 | ForEach-Object { [string]$_ }
    [pscustomobject]@{ isothrumExit = $LASTEXITCODE }
  } catch { [string]$_; [pscustomobject]@{ isothrumExit = 1 } }
}
function Read-Once($path) { $text = [System.IO.File]::ReadAllText($path); Remove-Item -LiteralPath $path -Force; $text | ConvertFrom-Json }
[System.IO.File]::WriteAllText((Join-Path $d 'ready'), '')
$n = 1
try {
while (-not (Test-Over)) {
  $req = Join-Path $d "req-$n.json"
  $envFile = Join-Path $d "env-$n.json"
  $cancel = Join-Path $d "cancel-$n"
  if (Test-Path -LiteralPath $cancel) { Remove-Quietly @($req, $envFile, $cancel); $n++; continue }
  if (-not (Test-Path -LiteralPath $req -PathType Leaf)) { Start-Sleep -Milliseconds 200; continue }
  $r = Read-Once $req
  if ($r.session -ne $token -or $r.seq -ne $n) { continue }
  $e = $null
  if (Test-Path -LiteralPath $envFile) { $e = Read-Once $envFile }
  $out = Join-Path $d "out-$n.log"
  $f = [System.IO.File]::Open($out, [System.IO.FileMode]::Append, [System.IO.FileAccess]::Write, [System.IO.FileShare]::ReadWrite)
  $log = New-Object System.IO.StreamWriter -ArgumentList $f, (New-Object System.Text.UTF8Encoding -ArgumentList $false)
  $log.AutoFlush = $true
  $rc = 1
  $cancelled = $false
  if ($null -eq $e -or $e.session -ne $token -or $e.seq -ne $n) {
    $log.WriteLine('isothrum: refused: the request came without its own variables')
  } else {
    $job = Start-Job -ScriptBlock $run -ArgumentList $r, $e
    $child = $null
    while ($true) {
      foreach ($o in @(Receive-Job -Job $job)) {
        if ($o -is [string]) { $log.WriteLine($o) }
        elseif ($null -ne $o.isothrumPid) { $child = $o.isothrumPid }
        elseif ($null -ne $o.isothrumExit) { $rc = $o.isothrumExit }
        else { $log.WriteLine([string]$o) }
      }
      if ($job.State -in 'Completed', 'Failed', 'Stopped') { if (-not $job.HasMoreData) { break }; continue }
      if ((Test-Path -LiteralPath $cancel) -or (Test-Over)) {
        $cancelled = $true
        for ($i = 0; $null -eq $child -and $i -lt 50; $i++) { foreach ($o in @(Receive-Job -Job $job)) { if ($null -ne $o.isothrumPid) { $child = $o.isothrumPid } }; Start-Sleep -Milliseconds 100 }
        if ($null -ne $child) { & taskkill.exe /T /F /PID $child | Out-Null; $job = $null } else { Stop-Job -Job $job }
        break
      }
      Start-Sleep -Milliseconds 100
    }
    if ($null -ne $job) { Remove-Job -Job $job -Force }
  }
  $log.Close()
  if ($cancelled) {
    Remove-Quietly @($out, $cancel)
  } else {
    $exit = Join-Path $d "exit-$n"
    [System.IO.File]::WriteAllText("$exit.tmp", [string]$rc)
    Move-Item -LiteralPath "$exit.tmp" -Destination $exit -Force
  }
  $n++
}
} finally { Write-Ended 'the elevated broker stopped' }
