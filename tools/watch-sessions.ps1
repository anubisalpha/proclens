# Elevated watcher: logs once a second whether a ProcLens kernel trace session exists.
# Kernel ETW sessions are invisible to unelevated queries, so this must run as administrator.
param(
    [int]$Seconds = 40,
    [Parameter(Mandatory)][string]$Out
)
$end = (Get-Date).AddSeconds($Seconds)
$lines = New-Object System.Collections.Generic.List[string]
while ((Get-Date) -lt $end) {
    $found = logman query -ets 2>&1 | Select-String -Pattern 'ProcLens'
    $lines.Add(("{0:HH:mm:ss}  {1}" -f (Get-Date), $(if ($found) { 'SESSION PRESENT: ' + (($found | ForEach-Object { $_.Line.Trim() }) -join '; ') } else { 'no session' })))
    Start-Sleep -Seconds 1
}
$lines | Out-File $Out -Encoding ascii
