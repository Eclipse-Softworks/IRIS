#Requires -Version 5.1
<#
.SYNOPSIS
    IRIS Language Installer for Windows (supports `irm ... | iex` and local bundles)
.DESCRIPTION
    Installs IRIS (iris.exe, iris_ui_host.dll, bundled LLVM toolchain + MinGW ucrt64
    sysroot, and optional VS Code extension) into a user-local directory (~/.iris) without
    requiring administrator privileges or manual toolchain setup.
.EXAMPLE
    irm https://raw.githubusercontent.com/Eclipse-Softworks/IRIS/main/installer/install.ps1 | iex
.EXAMPLE
    powershell -ExecutionPolicy Bypass -File installer\install.ps1 -InstallDir "$env:USERPROFILE\.iris"
#>

[CmdletBinding()]
param(
    [string]$Version = "",
    [string]$InstallDir = "",
    [switch]$Lightweight,
    [switch]$NoModifyPath,
    [switch]$NoVscode,
    [switch]$Yes
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$Repo = "Eclipse-Softworks/IRIS"
if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    $InstallDir = if ($env:IRIS_INSTALL_DIR) { $env:IRIS_INSTALL_DIR } else { Join-Path $env:USERPROFILE ".iris" }
}
$BinDir   = Join-Path $InstallDir "bin"
$ShareDir = Join-Path $InstallDir "share\iris"

function Write-Banner {
    Write-Host ""
    Write-Host "  IRIS Language Installer (Windows)" -ForegroundColor Cyan
    Write-Host "  Target prefix: $InstallDir" -ForegroundColor DarkGray
    Write-Host ""
}

function Write-Step { param([string]$m) Write-Host "  --> $m" -ForegroundColor Yellow }
function Write-Ok   { param([string]$m) Write-Host "  [OK] $m" -ForegroundColor Green }
function Write-Info { param([string]$m) Write-Host "  [i]  $m" -ForegroundColor Cyan }
function Write-Warn { param([string]$m) Write-Host "  [!]  $m" -ForegroundColor Yellow }

function Get-ScriptRootSafe {
    if (Test-Path variable:PSScriptRoot) {
        return $PSScriptRoot
    }
    return ""
}

function Get-TargetTriple {
    if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') {
        return "aarch64-pc-windows-msvc"
    }
    return "x86_64-pc-windows-msvc"
}

function Add-ToUserPath {
    param([string]$Dir)
    if ($NoModifyPath) {
        Write-Info "Skipped PATH modification (-NoModifyPath)."
        return
    }
    $regKey = 'HKCU:\Environment'
    $current = (Get-ItemProperty -Path $regKey -Name 'Path' -ErrorAction SilentlyContinue).Path
    if (-not $current) { $current = '' }
    $entries = @($current -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    if ($entries -contains $Dir) {
        Write-Info "User PATH already contains: $Dir"
    } else {
        $newPath = ($entries + $Dir) -join ';'
        [System.Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        Write-Ok "Added to user PATH: $Dir"
    }
    if (-not (($env:PATH -split ';') -contains $Dir)) {
        $env:PATH = "$Dir;$env:PATH"
    }
}

function Get-EditorCli {
    foreach ($c in @('code', 'code-insiders', 'cursor', 'codium')) {
        $cmd = Get-Command $c -ErrorAction SilentlyContinue
        if ($cmd) { return $cmd.Source }
    }
    foreach ($p in @(
        "$env:LOCALAPPDATA\Programs\Microsoft VS Code\bin\code.cmd",
        "$env:LOCALAPPDATA\Programs\Microsoft VS Code Insiders\bin\code-insiders.cmd",
        "$env:LOCALAPPDATA\Programs\cursor\resources\app\bin\cursor.cmd",
        'C:\Program Files\Microsoft VS Code\bin\code.cmd'
    )) {
        if (Test-Path $p) { return $p }
    }
    return $null
}

function Find-LocalBundleRoot {
    $sd = Get-ScriptRootSafe
    if ([string]::IsNullOrWhiteSpace($sd)) { return $null }
    if ((Test-Path (Join-Path $sd 'iris.exe')) -or (Test-Path (Join-Path $sd 'bin\iris.exe'))) {
        return $sd
    }
    $repoRelease = Join-Path $sd '..\target\release\iris.exe'
    if (Test-Path $repoRelease) {
        return (Resolve-Path (Join-Path $sd '..')).Path
    }
    return $null
}

function Install-FromRoot {
    param([string]$SrcRoot)

    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
    New-Item -ItemType Directory -Force -Path $ShareDir | Out-Null

    # 1. iris.exe
    $exeCandidates = @(
        (Join-Path $SrcRoot 'bin\iris.exe'),
        (Join-Path $SrcRoot 'iris.exe'),
        (Join-Path $SrcRoot 'target\release\iris.exe')
    )
    $irisSrc = $exeCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $irisSrc) {
        throw "Could not locate iris.exe under $SrcRoot"
    }
    Copy-Item $irisSrc (Join-Path $BinDir 'iris.exe') -Force
    Write-Ok "Installed iris.exe -> $BinDir\iris.exe"

    # 2. iris_ui_host.dll
    $uiCandidates = @(
        (Join-Path $SrcRoot 'bin\iris_ui_host.dll'),
        (Join-Path $SrcRoot 'iris_ui_host.dll'),
        (Join-Path $SrcRoot 'target\release\iris_ui_host.dll')
    )
    $uiSrc = $uiCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    if ($uiSrc) {
        Copy-Item $uiSrc (Join-Path $BinDir 'iris_ui_host.dll') -Force
        Write-Ok "Installed iris_ui_host.dll -> $BinDir\iris_ui_host.dll"
    }

    # 3. VC++ Runtime DLLs app-locally
    foreach ($dll in @('VCRUNTIME140.dll', 'VCRUNTIME140_1.dll', 'MSVCP140.dll')) {
        foreach ($d in @((Join-Path $SrcRoot 'bin'), $SrcRoot, 'C:\Windows\System32')) {
            $cand = Join-Path $d $dll
            if (Test-Path $cand) {
                Copy-Item $cand (Join-Path $BinDir $dll) -Force
                break
            }
        }
    }

    # 4. Bundled toolchain (LLVM/clang/lld + LLVM-C + ucrt64 sysroot)
    $tcSrc = Join-Path $SrcRoot 'toolchain'
    if ((-not $Lightweight) -and (Test-Path $tcSrc)) {
        $tcDst = Join-Path $InstallDir 'toolchain'
        if (Test-Path $tcDst) { Remove-Item $tcDst -Recurse -Force }
        Copy-Item $tcSrc $tcDst -Recurse -Force
        Write-Ok "Installed bundled toolchain -> $tcDst"
    }

    # 5. Metadata, logo icons & VS Code extension
    foreach ($meta in @('LLVM_VERSION', 'LICENSE', 'README.md')) {
        $mSrc = Join-Path $SrcRoot $meta
        if (-not (Test-Path $mSrc)) { $mSrc = Join-Path $SrcRoot "share\iris\$meta" }
        if (Test-Path $mSrc) {
            Copy-Item $mSrc (Join-Path $ShareDir $meta) -Force
        }
    }
    foreach ($pair in @(@('iris.png', 'icon.png'), @('iris.ico', 'icon.ico'))) {
        $dstName = $pair[0]
        $altName = $pair[1]
        $iconCand = @(
            (Join-Path $SrcRoot "share\iris\$dstName"),
            (Join-Path $SrcRoot $dstName),
            (Join-Path $SrcRoot $altName),
            (Join-Path $SrcRoot "installer\$altName")
        ) | Where-Object { Test-Path $_ } | Select-Object -First 1
        if ($iconCand) {
            Copy-Item $iconCand (Join-Path $ShareDir $dstName) -Force
        }
    }
    $vsix = Get-ChildItem -Path $SrcRoot -Filter 'iris-lang-*.vsix' -Recurse -ErrorAction SilentlyContinue |
            Sort-Object Name -Descending | Select-Object -First 1
    if ($vsix) {
        Copy-Item $vsix.FullName (Join-Path $ShareDir $vsix.Name) -Force
    }
}

Write-Banner

$localRoot = Find-LocalBundleRoot
if ($localRoot) {
    Write-Step "Installing from local bundle: $localRoot"
    Install-FromRoot -SrcRoot $localRoot
} else {
    $triple = Get-TargetTriple
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

    $headers = @{ 'User-Agent' = 'iris-installer'; 'Accept' = 'application/vnd.github+json' }
    if ([string]::IsNullOrWhiteSpace($Version)) {
        Write-Step "Resolving latest IRIS release..."
        $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -Headers $headers
        $tag = $rel.tag_name
    } else {
        $tag = $Version
    }

    $archive = if ($Lightweight) { "iris-$tag-$triple-lightweight.zip" } else { "iris-$tag-$triple.zip" }
    $dlUrl   = "https://github.com/$Repo/releases/download/$tag/$archive"
    $sumsUrl = "https://github.com/$Repo/releases/download/$tag/SHA256SUMS.txt"

    $tmpDir = Join-Path $env:TEMP ("iris-install-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force -Path $tmpDir | Out-Null
    try {
        $zipPath  = Join-Path $tmpDir $archive
        $sumsPath = Join-Path $tmpDir "SHA256SUMS.txt"
        $unpack   = Join-Path $tmpDir "unpack"

        Write-Step "Downloading $archive ($tag)..."
        Invoke-WebRequest -Uri $dlUrl -OutFile $zipPath -UseBasicParsing

        try {
            Invoke-WebRequest -Uri $sumsUrl -OutFile $sumsPath -UseBasicParsing
            $expected = $null
            foreach ($line in (Get-Content $sumsPath)) {
                if ($line -match "^([a-fA-F0-9]{64})\s+\*?$([regex]::Escape($archive))$") {
                    $expected = $Matches[1].ToLowerInvariant()
                    break
                }
            }
            if ($expected) {
                $actual = (Get-FileHash -Path $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
                if ($actual -ne $expected) {
                    throw "SHA-256 verification failed for $archive (expected $expected, got $actual)"
                }
                Write-Ok "Verified SHA-256 checksum for $archive"
            }
        } catch {
            if ($_.Exception.Message -like "*SHA-256 verification failed*") { throw }
            Write-Warn "Could not verify SHA256SUMS.txt ($_)"
        }

        Write-Step "Extracting archive..."
        Expand-Archive -Path $zipPath -DestinationPath $unpack -Force
        Install-FromRoot -SrcRoot $unpack

        if (-not $NoVscode) {
            $haveVsix = Get-ChildItem -Path $ShareDir -Filter 'iris-lang-*.vsix' -ErrorAction SilentlyContinue | Select-Object -First 1
            if (-not $haveVsix) {
                $verBare = $tag.TrimStart('v')
                $vsixUrl = "https://github.com/$Repo/releases/download/$tag/iris-lang-$verBare.vsix"
                try {
                    Invoke-WebRequest -Uri $vsixUrl -OutFile (Join-Path $ShareDir "iris-lang-$verBare.vsix") -UseBasicParsing
                } catch {}
            }
        }
    } finally {
        if (Test-Path $tmpDir) { Remove-Item $tmpDir -Recurse -Force -ErrorAction SilentlyContinue }
    }
}

Write-Step "Configuring user PATH..."
Add-ToUserPath -Dir $BinDir

if (-not $NoVscode) {
    $vsixInstalled = Get-ChildItem -Path $ShareDir -Filter 'iris-lang-*.vsix' -ErrorAction SilentlyContinue |
                     Sort-Object Name -Descending | Select-Object -First 1
    $editor = Get-EditorCli
    if ($editor -and $vsixInstalled) {
        Write-Step "Installing VS Code extension ($($vsixInstalled.Name))..."
        & $editor --install-extension $vsixInstalled.FullName --force 2>&1 | Out-Null
        if ($LASTEXITCODE -eq 0) {
            Write-Ok "Installed VS Code extension."
        } else {
            Write-Warn "Could not install VS Code extension automatically."
        }
    }
}

Write-Host ""
Write-Step "Generating PowerShell completions..."
$irisExe = Join-Path $BinDir 'iris.exe'
$compDir = Join-Path $ShareDir 'completions'
try {
    New-Item -ItemType Directory -Force -Path $compDir | Out-Null
    & $irisExe completions powershell | Set-Content -Path (Join-Path $compDir '_iris.ps1') -Encoding UTF8
    Write-Ok "Installed PowerShell completions -> $compDir\_iris.ps1"
} catch {
    Write-Warn "Could not generate PowerShell completions."
}

Write-Host ""
Write-Step "Running iris doctor..."
try {
    & $irisExe doctor
} catch {
    Write-Warn "iris doctor reported warnings; run 'iris doctor --fix' if needed."
}

Write-Host ""
Write-Host "  IRIS installed successfully into $InstallDir" -ForegroundColor Green
Write-Host "  Open a new terminal and run: iris --version" -ForegroundColor Cyan
Write-Host ""
