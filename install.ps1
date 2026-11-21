# Installs MyGraph from a GitHub release on Windows and starts it at logon.
#
#   irm https://raw.githubusercontent.com/dieron/mygraph/main/install.ps1 | iex
#
# What it does:
#   - downloads the Windows archive and the embedding model, and checks both against the
#     release's SHA256SUMS;
#   - installs into %LOCALAPPDATA%\MyGraph (bin\, data\, logs\, server.toml) and adds bin\ to your
#     PATH;
#   - writes server.toml on first install (listen 127.0.0.1:7420, no keys: only this machine can
#     reach it) and keeps it, and the data, on upgrades;
#   - starts the server at logon: a scheduled task for you, or a Startup shortcut where tasks
#     are not allowed. No administrator rights needed.
# Running it again upgrades. Nothing is sent anywhere except the downloads from github.com.
#
# Options, as environment variables set before running it (or parameters of a saved copy:
# .\install.ps1 -Version 0.3.0 -NoService):
#   MYGRAPH_VERSION      a release (default: the latest)
#   MYGRAPH_HOME         where to install (default: %LOCALAPPDATA%\MyGraph)
#   MYGRAPH_PORT         port on 127.0.0.1 (default: 7420; first install only)
#   MYGRAPH_NO_MODEL=1   skip the model (162 MB): words-only search
#   MYGRAPH_NO_SERVICE=1 install only; start it yourself with `mygraph --config ... serve`
#   -Uninstall           stop it and remove autostart; the folder, data and PATH entry stay

function Install-MyGraph {
    param(
        [string]$Version = $env:MYGRAPH_VERSION,
        [string]$InstallDir = $(if ($env:MYGRAPH_HOME) { $env:MYGRAPH_HOME } else { Join-Path $env:LOCALAPPDATA "MyGraph" }),
        [int]$Port = $(if ($env:MYGRAPH_PORT) { [int]$env:MYGRAPH_PORT } else { 7420 }),
        [switch]$NoModel = [bool]$env:MYGRAPH_NO_MODEL,
        [switch]$NoService = [bool]$env:MYGRAPH_NO_SERVICE,
        [switch]$Uninstall
    )
    $ErrorActionPreference = "Stop"
    $ProgressPreference = "SilentlyContinue"   # Invoke-WebRequest is far slower with it
    $Repo = "dieron/mygraph"
    $Model = "multilingual-e5-small"
    $Target = "x86_64-pc-windows-msvc"
    $TaskName = "MyGraph server"
    $Bin = Join-Path $InstallDir "bin"
    $Exe = Join-Path $Bin "mygraph.exe"
    $Config = Join-Path $InstallDir "server.toml"
    $Shortcut = Join-Path ([Environment]::GetFolderPath("Startup")) "MyGraph server.lnk"
    # conhost --headless runs the console program without a window.
    $Launcher = Join-Path $env:WINDIR "System32\conhost.exe"
    $LaunchArgs = "--headless `"$Exe`" --config `"$Config`" serve"

    function Stop-Server {
        Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue |
            Stop-ScheduledTask -ErrorAction SilentlyContinue
        # Only this install's server; MCP bridges (`mygraph mcp`) keep running.
        Get-CimInstance Win32_Process -Filter "Name = 'mygraph.exe'" |
            Where-Object { $_.ExecutablePath -eq $Exe -and $_.CommandLine -match '\bserve\b' } |
            ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
        Start-Sleep -Milliseconds 500
    }
    function Remove-Autostart {
        if (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue) {
            Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
        }
        if (Test-Path $Shortcut) { Remove-Item $Shortcut -Force }
    }

    if ($Uninstall) {
        Stop-Server
        Remove-Autostart
        Write-Host "MyGraph stopped and removed from autostart. Kept: $InstallDir (program, data, server.toml)."
        return
    }

    if (-not [Environment]::Is64BitOperatingSystem) { throw "MyGraph needs 64-bit Windows." }
    if (-not (Test-Path (Join-Path $env:WINDIR "System32\vcruntime140.dll"))) {
        Write-Warning "The Visual C++ runtime seems to be missing; install it if mygraph does not start: https://aka.ms/vs/17/release/vc_redist.x64.exe"
    }

    # --- download ------------------------------------------------------------------------------
    if (-not $Version) {
        $r = Invoke-WebRequest -Uri "https://github.com/$Repo/releases/latest" -UseBasicParsing -MaximumRedirection 5
        $final = $r.BaseResponse.ResponseUri, $r.BaseResponse.RequestMessage.RequestUri | Where-Object { $_ } | Select-Object -First 1
        $Version = ([string]$final) -replace '^.*/v', ''
        if ($Version -notmatch '^\d') { throw "cannot find the latest release ($final)" }
    }
    $Version = $Version -replace '^v', ''
    $Base = "https://github.com/$Repo/releases/download/v$Version"
    $Package = "mygraph-$Version-$Target"
    $Tmp = Join-Path ([IO.Path]::GetTempPath()) ("mygraph-install-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force -Path $Tmp | Out-Null
    try {
        Write-Host "MyGraph $Version for $Target"
        $sums = (Invoke-WebRequest -Uri "$Base/SHA256SUMS" -UseBasicParsing).Content
        if ($sums -is [byte[]]) { $sums = [Text.Encoding]::UTF8.GetString($sums) }
        function Get-Checked([string]$Name) {
            Write-Host "downloading $Name"
            $file = Join-Path $Tmp $Name
            Invoke-WebRequest -Uri "$Base/$Name" -OutFile $file -UseBasicParsing
            $line = ($sums -split "`n") | Where-Object { $_ -match "^([0-9a-f]{64})\s+\*?$([regex]::Escape($Name))\s*$" } | Select-Object -First 1
            if (-not $line) { throw "$Name is not listed in SHA256SUMS" }
            $want = ($line -split '\s+')[0]
            $got = (Get-FileHash -Algorithm SHA256 $file).Hash.ToLower()
            if ($got -ne $want) { throw "checksum mismatch for $Name" }
            $file
        }
        $zip = Get-Checked "$Package.zip"
        $modelZip = $null
        if (-not $NoModel -and -not (Test-Path (Join-Path $Bin "models\$Model\model.onnx"))) {
            $modelZip = Get-Checked "mygraph-model-$Model.zip"
        }

        # --- install ---------------------------------------------------------------------------
        New-Item -ItemType Directory -Force -Path $Bin, (Join-Path $InstallDir "data"), (Join-Path $InstallDir "logs") | Out-Null
        Expand-Archive -Path $zip -DestinationPath $Tmp -Force
        $unpacked = Join-Path $Tmp $Package
        Stop-Server
        # A running exe cannot be overwritten but can be renamed, so bridges keep working.
        if (Test-Path $Exe) { Move-Item $Exe (Join-Path $Bin ("mygraph.old-{0:yyyyMMddHHmmss}.exe" -f (Get-Date))) }
        Copy-Item (Join-Path $unpacked "mygraph.exe") $Exe
        Get-ChildItem $Bin -Filter "mygraph.old-*.exe" | ForEach-Object {
            Remove-Item $_.FullName -Force -ErrorAction SilentlyContinue   # in use: removed next time
        }
        $skills = Join-Path $InstallDir "skills"
        if (Test-Path $skills) { Remove-Item $skills -Recurse -Force }
        Copy-Item (Join-Path $unpacked "skills") $skills -Recurse
        Copy-Item (Join-Path $unpacked "*.md"), (Join-Path $unpacked "LICENSE-*") $InstallDir -Force
        if ($modelZip) {
            $m = Join-Path $Bin "models\$Model"
            if (Test-Path $m) { Remove-Item $m -Recurse -Force }
            Expand-Archive -Path $modelZip -DestinationPath $Bin -Force
        }
    } finally {
        Remove-Item $Tmp -Recurse -Force -ErrorAction SilentlyContinue
    }

    if (-not (Test-Path $Config)) {
        $fwd = { param($p) $p -replace '\\', '/' }
        $text = @"
# MyGraph server configuration, written by install.ps1; upgrades keep it.
# All settings: https://github.com/$Repo/blob/main/spec/api/conventions.md
# Without [[keys]] the server accepts only requests from this machine, as an admin.
listen = "127.0.0.1:$Port"
data_dir = "$(& $fwd (Join-Path $InstallDir 'data'))"
log_file = "$(& $fwd (Join-Path $InstallDir 'logs\mygraph.log'))"

[search]
model_dir = "$(& $fwd (Join-Path $Bin "models\$Model"))"
"@
        [IO.File]::WriteAllText($Config, $text, (New-Object Text.UTF8Encoding $false))
    }
    $listen = Select-String -Path $Config -Pattern '^listen\s*=\s*"([^"]+)"' | Select-Object -First 1
    $addr = if ($listen) { $listen.Matches[0].Groups[1].Value -replace '^0\.0\.0\.0:', '127.0.0.1:' } else { "127.0.0.1:$Port" }

    # PATH, for this user.
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if (-not (($userPath -split ';') -contains $Bin)) {
        [Environment]::SetEnvironmentVariable("Path", (("$userPath".TrimEnd(';'), $Bin) -join ';').TrimStart(';'), "User")
        Write-Host "added $Bin to your PATH (new terminals and apps see it)"
    }
    if (-not (($env:Path -split ';') -contains $Bin)) { $env:Path = "$env:Path;$Bin" }

    # --- autostart -----------------------------------------------------------------------------
    if ($NoService) {
        Write-Host "Installed MyGraph $Version. Start it with:"
        Write-Host "  mygraph --config `"$Config`" serve"
    } else {
        Remove-Autostart
        try {
            $action = New-ScheduledTaskAction -Execute $Launcher -Argument $LaunchArgs -WorkingDirectory $InstallDir
            $trigger = New-ScheduledTaskTrigger -AtLogOn -User "$env:USERDOMAIN\$env:USERNAME"
            $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
                -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
            Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger -Settings $settings `
                -Description "MyGraph server (local)" | Out-Null
            Start-ScheduledTask -TaskName $TaskName
            $how = "scheduled task '$TaskName'"
        } catch {
            $shell = New-Object -ComObject WScript.Shell
            $lnk = $shell.CreateShortcut($Shortcut)
            $lnk.TargetPath = $Launcher
            $lnk.Arguments = $LaunchArgs
            $lnk.WorkingDirectory = $InstallDir
            $lnk.Description = "MyGraph server (local)"
            $lnk.Save()
            Start-Process -FilePath $Launcher -ArgumentList $LaunchArgs -WorkingDirectory $InstallDir
            $how = "a Startup shortcut"
        }
        $ok = $false
        for ($i = 0; $i -lt 60 -and -not $ok; $i++) {
            try { Invoke-RestMethod -Uri "http://$addr/health" -TimeoutSec 2 | Out-Null; $ok = $true }
            catch { Start-Sleep -Seconds 1 }
        }
        if ($ok) {
            Write-Host "MyGraph $Version is running at http://$addr (started at logon by $how)."
        } else {
            Write-Warning "The server did not answer at http://$addr yet; see $(Join-Path $InstallDir 'logs\mygraph.log')"
        }
    }

    Write-Host @"

Connect an agent:
  Claude Code:  claude plugin marketplace add $Repo; claude plugin install mygraph@mygraph
     or:        claude mcp add --transport http -s user mygraph http://$addr/mcp
  Others:       https://github.com/$Repo#connecting-a-client
Then ask it to remember something. Program, data and settings: $InstallDir
"@
}

Install-MyGraph @args
