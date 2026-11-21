<#
.SYNOPSIS
  Starts a throwaway MyGraph server for recording the demo (demo/README.md).

.DESCRIPTION
  Everything goes into one scratch folder, outside the repository:
    server.toml      three keys: admin, alice (agent-write) and bob (agent-write), random each run
    data\            the graphs (only the empty "demo" graph this script creates)
    alice.mcp.json   MCP config for the first agent (actor "alice")
    bob.mcp.json     MCP config for the second agent (actor "bob")
    settings.json    Claude Code settings for the recording: no hooks, no custom status line
    viewer-key.txt   the key to paste into the graph viewer (alice's; read-only would do too)

  The server listens on 127.0.0.1 only. Nothing here touches your real server or its graphs.

.EXAMPLE
  ./demo/setup.ps1 -ModelDir C:\path\to\models\multilingual-e5-small
  ./demo/setup.ps1 -Reset        # stop the demo server and start again from an empty graph
  ./demo/setup.ps1 -Stop         # stop the demo server and delete the scratch folder
#>
param(
  [string]$Dir = (Join-Path ([IO.Path]::GetTempPath()) "mygraph-demo"),
  [int]$Port = 7499,
  [string]$Exe = "mygraph",
  [string]$ModelDir,
  [switch]$Reset,
  [switch]$Stop
)
$ErrorActionPreference = "Stop"
$base = "http://127.0.0.1:$Port"

function Stop-DemoServer {
  $pidFile = Join-Path $Dir "server.pid"
  if (Test-Path $pidFile) {
    $id = [int](Get-Content $pidFile)
    try { Stop-Process -Id $id -Force -ErrorAction Stop; Write-Host "Stopped the demo server ($id)." } catch {}
    Remove-Item $pidFile -Force
  }
}

function New-Key {
  $b = [byte[]]::new(24)
  [Security.Cryptography.RandomNumberGenerator]::Fill($b)
  "demo_" + ([Convert]::ToBase64String($b) -replace '[+/=]', '')
}

if ($Stop -or $Reset) {
  Stop-DemoServer
  if (Test-Path $Dir) { Start-Sleep -Milliseconds 500; Remove-Item $Dir -Recurse -Force }
  if ($Stop) { Write-Host "Removed $Dir."; return }
}
if (Test-Path (Join-Path $Dir "server.pid")) {
  throw "A demo server is already set up in $Dir. Use -Reset to start over or -Stop to remove it."
}

$exePath = (Get-Command $Exe -ErrorAction Stop).Source
New-Item -ItemType Directory -Force (Join-Path $Dir "data") | Out-Null

$admin = New-Key; $alice = New-Key; $bob = New-Key
$toml = @"
# Throwaway demo server (demo/setup.ps1). Delete this folder when done.
data_dir = "$((Join-Path $Dir 'data') -replace '\\', '/')"
listen = "127.0.0.1:$Port"
default_graph = "demo"
log_file = "$((Join-Path $Dir 'server.log') -replace '\\', '/')"

[[keys]]
key = "$admin"
actor = "admin"
role = "admin"

[[keys]]
key = "$alice"
actor = "alice"
role = "agent-write"

[[keys]]
key = "$bob"
actor = "bob"
role = "agent-write"
"@
if ($ModelDir) { $toml += "`n[search]`nmodel_dir = `"$($ModelDir -replace '\\', '/')`"`n" }
Set-Content (Join-Path $Dir "server.toml") $toml -Encoding utf8

foreach ($who in @(@{ n = "alice"; k = $alice }, @{ n = "bob"; k = $bob })) {
  $cfg = @{ mcpServers = @{ mygraph = @{ type = "http"; url = "$base/mcp"; headers = @{ Authorization = "Bearer $($who.k)" } } } }
  $cfg | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $Dir "$($who.n).mcp.json") -Encoding utf8
}
'{ "disableAllHooks": true }' | Set-Content (Join-Path $Dir "settings.json") -Encoding utf8
Set-Content (Join-Path $Dir "viewer-key.txt") $alice -Encoding ascii -NoNewline

$p = Start-Process $exePath -ArgumentList @("serve", "--config", (Join-Path $Dir "server.toml")) `
  -WindowStyle Hidden -PassThru
Set-Content (Join-Path $Dir "server.pid") $p.Id

$ready = $false
foreach ($i in 1..60) {
  try { Invoke-RestMethod "$base/health" -TimeoutSec 2 | Out-Null; $ready = $true; break } catch { Start-Sleep -Milliseconds 500 }
}
if (-not $ready) { throw "The demo server did not start; see $(Join-Path $Dir 'server.log')." }

Invoke-RestMethod -Method Post "$base/admin/graphs" -Headers @{ Authorization = "Bearer $admin" } `
  -ContentType "application/json" -Body '{"name": "demo"}' | Out-Null

$work = Join-Path $Dir "work"
New-Item -ItemType Directory -Force (Join-Path $work "alice"), (Join-Path $work "bob") | Out-Null
Write-Host @"

Demo server: $base (graph "demo", empty). Scratch folder: $Dir

Terminal 1 (alice):
  cd "$work\alice"; claude --strict-mcp-config --mcp-config "$Dir\alice.mcp.json" --settings "$Dir\settings.json"

Terminal 2 (bob):
  cd "$work\bob"; claude --strict-mcp-config --mcp-config "$Dir\bob.mcp.json" --settings "$Dir\settings.json"

Viewer: $base/admin/ui/graph?graph=demo  (paste the key from viewer-key.txt once)
"@
