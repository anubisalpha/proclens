# Compares two dumps made by dump-etw-schema.ps1 and prints what differs between the two Windows builds.
#
#   .\tools\diff-etw-schema.ps1 -A win10-19045.json -B server2025-26100.json
#   .\tools\diff-etw-schema.ps1 -A a.json -B b.json -Max 200     # show more lines per section
#
# The "ProcLens reads these" section comes first: those are the event families the collector decodes
# (FileIo, DiskIo, PerfInfo, Thread, TcpIp, UdpIp). A class that exists in A but not in B, or whose GUID,
# version, event type numbers or fields differ, is exactly what makes a build need its own profile.
# Keep this file ASCII: Windows PowerShell 5.1 reads BOM-less files as ANSI.
param(
    [Parameter(Mandatory)][string]$A,
    [Parameter(Mandatory)][string]$B,
    [int]$Max = 40
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Web.Extensions

function Load([string]$path) {
    # ConvertFrom-Json in Windows PowerShell 5.1 chokes on files this size; the serializer does not.
    $ser = New-Object System.Web.Script.Serialization.JavaScriptSerializer
    $ser.MaxJsonLength = [int]::MaxValue
    $ser.RecursionLimit = 200
    return $ser.DeserializeObject([IO.File]::ReadAllText($path))
}

$da = Load $A
$db = Load $B
$ma = $da['machine']; $mb = $db['machine']
"A: $($ma['product']) $($ma['displayVersion']) build $($ma['build']) $($ma['architecture'])   ($A)"
"B: $($mb['product']) $($mb['displayVersion']) build $($mb['build']) $($mb['architecture'])   ($B)"
"A: $($da['counts']['mofEtwClasses']) MOF event classes, $($da['counts']['manifestProviders']) manifest providers"
"B: $($db['counts']['mofEtwClasses']) MOF event classes, $($db['counts']['manifestProviders']) manifest providers"
""

# The families the collector decodes.
$Critical = '^(FileIo|DiskIo|PerfInfo|Thread|TcpIp|UdpIp|DPC|ISR|SampledProfile|ImageLoad|Process|Image)(_|$)'

function Q($q, [string]$k) {
    if ($q.ContainsKey($k)) { $v = $q[$k]; if ($v -is [array] -or $v -is [System.Collections.ArrayList]) { return (($v | ForEach-Object { "$_" }) -join ',') } else { return "$v" } }
    return ''
}

# name -> @{ head = "guid ver types names"; props = sorted "name:type:id:pointer" lines }
function Index($doc) {
    $h = @{}
    foreach ($c in $doc['mof']) {
        $q = $c['qualifiers']
        $props = @($c['properties'] | ForEach-Object {
            $pq = $_['qualifiers']
            "{0}:{1}:id={2}:ptr={3}" -f $_['name'], $_['type'], (Q $pq 'WmiDataId'), (Q $pq 'pointer')
        } | Sort-Object)
        $h[$c['name']] = @{
            parent = [string]$c['parent']
            head   = "guid={0} ver={1} types=[{2}] names=[{3}]" -f (Q $q 'Guid'), (Q $q 'EventVersion'), (Q $q 'EventType'), (Q $q 'EventTypeName')
            props  = $props
        }
    }
    return $h
}
$ia = Index $da
$ib = Index $db

function Section([string]$title, $lines) {
    $lines = @($lines | Where-Object { $_ })   # an empty pipeline would otherwise count as one null line
    "== $title ($($lines.Count))"
    if ($lines.Count -eq 0) { "   none" }
    else {
        $lines | Select-Object -First $Max | ForEach-Object { "   $_" }
        if ($lines.Count -gt $Max) { "   ... and $($lines.Count - $Max) more (raise -Max)" }
    }
    ""
}

$onlyA = @($ia.Keys | Where-Object { -not $ib.ContainsKey($_) } | Sort-Object)
$onlyB = @($ib.Keys | Where-Object { -not $ia.ContainsKey($_) } | Sort-Object)
$changed = New-Object System.Collections.ArrayList
foreach ($k in ($ia.Keys | Where-Object { $ib.ContainsKey($_) } | Sort-Object)) {
    $x = $ia[$k]; $y = $ib[$k]
    $notes = @()
    if ($x.head -ne $y.head) { $notes += "header: A {$($x.head)}  B {$($y.head)}" }
    if ($x.parent -ne $y.parent) { $notes += "parent: A $($x.parent)  B $($y.parent)" }
    $pa = @($x.props | Where-Object { $y.props -notcontains $_ })
    $pb = @($y.props | Where-Object { $x.props -notcontains $_ })
    if ($pa.Count -or $pb.Count) { $notes += "fields only in A [$($pa -join '; ')]  only in B [$($pb -join '; ')]" }
    if ($notes.Count) { [void]$changed.Add("$k  -  " + ($notes -join '  |  ')) }
}

Section "ProcLens reads these: classes only in A" ($onlyA | Where-Object { $_ -match $Critical } | ForEach-Object { "$_  ($($ia[$_].head))" })
Section "ProcLens reads these: classes only in B" ($onlyB | Where-Object { $_ -match $Critical } | ForEach-Object { "$_  ($($ib[$_].head))" })
Section "ProcLens reads these: classes that differ" ($changed | Where-Object { $_ -match $Critical })
Section "Other MOF classes only in A" ($onlyA | Where-Object { $_ -notmatch $Critical })
Section "Other MOF classes only in B" ($onlyB | Where-Object { $_ -notmatch $Critical })
Section "Other MOF classes that differ" ($changed | Where-Object { $_ -notmatch $Critical })

# manifest providers: present on one side only, and events whose id/version/template changed
# Providers can share a name, so the key is the name plus the provider id.
function ProviderIndex($doc) {
    $h = @{}
    foreach ($p in $doc['providers']) {
        $id = if ($p.ContainsKey('id')) { [string]$p['id'] } else { '' }
        $h["$($p['name']) [$id]"] = $p
    }
    return $h
}
$pa = ProviderIndex $da
$pb = ProviderIndex $db
Section "Manifest providers only in A" ($pa.Keys | Where-Object { -not $pb.ContainsKey($_) } | Sort-Object)
Section "Manifest providers only in B" ($pb.Keys | Where-Object { -not $pa.ContainsKey($_) } | Sort-Object)
$pd = foreach ($k in ($pa.Keys | Where-Object { $pb.ContainsKey($_) } | Sort-Object)) {
    if ($pa[$k].ContainsKey('error') -or $pb[$k].ContainsKey('error')) { continue }
    $ea = @{}; foreach ($e in $pa[$k]['events']) { $ea["$($e['id']).$($e['version'])"] = $e['template'] }
    $eb = @{}; foreach ($e in $pb[$k]['events']) { $eb["$($e['id']).$($e['version'])"] = $e['template'] }
    $gone = @($ea.Keys | Where-Object { -not $eb.ContainsKey($_) }).Count
    $new = @($eb.Keys | Where-Object { -not $ea.ContainsKey($_) }).Count
    $tpl = @($ea.Keys | Where-Object { $eb.ContainsKey($_) -and $ea[$_] -ne $eb[$_] }).Count
    if ($gone -or $new -or $tpl) { "$k  -  events only in A: $gone, only in B: $new, same id with a different field template: $tpl" }
}
Section "Manifest providers whose events differ" $pd
