<#
.SYNOPSIS
    Sign Windows release binaries and installers using Azure Trusted Signing or Authenticode PFX.
.DESCRIPTION
    Skips cleanly with exit code 0 when signing environment variables are not configured,
    unless -RequireSigning is specified.
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string[]]$Files,
    [switch]$RequireSigning
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Find-SignTool {
    $fromPath = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($fromPath) { return $fromPath.Source }

    $kitsRoot = "C:\Program Files (x86)\Windows Kits\10\bin"
    if (Test-Path $kitsRoot) {
        $cand = Get-ChildItem -Path $kitsRoot -Filter signtool.exe -Recurse -ErrorAction SilentlyContinue |
                Where-Object { $_.FullName -match '\\x64\\signtool\.exe$' } |
                Sort-Object FullName -Descending |
                Select-Object -First 1
        if ($cand) { return $cand.FullName }
    }
    return $null
}

$targets = @()
foreach ($f in $Files) {
    if (Test-Path $f) {
        $targets += (Resolve-Path $f).Path
    }
}
if ($targets.Count -eq 0) {
    Write-Host "[skip] No target files exist to sign." -ForegroundColor Yellow
    exit 0
}

$hasAzure = (-not [string]::IsNullOrWhiteSpace($env:AZURE_TRUSTED_SIGNING_ENDPOINT)) -and
            (-not [string]::IsNullOrWhiteSpace($env:AZURE_TRUSTED_SIGNING_ACCOUNT)) -and
            (-not [string]::IsNullOrWhiteSpace($env:AZURE_TRUSTED_SIGNING_PROFILE))
$hasPfx   = (-not [string]::IsNullOrWhiteSpace($env:WINDOWS_CERT_PFX_BASE64)) -and
            (-not [string]::IsNullOrWhiteSpace($env:WINDOWS_CERT_PASSWORD))

if (-not $hasAzure -and -not $hasPfx) {
    if ($RequireSigning) {
        throw "Windows signing requested (-RequireSigning), but neither Azure Trusted Signing nor PFX secrets are configured."
    }
    Write-Host "[skip] Windows signing secrets not configured (skipping Authenticode signing)." -ForegroundColor Yellow
    exit 0
}

$signTool = Find-SignTool
if (-not $signTool) {
    throw "signtool.exe not found in PATH or Windows 10/11 SDK."
}

if ($hasAzure) {
    $dlib = $env:AZURE_TRUSTED_SIGNING_DLIB
    if ([string]::IsNullOrWhiteSpace($dlib)) {
        $dlib = Join-Path $env:TEMP "azure-signing\Azure.CodeSigning.Dlib.dll"
    }
    if (-not (Test-Path $dlib)) {
        throw "Azure.CodeSigning.Dlib.dll not found (set AZURE_TRUSTED_SIGNING_DLIB)."
    }
    $metaPath = Join-Path $env:TEMP ("iris-acs-" + [guid]::NewGuid().ToString("N") + ".json")
    try {
        @{
            Endpoint = $env:AZURE_TRUSTED_SIGNING_ENDPOINT
            CodeSigningAccountName = $env:AZURE_TRUSTED_SIGNING_ACCOUNT
            CertificateProfileName = $env:AZURE_TRUSTED_SIGNING_PROFILE
        } | ConvertTo-Json | Set-Content -Path $metaPath -Encoding UTF8

        foreach ($t in $targets) {
            Write-Host "Signing (Azure Trusted Signing): $t" -ForegroundColor Cyan
            & $signTool sign /v /fd SHA256 /tr "http://timestamp.acs.microsoft.com" /td SHA256 /dlib $dlib /dmdf $metaPath $t
            if ($LASTEXITCODE -ne 0) { throw "signtool Azure Trusted Signing failed on $t" }
        }
    } finally {
        Remove-Item $metaPath -Force -ErrorAction SilentlyContinue
    }
} else {
    $pfxPath = Join-Path $env:TEMP ("iris-cert-" + [guid]::NewGuid().ToString("N") + ".pfx")
    try {
        [System.IO.File]::WriteAllBytes($pfxPath, [System.Convert]::FromBase64String($env:WINDOWS_CERT_PFX_BASE64))
        foreach ($t in $targets) {
            Write-Host "Signing (Authenticode PFX): $t" -ForegroundColor Cyan
            & $signTool sign /fd SHA256 /tr "http://timestamp.digicert.com" /td SHA256 /f $pfxPath /p $env:WINDOWS_CERT_PASSWORD $t
            if ($LASTEXITCODE -ne 0) { throw "signtool PFX signing failed on $t" }
        }
    } finally {
        Remove-Item $pfxPath -Force -ErrorAction SilentlyContinue
    }
}

Write-Host "[ok] Signed $($targets.Count) Windows binary/installer file(s)." -ForegroundColor Green
