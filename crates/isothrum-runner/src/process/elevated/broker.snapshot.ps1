$ErrorActionPreference = 'Continue'
$ProgressPreference = 'SilentlyContinue'
try { [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding -ArgumentList $false } catch {}
$d = 'C:\Users\ada\runs\r\elevated\0f1e'
$token = '0f1e2d'
function Write-Ended($why) { if (-not (Test-Path -LiteralPath (Join-Path $d 'done'))) { try { [System.IO.File]::WriteAllText((Join-Path $d 'ended'), $why) } catch {} } }
function Remove-Quietly($paths) { foreach ($p in $paths) { Remove-Item -LiteralPath $p -Force -ErrorAction SilentlyContinue } }
function Test-Over { (-not (Test-Path -LiteralPath $d)) -or (Test-Path -LiteralPath (Join-Path $d 'done')) }
$sys = [System.Environment]::SystemDirectory
$programFiles = [System.Environment]::GetFolderPath('ProgramFiles')
$programs = @{
  'vagrant' = @((Join-Path $programFiles 'Vagrant\bin\vagrant.exe'), (Join-Path $env:SystemDrive 'HashiCorp\Vagrant\bin\vagrant.exe'))
  'powershell' = @((Join-Path $sys 'WindowsPowerShell\v1.0\powershell.exe'))
}
$systemPath = @($sys, (Join-Path $sys 'Wbem'), (Join-Path $sys 'WindowsPowerShell\v1.0'), (Join-Path $sys 'OpenSSH'), [System.Environment]::GetFolderPath('Windows'))
$modulePath = @((Join-Path $sys 'WindowsPowerShell\v1.0\Modules'), (Join-Path $programFiles 'WindowsPowerShell\Modules')) -join ';'
[System.Environment]::SetEnvironmentVariable('PSModulePath', $modulePath, 'Process')
$trustedOwners = @('S-1-5-32-544', 'S-1-5-18', 'S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464')
function Test-Owned($path) {
  $p = $path
  while ($p -and $p -ne [System.IO.Path]::GetPathRoot($p)) {
    try { $owner = (Get-Acl -LiteralPath $p).GetOwner([System.Security.Principal.SecurityIdentifier]).Value } catch { return $false }
    if ($trustedOwners -notcontains $owner) { return $false }
    $p = Split-Path -Parent $p
  }
  $true
}
function Resolve-Program($name) {
  if ($name -isnot [string] -or -not $programs.ContainsKey($name)) { return $null }
  foreach ($p in $programs[$name]) { if ((Test-Path -LiteralPath $p -PathType Leaf) -and (Test-Owned $p)) { return $p } }
  $null
}
$vagrantVars = @('VAGRANT_HOME', 'VAGRANT_CWD', 'VAGRANT_DOTFILE_PATH', 'VAGRANT_VAGRANTFILE', 'VAGRANT_DEFAULT_PROVIDER', 'VAGRANT_CHECKPOINT_DISABLE', 'VAGRANT_NO_COLOR', 'VAGRANT_LOG')
$systemVars = @('PATH', 'PATHEXT', 'COMSPEC', 'PSMODULEPATH', 'SYSTEMROOT', 'WINDIR', 'PROGRAMFILES')
$loaderPrefixes = @('GEM_', 'BUNDLE_', 'RUBY')
function Test-Loader($k) {
  $u = $k.ToUpperInvariant()
  if ($u.StartsWith('VAGRANT_')) { return $vagrantVars -notcontains $u }
  foreach ($p in $loaderPrefixes) { if ($u.StartsWith($p)) { return $true } }
  $false
}
function Test-Var($k) {
  if ($k -isnot [string] -or $k -eq '' -or $k.Contains('=')) { return $false }
  -not (($systemVars -contains $k.ToUpperInvariant()) -or (Test-Loader $k))
}
foreach ($v in @(Get-ChildItem Env:)) { if (Test-Loader $v.Name) { [System.Environment]::SetEnvironmentVariable($v.Name, $null, 'Process') } }
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
  param($r, $e, $exe, $path, $modules)
  [pscustomobject]@{ isothrumPid = $PID }
  try { [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding -ArgumentList $false } catch {}
  [System.Environment]::SetEnvironmentVariable('PATH', $path, 'Process')
  [System.Environment]::SetEnvironmentVariable('PSModulePath', $modules, 'Process')
  foreach ($p in $e.env) { [System.Environment]::SetEnvironmentVariable($p.k, $p.v, 'Process') }
  try {
    Set-Location -LiteralPath $r.cwd -ErrorAction Stop
    $global:LASTEXITCODE = 0
    & $exe @($r.args) 2>&1 | ForEach-Object { [string]$_ }
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
  $exe = Resolve-Program $r.program
  if ($null -eq $e -or $e.session -ne $token -or $e.seq -ne $n) {
    $log.WriteLine('isothrum: refused: the request came without its own variables')
  } elseif ($null -eq $exe) {
    $log.WriteLine('isothrum: refused: the elevated session runs only Vagrant and Windows PowerShell, from where an administrator installed them')
  } elseif (@($e.env | Where-Object { -not (Test-Var $_.k) }).Count -gt 0) {
    $log.WriteLine('isothrum: refused: the request sets a variable an elevated program is never given')
  } else {
    $path = (@((Split-Path -Parent $exe)) + $systemPath) -join ';'
    $job = Start-Job -ScriptBlock $run -ArgumentList $r, $e, $exe, $path, $modulePath
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
