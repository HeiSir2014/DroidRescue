# Fetch the third-party runtime pieces embedded by build.rs into vendor/scrcpy.
# They are not committed: the official scrcpy release is downloaded, pinned by
# SHA256, and only the files the native client needs are extracted.
#
#   pwsh scripts/fetch-vendor.ps1
param(
    [string]$Version = "5.0.1",
    [string]$Sha256 = "B12A2C4EE8BE317422451FC7DCF8EE20A71B5EA7EF9AD73DDD825A25316227A5"
)
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root "vendor/scrcpy"
$files = @("adb.exe", "AdbWinApi.dll", "AdbWinUsbApi.dll", "scrcpy-server",
           "avcodec-63.dll", "avutil-61.dll", "swresample-7.dll", "LICENSE.txt")

$versionFile = Join-Path $dest "VERSION.txt"
if ((Test-Path $versionFile) -and ((Get-Content $versionFile -Raw).Trim() -eq "v$Version") -and
    -not ($files | Where-Object { -not (Test-Path (Join-Path $dest $_)) })) {
    Write-Host "vendor/scrcpy already at v$Version"
    exit 0
}

$work = Join-Path ([System.IO.Path]::GetTempPath()) "droidrescue-vendor-$Version"
New-Item -ItemType Directory -Force $work | Out-Null
$zip = Join-Path $work "scrcpy-win64-v$Version.zip"
if (-not (Test-Path $zip)) {
    $url = "https://github.com/Genymobile/scrcpy/releases/download/v$Version/scrcpy-win64-v$Version.zip"
    Write-Host "downloading $url"
    Invoke-WebRequest $url -OutFile $zip -UseBasicParsing
}
$actual = (Get-FileHash $zip -Algorithm SHA256).Hash
if ($actual -ne $Sha256) {
    Remove-Item $zip
    throw "SHA256 mismatch for scrcpy v${Version}: expected $Sha256, got $actual"
}

$unpacked = Join-Path $work "x"
if (Test-Path $unpacked) { Remove-Item $unpacked -Recurse -Force }
Expand-Archive $zip $unpacked
$src = Join-Path $unpacked "scrcpy-win64-v$Version"

New-Item -ItemType Directory -Force $dest | Out-Null
foreach ($f in $files) {
    Copy-Item (Join-Path $src $f) (Join-Path $dest $f) -Force
}
Set-Content -Path $versionFile -Value "v$Version" -NoNewline
Write-Host "vendor/scrcpy ready (v$Version)"
