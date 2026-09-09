param([string]$Iris = 'iris', [string]$AuditDirectory = '')
$ErrorActionPreference = 'Stop'
if (-not $AuditDirectory) {
    $AuditDirectory = Join-Path ([IO.Path]::GetTempPath()) ('iris-evolution-' + [guid]::NewGuid().ToString('N'))
}
New-Item -ItemType Directory -Path $AuditDirectory -Force | Out-Null
$constitution = Join-Path $PSScriptRoot 'constitution.txt'
$digest = (Get-FileHash -Algorithm SHA256 -LiteralPath $constitution).Hash.ToLowerInvariant()
& $Iris evolve --baseline (Join-Path $PSScriptRoot 'baseline.iris') --candidate (Join-Path $PSScriptRoot 'candidate.iris') --cases (Join-Path $PSScriptRoot 'cases.json') --constitution $constitution --constitution-sha256 $digest --audit (Join-Path $AuditDirectory 'audit.jsonl') --audit-head (Join-Path $AuditDirectory 'audit.head.json') --min-output=-1000 --max-output 1000
if ($LASTEXITCODE -ne 0) { throw "Evolution failed: $LASTEXITCODE" }
Write-Output "Audit retained in $AuditDirectory"
