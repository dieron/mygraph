<#
.SYNOPSIS
  Installs or upgrades the local MyGraph server and starts it at logon.

.DESCRIPTION
  Copies a built mygraph.exe to the install folder (default %LOCALAPPDATA%\MyGraph\bin), writes
  server.toml on first install, registers autostart at logon (a hidden scheduled task, or a
  Startup-folder shortcut when task registration is not allowed), and (re)starts the server.
  Running it again deploys a new build; the data folder and server.toml are kept.

  With -BridgeOnly it only installs the program for MCP clients that reach a server elsewhere
  (`mygraph mcp --url http://<host>:7420`): no local server, no autostart. The bridge answers
  tools/list itself, so update it whenever the server gets new tools.

  Build first:  cargo build --release -p mygraph-server

.EXAMPLE
  .\deploy\windows\deploy.ps1
.EXAMPLE
  .\deploy\windows\deploy.ps1 -Uninstall      # stops it and removes autostart; data stays
.EXAMPLE
  .\deploy\windows\deploy.ps1 -BridgeOnly     # the server runs elsewhere (deploy/docker/deploy.sh)
#>
param(
    [string]$Source = (Join-Path $PSScriptRoot "..\..\target\release\mygraph.exe"),
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA "MyGraph"),
    [string]$DataDir = (Join-Path $env:USERPROFILE "mygraph-data"),
    [string]$Listen = "127.0.0.1:7420",
    [switch]$Uninstall,
    [switch]$BridgeOnly
)
$ErrorActionPreference = "Stop"
$TaskName = "MyGraph server"
$Bin = Join-Path $InstallDir "bin"
$Exe = Join-Path $Bin "mygraph.exe"
$Config = Join-Path $InstallDir "server.toml"
$Shortcut = Join-Path ([Environment]::GetFolderPath("Startup")) "MyGraph server.lnk"
# conhost --headless runs the console program without a window.
$Launcher = Join-Path $env:WINDIR "System32\conhost.exe"
$LaunchArgs = "--headless `"$Exe`" --config `"$Config`" serve"

function Stop-MyGraph {
    Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue | Stop-ScheduledTask -ErrorAction SilentlyContinue
    # Only the deployed server: same executable and a `serve` command line. MCP bridges
    # (`mygraph mcp`) started by Claude sessions keep running.
    Get-CimInstance Win32_Process -Filter "Name = 'mygraph.exe'" |
        Where-Object { $_.ExecutablePath -eq $Exe -and $_.CommandLine -match '\bserve\b' } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force; Write-Host "stopped server (pid $($_.ProcessId))" }
    Start-Sleep -Milliseconds 500
}

function Remove-Autostart {
    if (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue) {
        Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
    }
    if (Test-Path $Shortcut) { Remove-Item $Shortcut -Force }
}

if ($Uninstall) {
    Stop-MyGraph
    Remove-Autostart
    Write-Host "MyGraph stopped and removed from autostart. Kept: $InstallDir and $DataDir"
    exit 0
}

$Source = (Resolve-Path $Source).Path
New-Item -ItemType Directory -Force -Path $Bin, (Join-Path $InstallDir "logs"), $DataDir | Out-Null

# 1. Stop the running server, then swap the binary. A running exe cannot be overwritten but can
#    be renamed, so MCP bridges that still use the old one keep working.
Stop-MyGraph
if (Test-Path $Exe) {
    $old = Join-Path $Bin ("mygraph.old-{0:yyyyMMddHHmmss}.exe" -f (Get-Date))
    Move-Item $Exe $old
}
Copy-Item $Source $Exe
Get-ChildItem $Bin -Filter "mygraph.old-*.exe" | ForEach-Object {
    Remove-Item $_.FullName -Force -ErrorAction SilentlyContinue   # in use: removed next time
}
$version = & $Exe versions
Write-Host "installed $Exe  $version"
if ($BridgeOnly) {
    Remove-Autostart
    Write-Host "bridge only: no local server. Restart Claude apps to use the new program."
    exit 0
}

# 2. Configuration, written once.
if (-not (Test-Path $Config)) {
    $log = (Join-Path $InstallDir "logs\mygraph.log") -replace '\\', '/'
    @"
# MyGraph server configuration (spec/api/conventions.md). Edit and redeploy, or restart the task.
data_dir = "$($DataDir -replace '\\', '/')"
listen = "$Listen"
log_file = "$log"
preload = true
"@ | ForEach-Object { [IO.File]::WriteAllText($Config, $_, (New-Object Text.UTF8Encoding $false)) }
    Write-Host "wrote $Config"
}

# 3. Autostart at logon: a scheduled task for this user, else a Startup-folder shortcut.
Remove-Autostart
try {
    $action = New-ScheduledTaskAction -Execute $Launcher -Argument $LaunchArgs -WorkingDirectory $InstallDir
    $trigger = New-ScheduledTaskTrigger -AtLogOn -User "$env:USERDOMAIN\$env:USERNAME"
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
    Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger -Settings $settings `
        -Description "MyGraph graph database (local)" | Out-Null
    Write-Host "autostart: scheduled task '$TaskName'"
    Start-ScheduledTask -TaskName $TaskName
} catch {
    Write-Host "scheduled task not allowed ($($_.Exception.Message.Trim())); using a Startup shortcut"
    $shell = New-Object -ComObject WScript.Shell
    $lnk = $shell.CreateShortcut($Shortcut)
    $lnk.TargetPath = $Launcher
    $lnk.Arguments = $LaunchArgs
    $lnk.WorkingDirectory = $InstallDir
    $lnk.Description = "MyGraph graph database (local)"
    $lnk.Save()
    Write-Host "autostart: $Shortcut"
    Start-Process -FilePath $Launcher -ArgumentList $LaunchArgs -WorkingDirectory $InstallDir
}

# 4. Wait until it answers.
$url = "http://$Listen/health"
for ($i = 0; $i -lt 60; $i++) {
    try {
        $h = Invoke-RestMethod -Uri $url -TimeoutSec 2
        Write-Host "running: $url  (server $($h.server_version), spec $($h.spec_version))"
        Write-Host "log: $(Join-Path $InstallDir 'logs\mygraph.log')"
        exit 0
    } catch { Start-Sleep -Seconds 1 }
}
Write-Error "the server did not answer at $url within 60 s; see $(Join-Path $InstallDir 'logs\mygraph.log')"
