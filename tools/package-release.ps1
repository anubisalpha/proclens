# Builds ProcLens from the current checkout and packages a release zip and a setup installer.
#   .\tools\package-release.ps1                 # writes dist\proclens-<version>-windows-x64.zip and
#                                               # ...-windows-x64-setup.exe, each with a .sha256
#   .\tools\package-release.ps1 -AllowDirty     # package an uncommitted tree (never for a published release)
#
# Builds go to .\target-release (not .\target), so a running dev copy can't block them.
# Steps: check the version numbers agree and the tree is clean, run every test, build the UI, the
# collector and the app, stage the files, generate the third-party notices, zip, and checksum.
param(
    [string]$OutDir = 'dist',
    [switch]$AllowDirty
)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location $root

# Build into a directory of its own: it never collides with a running dev copy of the app or
# collector (which lock their .exe files), and a release can't pick up stale dev artifacts.
$env:CARGO_TARGET_DIR = Join-Path $root 'target-release'

function Step($m) { Write-Host "==> $m" -ForegroundColor Cyan }
function Run([string]$exe, [string[]]$argv) {
    & $exe @argv
    if ($LASTEXITCODE -ne 0) { throw "$exe $($argv -join ' ') failed with exit code $LASTEXITCODE" }
}

# 1. One version everywhere.
Step 'Checking versions'
$version = (Get-Content app\src-tauri\tauri.conf.json -Raw | ConvertFrom-Json).version
$others = @{ 'app/package.json' = (Get-Content app\package.json -Raw | ConvertFrom-Json).version }
foreach ($c in 'proto', 'collector', 'app\src-tauri') {
    $others["$c/Cargo.toml"] = (Select-String -Path "$c\Cargo.toml" -Pattern '^version = "(.+)"$').Matches[0].Groups[1].Value
}
foreach ($k in $others.Keys) {
    if ($others[$k] -ne $version) { throw "version mismatch: tauri.conf.json says $version but $k says $($others[$k])" }
}
Write-Host "version $version"
# Inno Setup needs a numeric x.y.z.w for the file's version resource.
$numericVersion = ($version -replace '-.*$', '') + '.0'

# The installer compiler must exist before the (long) builds start.
$iscc = (Get-Command iscc -ErrorAction SilentlyContinue).Source
if (-not $iscc) { $iscc = 'C:\Program Files (x86)\Inno Setup 6\ISCC.exe' }
if (-not (Test-Path $iscc)) { throw 'Inno Setup 6 (ISCC.exe) not found: install it (e.g. choco install innosetup)' }

# 2. Release builds come from committed code.
$dirty = git status --porcelain
if ($dirty -and -not $AllowDirty) { throw "working tree has uncommitted changes (use -AllowDirty only for a trial run):`n$dirty" }

# 3. Tests, then builds. Each target is built explicitly: a combined -p/--examples build once left a stale binary.
Step 'Tests'
Run cargo @('test', '--workspace', '--release', '--locked')
Push-Location app
Run npm @('test')
Step 'Building the UI'
Run npm @('run', 'build')
Pop-Location
Step 'Building the collector'
Run cargo @('build', '--release', '-p', 'collector', '--locked')
Step 'Building the app'
Run cargo @('build', '--release', '-p', 'proclens-app', '--features', 'custom-protocol', '--locked')

# 4. Stage.
$name = "proclens-$version-windows-x64"
$stage = Join-Path $OutDir $name
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force $stage | Out-Null
Step "Staging $stage"
$bin = Join-Path $env:CARGO_TARGET_DIR 'release'
Copy-Item (Join-Path $bin 'proclens-app.exe'), (Join-Path $bin 'collector.exe'), LICENSE, README.md $stage
Copy-Item vendor\ferrisetw\License.md (Join-Path $stage 'ferrisetw-LICENSE.md')
$notes = "docs\releases\v$version.md"
if (Test-Path $notes) { Copy-Item $notes (Join-Path $stage 'RELEASE-NOTES.md') } else { Write-Warning "no $notes" }
Run node @('tools\third-party-notices.mjs', (Join-Path $stage 'THIRD-PARTY-NOTICES.txt'))

# 5. Zip and checksum. Entries are added one by one with forward slashes: .NET Framework's
# ZipFile.CreateFromDirectory (what Windows PowerShell 5.1 has) writes backslashes, which other
# unzip tools turn into file names containing a literal backslash. Checked again below.
Step 'Zipping'
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = Join-Path $OutDir "$name.zip"
if (Test-Path $zip) { [IO.File]::Delete((Resolve-Path $zip).Path) }
$zipPath = Join-Path (Resolve-Path $OutDir).Path "$name.zip"
$base = (Resolve-Path $stage).Path
$fs = [IO.File]::Open($zipPath, 'Create')
$za = New-Object IO.Compression.ZipArchive($fs, 'Create')
foreach ($file in Get-ChildItem $base -Recurse -File) {
    $rel = $file.FullName.Substring($base.Length + 1).Replace('\', '/')
    $entry = $za.CreateEntry("$name/$rel", 'Optimal')
    $dst = $entry.Open(); $src = [IO.File]::OpenRead($file.FullName)
    $src.CopyTo($dst); $src.Dispose(); $dst.Dispose()
}
$za.Dispose(); $fs.Dispose()
$check = [IO.Compression.ZipFile]::OpenRead($zipPath)
$bad = @($check.Entries | Where-Object { $_.FullName.Contains('\') })
$count = $check.Entries.Count
$check.Dispose()
if ($bad.Count -gt 0) { throw "zip has $($bad.Count) entries with backslashes in their names" }
Write-Host "zip entries: $count, all with forward slashes"
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
[IO.File]::WriteAllText((Join-Path (Resolve-Path $OutDir).Path "$name.zip.sha256"), "$hash  $name.zip`n", (New-Object Text.UTF8Encoding($false)))

# 6. Installer: the same staged files, wrapped by Inno Setup into one self-extracting setup .exe.
Step 'Building the installer'
$setupName = "proclens-$version-windows-x64-setup"
$outAbs = (Resolve-Path $OutDir).Path
$setup = Join-Path $outAbs "$setupName.exe"
if (Test-Path $setup) { [IO.File]::Delete($setup) }
Run $iscc @('/Qp', "/DAppVersion=$version", "/DNumericVersion=$numericVersion", "/DStageDir=$base", "/DOutDir=$outAbs", "/DRepoRoot=$root", 'installer\proclens.iss')
if (-not (Test-Path $setup)) { throw "Inno Setup finished but $setup does not exist" }
$setupHash = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLower()
[IO.File]::WriteAllText("$setup.sha256", "$setupHash  $setupName.exe`n", (New-Object Text.UTF8Encoding($false)))

Write-Host ''
Write-Host "Built $zip" -ForegroundColor Green
Write-Host "SHA-256 $hash"
Write-Host "Built $setup" -ForegroundColor Green
Write-Host "SHA-256 $setupHash"
Get-ChildItem $stage | ForEach-Object { '{0,10:N0} bytes  {1}' -f $_.Length, $_.Name }
