# Start menu shortcut target: runs `collector.exe discover` elevated and keeps the window open.
# Installed next to collector.exe under Program Files (writable only by administrators), so
# running it elevated does not widen what an ordinary program could tamper with.
# Keep this file ASCII: Windows PowerShell 5.1 reads BOM-less files as ANSI.
param([switch]$Elevated)

$ErrorActionPreference = 'Stop'
$collector = Join-Path $PSScriptRoot 'collector.exe'
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

if (-not $isAdmin) {
    if ($Elevated) {
        Write-Host 'Could not get administrator rights.'
        Read-Host 'Press Enter to close'
        exit 1
    }
    # Re-launch this script elevated (one UAC prompt); this unelevated window then closes.
    $argList = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', ('"{0}"' -f $PSCommandPath), '-Elevated')
    try {
        Start-Process -FilePath 'powershell.exe' -ArgumentList $argList -Verb RunAs
    } catch {
        Write-Host 'Administrator approval was declined, so discover was not run.'
        Read-Host 'Press Enter to close'
    }
    exit
}

$Host.UI.RawUI.WindowTitle = 'ProcLens: discover'
Write-Host 'ProcLens discover: derives this Windows build''s ETW event tables from live events'
Write-Host 'and compares them with the built-in profile. It prints event types and field names only.'
Write-Host 'If you are reporting a problem, copy this window''s output into a GitHub issue:'
Write-Host 'https://github.com/anubisalpha/proclens/issues'
Write-Host ''
if (-not (Test-Path $collector)) {
    Write-Host "collector.exe was not found next to this script ($collector)."
} else {
    & $collector discover
    Write-Host ''
    Write-Host "discover finished (exit code $LASTEXITCODE)."
}
Write-Host ''
Read-Host 'Press Enter to close'
