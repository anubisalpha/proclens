# Launches tools\spike.ps1 elevated (one UAC prompt) and waits for it to finish.
param(
    [int]$Seconds = 15,
    [Parameter(Mandatory)][string]$Out,
    [Parameter(Mandatory)][string]$MarkerDir
)
$script = Join-Path $PSScriptRoot 'spike.ps1'
$argList = "-NoProfile -ExecutionPolicy Bypass -File `"$script`" -Seconds $Seconds -Out `"$Out`" -MarkerDir `"$MarkerDir`""
Start-Process -FilePath powershell.exe -ArgumentList $argList -Verb RunAs -Wait
