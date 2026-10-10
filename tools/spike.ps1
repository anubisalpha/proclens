# Runs the collector spike (mem + etw) and known disk workloads, writing results to $Out.
# Must run elevated; launch via run-elevated.ps1 or an admin prompt.
#
# Expected in the report (exact byte counts):
#   proclens_marker.bin  400 MB  (200 written + 200 read back)
#   proclens_preopen.bin  50 MB  (read; handle was opened BEFORE the trace started)
param(
    [int]$Seconds = 15,
    [Parameter(Mandatory)][string]$Out,
    [Parameter(Mandatory)][string]$MarkerDir
)
$ErrorActionPreference = 'Stop'
$exe = Join-Path $PSScriptRoot '..\target\release\collector.exe'

& $exe mem *> "$Out.mem.txt"

# Pre-opened file: created and held open before the trace exists, so no Create event is traced.
$buf = New-Object byte[] (1MB)
(New-Object Random).NextBytes($buf)
$preopen = Join-Path $MarkerDir 'proclens_preopen.bin'
$pf = New-Object IO.FileStream($preopen, 'Create', 'ReadWrite', 'None', 4096)
1..50 | ForEach-Object { $pf.Write($buf, 0, $buf.Length) }
$pf.Flush()

$p = Start-Process -FilePath $exe -ArgumentList @('etw', "$Seconds") -PassThru -WindowStyle Hidden `
    -RedirectStandardOutput "$Out.etw.txt" -RedirectStandardError "$Out.etw.err.txt"

# Independent reference for DPC/ISR time: Windows' own processor counters over (roughly) the same window.
$counterJob = Start-Job -ArgumentList $Seconds -ScriptBlock {
    param($n)
    $paths = '\Processor(_Total)\% DPC Time', '\Processor(_Total)\% Interrupt Time'
    $s = Get-Counter -Counter $paths -SampleInterval 1 -MaxSamples ($n - 2)
    foreach ($path in $paths) {
        $avg = ($s | ForEach-Object { ($_.CounterSamples | Where-Object { $_.Path -like "*$($path.Split('\')[-1])" }).CookedValue } | Measure-Object -Average).Average
        "$path avg=$avg"
    }
}

Start-Sleep -Seconds 4   # let the trace session come up before generating the workload

# Read the pre-opened file through the existing handle.
$pf.Position = 0
while ($pf.Read($buf, 0, $buf.Length) -gt 0) { }

# Marker: write 200 MB, then read it back through a second handle.
$big = New-Object byte[] (4MB)
(New-Object Random).NextBytes($big)
$marker = Join-Path $MarkerDir 'proclens_marker.bin'
$fs = New-Object IO.FileStream($marker, 'Create', 'Write', 'None', 4096, 'WriteThrough')
1..50 | ForEach-Object { $fs.Write($big, 0, $big.Length) }
$fs.Dispose()
$fs = New-Object IO.FileStream($marker, 'Open', 'Read', 'None', 4096, 'SequentialScan')
while ($fs.Read($big, 0, $big.Length) -gt 0) { }
$fs.Dispose()
[IO.File]::Delete($marker)

$p.WaitForExit()
$cpus = $env:NUMBER_OF_PROCESSORS
(Receive-Job $counterJob -Wait) + "logical CPUs=$cpus" | Out-File "$Out.counters.txt"
$pf.Dispose()
[IO.File]::Delete($preopen)
"done" | Out-File "$Out.done"
