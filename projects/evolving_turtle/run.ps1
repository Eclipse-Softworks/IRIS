# One command: a ROS 2 robot whose controller is replaced while it drives.
#
#     powershell -ExecutionPolicy Bypass -File projects/evolving_turtle/run.ps1
#
# Starts turtlesim, runs `iris evolve-live` with turtle1 as the plant (over
# ROS 2), and stops turtlesim again. The evolution log and the hash-chained
# audit are printed as they happen; the script exits non-zero unless the
# run ends with the expected story: the allocating candidate refused, the
# unclamped one promoted then rolled back, the saturated one promoted, and
# the audit chain verified.
#
# Environment: ROS2_DIR (default C:\dev\ros2_humble\ros2-windows), IRIS
# (default: target\release or target\debug iris.exe in this repository).

$ErrorActionPreference = "Stop"
$project = $PSScriptRoot
$repo = (Resolve-Path (Join-Path $project "..\..")).Path
$ros = if ($env:ROS2_DIR) { $env:ROS2_DIR } else { "C:\dev\ros2_humble\ros2-windows" }
$iris = if ($env:IRIS) { $env:IRIS } else {
    @("target\release\iris.exe", "target\debug\iris.exe") | ForEach-Object { Join-Path $repo $_ } |
        Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (-not $iris) { $iris = "iris" }

# Both processes talk over loopback only (see cyclonedds_local.xml).
$env:RMW_IMPLEMENTATION = "rmw_cyclonedds_cpp"
$env:CYCLONEDDS_URI = "file://" + ((Join-Path $project "cyclonedds_local.xml") -replace '\\', '/')
$env:AMENT_PREFIX_PATH = $ros
$env:QT_QPA_PLATFORM_PLUGIN_PATH = Join-Path $ros "bin\platforms"
$env:PATH = (Join-Path $ros "bin") + ";" + $env:PATH

$turtlesim = Start-Process -FilePath (Join-Path $ros "lib\turtlesim\turtlesim_node.exe") -PassThru -WindowStyle Normal
try {
    Start-Sleep -Seconds 2
    if ($turtlesim.HasExited) { throw ("turtlesim exited at start (0x{0:X})" -f $turtlesim.ExitCode) }

    $audit = Join-Path $repo "target\evolving_turtle"
    New-Item -ItemType Directory -Force $audit | Out-Null
    Remove-Item -ErrorAction SilentlyContinue (Join-Path $audit "audit.jsonl"), (Join-Path $audit "audit.head.json")
    $hash = (Get-FileHash (Join-Path $project "constitution.txt") -Algorithm SHA256).Hash.ToLower()

    & $iris evolve-live `
        --plant (Join-Path $project "ros2_plant.iris") `
        --baseline (Join-Path $project "baseline.iris") `
        --candidate (Join-Path $project "candidates\with_history.iris") `
        --candidate (Join-Path $project "candidates\lqr_unclamped.iris") `
        --candidate (Join-Path $project "candidates\lqr_saturated.iris") `
        --cases (Join-Path $project "cases.json") `
        --constitution (Join-Path $project "constitution.txt") --constitution-sha256 $hash `
        --audit (Join-Path $audit "audit.jsonl") --audit-head (Join-Path $audit "audit.head.json") `
        --min-output=-2 --max-output 2 --setpoints 2,8,3,6 --hold 6 --dt 0.032 --offer-every 4 `
        --require-allocation-free 2>&1 | Tee-Object -Variable lines | Out-Host
    $code = $LASTEXITCODE
    $text = ($lines | Out-String)
    if ($code -ne 0) { throw "evolve-live failed (exit $code)" }
    $story = @(
        "rejected        with_history",
        "promoted        lqr_unclamped",
        "rolled back     generation 2",
        "promoted        lqr_saturated",
        "audit chain verified"
    )
    $missing = $story | Where-Object { -not $text.Contains($_) }
    if ($missing) { throw ("the run did not show: " + ($missing -join "; ")) }
    Write-Host ""
    Write-Host "evolving turtle: refused, promoted, rolled back, promoted, audit verified"
}
finally {
    if (-not $turtlesim.HasExited) { Stop-Process -Id $turtlesim.Id -Force }
}
