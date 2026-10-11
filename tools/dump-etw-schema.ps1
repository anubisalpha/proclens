# Dumps this Windows install's ETW event schemas to one JSON file, so builds can be compared.
#
#   .\tools\dump-etw-schema.ps1 -Out schema.json            # kernel (MOF) classes + the providers ProcLens cares about
#   .\tools\dump-etw-schema.ps1 -Out schema.json -AllProviders   # every manifest provider (large, slower)
#
# Two sources, because Windows keeps two kinds of event schema:
#  1. MOF classes in WMI (root\wmi). The classic kernel events ProcLens reads (FileIo, DiskIo, PerfInfo
#     DPC/ISR, TcpIp, UdpIp, Thread, ...) are described here: GUID, version, event type numbers, field names,
#     types and order (WmiDataId). If a family is missing here, tools that decode by schema (TDH, tracerpt,
#     and so ProcLens) cannot name or parse its events.
#  2. Manifest providers (Get-WinEvent -ListProvider): events with id, version, opcode, keywords and the
#     field template. Limited to the families in $Wanted unless -AllProviders is given.
#
# Compare two dumps with tools\diff-etw-schema.ps1. Read-only: nothing on the machine is changed.
# Keep this file ASCII: Windows PowerShell 5.1 reads BOM-less files as ANSI.
param(
    [Parameter(Mandatory)][string]$Out,
    [switch]$AllProviders
)
$ErrorActionPreference = 'Stop'
$started = Get-Date

$Wanted = 'Kernel|TCPIP|Winsock|NDIS|WLAN|WlanConn|Bluetooth|BTH|DNS-Client|Diagnostics-Perf|Processor-Power|Disk|Storage|NTFS|FilterManager'

function Qualifiers($items) {
    $h = [ordered]@{}
    foreach ($q in $items) { $h[$q.Name] = $q.Value }
    return $h
}

# --- 1. MOF event classes
$classes = Get-CimClass -Namespace root/wmi
$byName = @{}
foreach ($c in $classes) { $byName[$c.CimClassName] = $c }

function IsEtwClass($c) {
    $n = $c; $hops = 0
    while ($n -and $hops -lt 20) {
        if ($n.CimClassName -eq 'EventTrace') { return $true }
        if (-not $n.CimSuperClassName) { return $false }
        $n = $byName[$n.CimSuperClassName]; $hops++
    }
    return $false
}

$mof = foreach ($c in $classes | Where-Object { IsEtwClass $_ } | Sort-Object CimClassName) {
    [ordered]@{
        name       = $c.CimClassName
        parent     = $c.CimSuperClassName
        qualifiers = Qualifiers $c.CimClassQualifiers
        properties = @($c.CimClassProperties | ForEach-Object {
            [ordered]@{ name = $_.Name; type = [string]$_.CimType; qualifiers = Qualifiers $_.Qualifiers }
        })
    }
}

# --- 2. manifest providers
$names = [System.Diagnostics.Eventing.Reader.EventLogSession]::GlobalSession.GetProviderNames() | Sort-Object
$pick = if ($AllProviders) { $names } else { $names | Where-Object { $_ -match $Wanted } }
$providers = foreach ($n in $pick) {
    try {
        # Two providers can share a name (Ntfs does), so this may return more than one.
        foreach ($p in @(Get-WinEvent -ListProvider $n -ErrorAction Stop)) {
            [ordered]@{
                name   = [string]$p.Name
                id     = [string]$p.Id
                events = @($p.Events | ForEach-Object {
                    [ordered]@{
                        id = $_.Id; version = $_.Version; opcode = $_.Opcode.Value; opcodeName = $_.Opcode.Name
                        task = $_.Task.Value; level = $_.Level.Value; keywords = [string]$_.Keywords
                        template = $_.Template
                    }
                })
            }
        }
    } catch {
        [ordered]@{ name = $n; error = $_.Exception.Message }
    }
}

# --- which machine this is
$cv = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
$product = $cv.ProductName
if ([int]$cv.CurrentBuild -ge 22000 -and $product -like 'Windows 10*') { $product = $product -replace '^Windows 10', 'Windows 11' }
$doc = [ordered]@{
    schemaVersion = 1
    machine = [ordered]@{
        product = $product; edition = $cv.EditionID; displayVersion = $cv.DisplayVersion
        build = "$($cv.CurrentBuild).$($cv.UBR)"; architecture = $env:PROCESSOR_ARCHITECTURE
    }
    counts = [ordered]@{ wmiClassesTotal = $classes.Count; mofEtwClasses = @($mof).Count; manifestProviders = @($providers).Count }
    mof = @($mof)
    providers = @($providers)
}
$json = $doc | ConvertTo-Json -Depth 8 -Compress
[IO.File]::WriteAllText($Out, $json, (New-Object Text.UTF8Encoding $false))
$secs = [math]::Round(((Get-Date) - $started).TotalSeconds, 1)
"wrote $Out ($([math]::Round($json.Length / 1MB, 1)) MB) in $secs s: $($doc.counts.mofEtwClasses) MOF event classes of $($classes.Count) in root\wmi, $($doc.counts.manifestProviders) manifest providers"
