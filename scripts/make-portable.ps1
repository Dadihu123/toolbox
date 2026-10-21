# Assembles the portable distribution folder + zip.
#
#   pwsh -File scripts/make-portable.ps1
#
# Output: dist/Toolbox-portable-<version>/{toolbox.exe, tools/, data/README.txt}
#         dist/Toolbox-portable-<version>.zip
#
# The whole point of the portable build is that <exe>/data is the only place the
# app writes, so the shipped folder contains an empty data/ with a note.

param(
    [string]$Configuration = 'release',
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$srcTauri = Join-Path $repo 'src-tauri'
$dist = Join-Path $repo 'dist'

# version from Cargo.toml
$cargoToml = Get-Content (Join-Path $srcTauri 'Cargo.toml') -Raw
if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
    throw 'could not read version from Cargo.toml'
}
$version = $Matches[1]

if (-not $SkipBuild) {
    Write-Host "building ($Configuration)..." -ForegroundColor Cyan
    Push-Location $srcTauri
    try {
        if ($Configuration -eq 'release') { cargo build --release } else { cargo build }
        if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
    } finally {
        Pop-Location
    }
}

$targetDir = Join-Path $srcTauri "target\$Configuration"
$exe = Join-Path $targetDir 'toolbox.exe'
if (-not (Test-Path $exe)) { throw "missing $exe" }

$outName = "Toolbox-portable-$version"
$outDir = Join-Path $dist $outName
if (Test-Path $outDir) { Remove-Item $outDir -Recurse -Force }
New-Item -ItemType Directory -Path $outDir -Force | Out-Null

Copy-Item $exe (Join-Path $outDir 'toolbox.exe')
Copy-Item (Join-Path $repo 'tools') (Join-Path $outDir 'tools') -Recurse -Force
Copy-Item (Join-Path $repo 'README.md') (Join-Path $outDir 'README.md') -Force

$dataDir = Join-Path $outDir 'data'
New-Item -ItemType Directory -Path $dataDir -Force | Out-Null
@'
This folder holds every piece of state Toolbox writes.

  config.json     appearance, floating ball position, panel hotkey
  hotkeys.json    per-action hotkey overrides
  tools.json      installed tool records
  tool-data/      per-tool key/value storage
  logs/           daily logs, kept 7 days
  downloads/      files exported by tools
  webview2/       WebView2 profile (browser cache)

Delete this folder to fully reset the app.
'@ | Set-Content (Join-Path $dataDir 'README.txt') -Encoding UTF8

$zip = Join-Path $dist "$outName.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path (Join-Path $outDir '*') -DestinationPath $zip -CompressionLevel Optimal

$size = [math]::Round((Get-Item $zip).Length / 1MB, 2)
Write-Host ""
Write-Host "portable build ready" -ForegroundColor Green
Write-Host "  folder: $outDir"
Write-Host "  zip   : $zip ($size MB)"
