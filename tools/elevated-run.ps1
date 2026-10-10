# Runs an exe elevated (one UAC prompt) with its stdout+stderr captured to -Out, and waits.
# ShellExecute "runas" can't redirect output itself, so an elevated PowerShell does it.
param(
    [Parameter(Mandatory)][string]$Exe,
    [string]$ExeArgs = '',
    [Parameter(Mandatory)][string]$Out
)
$inner = "& '$Exe' $ExeArgs *> '$Out'; `$LASTEXITCODE | Out-File '$Out.exit'"
Start-Process -FilePath powershell.exe -ArgumentList @('-NoProfile', '-Command', $inner) -Verb RunAs -Wait -WindowStyle Hidden
