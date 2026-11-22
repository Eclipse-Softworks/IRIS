# build_portable.ps1 — Build a portable .zip archive for IRIS on Windows
# ──────────────────────────────────────────────────────────────────────────
# Usage:
#   powershell -ExecutionPolicy Bypass -File installer\windows\build_portable.ps1
#
# Produces: installer\dist\IRIS-<version>-windows-x64-portable.zip
# A simple zip containing iris.exe + stdlib + examples (no installer UI).
# ──────────────────────────────────────────────────────────────────────────

param(
    [string]$Version = "1.0.0-rc3",
    [string]$Arch = "x64",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$Root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
if (-not $Root -or -not (Test-Path $Root)) {
    $Root = Split-Path $PSScriptRoot -Parent
    $Root = Split-Path $Root -Parent
}

Write-Host "IRIS Portable Archive Builder v${Version}" -ForegroundColor Cyan
Write-Host "Project root: $Root" -ForegroundColor Gray

# ── Build ─────────────────────────────────────────────────────────────────
if (-not $SkipBuild) {
    Write-Host "`n[1/4] Building release binaries..." -ForegroundColor Yellow
    Push-Location $Root
    & cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed" }
    & cargo build --release -p iris_ui_host
    if ($LASTEXITCODE -ne 0) { throw "cargo build --release -p iris_ui_host failed" }
    Pop-Location
    Write-Host "  Build complete." -ForegroundColor Green
} else {
    Write-Host "`n[1/4] Skipped build." -ForegroundColor DarkGray
}

# ── Stage ─────────────────────────────────────────────────────────────────
Write-Host "[2/4] Staging files..." -ForegroundColor Yellow

$StageDir = Join-Path $Root "installer\dist\portable-stage"
if (Test-Path $StageDir) { Remove-Item $StageDir -Recurse -Force }
New-Item -ItemType Directory -Force -Path $StageDir | Out-Null

$IrisExe = Join-Path $Root "target\release\iris.exe"
if (-not (Test-Path $IrisExe)) { throw "iris.exe not found at $IrisExe" }

$BinStage = Join-Path $StageDir "bin"
New-Item -ItemType Directory -Force -Path $BinStage | Out-Null

Copy-Item $IrisExe $BinStage -Force
Copy-Item $IrisExe $StageDir -Force
Write-Host "  bin/iris.exe" -ForegroundColor Green

$UiHostDll = Join-Path $Root "target\release\iris_ui_host.dll"
if (Test-Path $UiHostDll) {
    Copy-Item $UiHostDll $BinStage -Force
    Copy-Item $UiHostDll $StageDir -Force
    Write-Host "  bin/iris_ui_host.dll" -ForegroundColor Green
}

foreach ($dll in @('VCRUNTIME140.dll', 'VCRUNTIME140_1.dll', 'MSVCP140.dll')) {
    $vcrt = Join-Path "C:\Windows\System32" $dll
    if (Test-Path $vcrt) {
        Copy-Item $vcrt $BinStage -Force
        Copy-Item $vcrt $StageDir -Force
    }
}

# Examples
$ExamplesSrc = Join-Path $Root "examples"
if (Test-Path $ExamplesSrc) {
    Copy-Item $ExamplesSrc (Join-Path $StageDir "examples") -Recurse -Force
    Write-Host "  examples/" -ForegroundColor Green
}

# Bundled LLVM toolchain + MinGW sysroot
$ToolchainSrc = Join-Path $Root "toolchain"
if (Test-Path $ToolchainSrc) {
    Copy-Item $ToolchainSrc (Join-Path $StageDir "toolchain") -Recurse -Force
    $tcSize = [math]::Round(((Get-ChildItem (Join-Path $StageDir "toolchain") -Recurse -File | Measure-Object -Property Length -Sum).Sum) / 1048576, 1)
    Write-Host "  toolchain/ ($tcSize MB)" -ForegroundColor Green
}

# License + README + LLVM_VERSION + logo
Copy-Item (Join-Path $Root "LICENSE") $StageDir -Force
Copy-Item (Join-Path $Root "README.md") $StageDir -Force
Copy-Item (Join-Path $Root "LLVM_VERSION") $StageDir -Force
$IconIco = Join-Path $Root "installer\icon.ico"
if (Test-Path $IconIco) { Copy-Item $IconIco (Join-Path $StageDir "icon.ico") -Force }
$IconPng = Join-Path $Root "installer\icon.png"
if (-not (Test-Path $IconPng)) { $IconPng = Join-Path $Root "vscode-iris\icon.png" }
if (Test-Path $IconPng) { Copy-Item $IconPng (Join-Path $StageDir "icon.png") -Force }

# VSCode extension
$Vsix = Get-ChildItem (Join-Path $Root "vscode-iris\*.vsix") -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending | Select-Object -First 1
if ($Vsix) {
    Copy-Item $Vsix.FullName $StageDir -Force
    Write-Host "  $($Vsix.Name)" -ForegroundColor Green
}

# Install script
$InstallPs1 = Join-Path $PSScriptRoot "install.ps1"
if (-not (Test-Path $InstallPs1)) {
    # Fallback to old location
    $InstallPs1 = Join-Path $Root "installer\install.ps1"
}
if (Test-Path $InstallPs1) {
    Copy-Item $InstallPs1 $StageDir -Force
    Write-Host "  install.ps1" -ForegroundColor Green
}

$UninstallPs1 = Join-Path $PSScriptRoot "uninstall.ps1"
if (-not (Test-Path $UninstallPs1)) {
    $UninstallPs1 = Join-Path $Root "installer\uninstall.ps1"
}
if (Test-Path $UninstallPs1) {
    Copy-Item $UninstallPs1 $StageDir -Force
    Write-Host "  uninstall.ps1" -ForegroundColor Green
}

# ── Create ZIP ────────────────────────────────────────────────────────────
Write-Host "[3/4] Creating zip archive..." -ForegroundColor Yellow

$DistDir = Join-Path $Root "installer\dist"
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null

$ZipName = "IRIS-${Version}-windows-${Arch}-portable.zip"
$ZipPath = Join-Path $DistDir $ZipName

if (Test-Path $ZipPath) { Remove-Item $ZipPath -Force }
Compress-Archive -Path "$StageDir\*" -DestinationPath $ZipPath -CompressionLevel Optimal

# ── Cleanup ───────────────────────────────────────────────────────────────
Remove-Item $StageDir -Recurse -Force

# ── Done ──────────────────────────────────────────────────────────────────
Write-Host "[4/4] Done." -ForegroundColor Yellow
$zipSizeMB = [math]::Round((Get-Item $ZipPath).Length / 1048576, 1)
Write-Host "`nPortable archive ready:" -ForegroundColor Cyan
Write-Host "  $ZipPath ($zipSizeMB MB)" -ForegroundColor White
Write-Host ""
Write-Host "  Extract and run:" -ForegroundColor Gray
Write-Host "    Expand-Archive $ZipName -DestinationPath C:\iris" -ForegroundColor Cyan
Write-Host "    C:\iris\iris.exe --version" -ForegroundColor Cyan
Write-Host ""
