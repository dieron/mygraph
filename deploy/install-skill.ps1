<#
.SYNOPSIS
  Installs the mygraph agent skill for Claude Code, Codex, Cursor and GitHub Copilot, and builds
  a zip for the Claude desktop app / claude.ai.

.DESCRIPTION
  The skill (skills/mygraph: SKILL.md + references/) is in the Agent Skills format, which these
  tools share; only the folder they read differs:
    ~/.claude/skills/mygraph   Claude Code
    ~/.agents/skills/mygraph   Codex, Cursor, GitHub Copilot
  The zip (dist/mygraph-skill.zip) is for uploading in Claude's Settings -> Capabilities -> Skills.
  Running it again replaces the installed copies.

.EXAMPLE
  .\deploy\install-skill.ps1
#>
param(
    [string]$Source = (Join-Path $PSScriptRoot "..\skills\mygraph"),
    [string[]]$Targets = @(
        (Join-Path $env:USERPROFILE ".claude\skills\mygraph"),
        (Join-Path $env:USERPROFILE ".agents\skills\mygraph")
    ),
    [string]$Zip = (Join-Path $PSScriptRoot "..\dist\mygraph-skill.zip")
)
$ErrorActionPreference = "Stop"
$Source = (Resolve-Path $Source).Path
if (-not (Test-Path (Join-Path $Source "SKILL.md"))) { throw "no SKILL.md in $Source" }

foreach ($t in $Targets) {
    if (Test-Path $t) { Remove-Item $t -Recurse -Force }
    New-Item -ItemType Directory -Force -Path (Split-Path $t) | Out-Null
    Copy-Item $Source $t -Recurse
    Write-Host "installed $t"
}

# The zip holds the folder itself (mygraph/SKILL.md, mygraph/references/...).
New-Item -ItemType Directory -Force -Path (Split-Path $Zip) | Out-Null
if (Test-Path $Zip) { Remove-Item $Zip -Force }
Compress-Archive -Path $Source -DestinationPath $Zip
Write-Host "built $((Resolve-Path $Zip).Path) (upload in Claude: Settings -> Capabilities -> Skills)"
