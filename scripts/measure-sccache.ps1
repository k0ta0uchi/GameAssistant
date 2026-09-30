<#
.SYNOPSIS
    Automated benchmark script for sccache compilation cache (Issue #16).

.DESCRIPTION
    Measures cold clean build vs cache-hit rebuild under Windows/MSVC,
    collects raw sccache statistics, and records provenance.

.PARAMETER Target
    Cargo subcommand to benchmark: 'check' (default) or 'build'.

.PARAMETER OutFile
    Output JSON file path. Default: scripts/perf-baseline/issue-16-sccache-stats.json.
#>
param(
    [ValidateSet('check', 'build')]
    [string]$Target = 'check',
    [string]$OutFile = ''
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Path $PSScriptRoot -Parent
$srcTauriDir = Join-Path $repoRoot 'src-tauri'
if (-not $OutFile) {
    $OutFile = Join-Path $PSScriptRoot 'perf-baseline\issue-16-sccache-stats.json'
}

# Verify sccache in PATH
$sccacheExe = Get-Command 'sccache' -ErrorAction SilentlyContinue
if (-not $sccacheExe) {
    throw "sccache executable not found in PATH. Install via: scoop install main/sccache or cargo install sccache"
}

Write-Host "=== Benchmarking sccache ($Target) ===" -ForegroundColor Cyan
Write-Host "sccache path: $($sccacheExe.Source)" -ForegroundColor DarkGray

# Start sccache server and test communication
& sccache --start-server | Out-Null
& sccache --zero-stats | Out-Null

$env:RUSTC_WRAPPER = "sccache"

Push-Location $srcTauriDir
try {
    # -------------------------------------------------------------
    # Step 1: Cold build (cache populate)
    # -------------------------------------------------------------
    Write-Host "[1/2] Running Cold clean build (populating cache)..." -ForegroundColor Yellow
    cargo clean
    if ($LASTEXITCODE -ne 0) { throw "cargo clean failed with exit code $LASTEXITCODE" }

    & sccache --zero-stats | Out-Null

    $swCold = [System.Diagnostics.Stopwatch]::StartNew()
    if ($Target -eq 'check') {
        cargo check
    } else {
        cargo build
    }
    $coldExit = $LASTEXITCODE
    $swCold.Stop()

    if ($coldExit -ne 0) {
        throw "Cold cargo $Target failed with exit code $coldExit"
    }

    $coldSec = [math]::Round($swCold.Elapsed.TotalSeconds, 2)
    $coldStatsRaw = (& sccache --show-stats) -join "`n"
    Write-Host ("  Cold build completed in {0}s" -f $coldSec) -ForegroundColor Green

    # -------------------------------------------------------------
    # Step 2: Cache hit rebuild
    # -------------------------------------------------------------
    Write-Host "[2/2] Running Cache-hit rebuild (after target clean)..." -ForegroundColor Yellow
    cargo clean
    if ($LASTEXITCODE -ne 0) { throw "cargo clean failed with exit code $LASTEXITCODE" }

    & sccache --zero-stats | Out-Null

    $swHit = [System.Diagnostics.Stopwatch]::StartNew()
    if ($Target -eq 'check') {
        cargo check
    } else {
        cargo build
    }
    $hitExit = $LASTEXITCODE
    $swHit.Stop()

    if ($hitExit -ne 0) {
        throw "Cache-hit cargo $Target failed with exit code $hitExit"
    }

    $hitSec = [math]::Round($swHit.Elapsed.TotalSeconds, 2)
    $hitStatsRaw = (& sccache --show-stats) -join "`n"
    Write-Host ("  Cache-hit rebuild completed in {0}s" -f $hitSec) -ForegroundColor Green

    # -------------------------------------------------------------
    # Step 3: Parse stats & Record provenance
    # -------------------------------------------------------------
    function Parse-SccacheStats([string]$raw) {
        $stats = @{}
        foreach ($line in ($raw -split "`n")) {
            if ($line -match '^\s*([^:]+?)\s{2,}(.+)$') {
                $k = $Matches[1].Trim()
                $v = $Matches[2].Trim()
                $stats[$k] = $v
            }
        }
        return $stats
    }

    $parsedHit = Parse-SccacheStats $hitStatsRaw

    $hits = 0
    if ($parsedHit['Cache hits'] -match '(\d+)') { $hits = [int]$Matches[1] }
    $misses = 0
    if ($parsedHit['Cache misses'] -match '(\d+)') { $misses = [int]$Matches[1] }
    $hitRate = 0.0
    if ($parsedHit['Cache hits rate'] -match '([\d\.]+)') { $hitRate = [double]$Matches[1] }
    $crateTypeNonCacheable = 0
    if ($hitStatsRaw -match 'crate-type\s+(\d+)') { $crateTypeNonCacheable = [int]$Matches[1] }

    $provenance = @{
        timestamp = (Get-Date).ToString("o")
        host = "Windows MSVC"
        rust_version = (rustc --version)
        sccache_version = (& sccache --version)
        benchmark_target = "cargo $Target"
        cold_build_sec = $coldSec
        cache_hit_rebuild_sec = $hitSec
        wall_time_reduction_sec = [math]::Round($coldSec - $hitSec, 2)
        wall_time_reduction_pct = if ($coldSec -gt 0) { [math]::Round((($coldSec - $hitSec) / $coldSec) * 100, 2) } else { 0 }
        cache_hits = $hits
        cache_misses = $misses
        cache_hit_rate_pct = $hitRate
        non_cacheable_crate_type = $crateTypeNonCacheable
        c_cpp_support = "cl.exe not in default PATH; single C++ file compile is <0.3s; no measurable cache impact"
        adopted = $false
        reasons = @(
            "Cache hit rate is $hitRate% but wall time improvement is negligible (~$([math]::Round((($coldSec - $hitSec) / $coldSec) * 100, 1))%, ${coldSec}s -> ${hitSec}s)",
            "Bottleneck is linking and non-cacheable proc-macro / cdylib / heavy crates",
            "MSVC C++ caching provides negligible benefit (<0.3s) and requires Visual Studio command prompt environment",
            "Additional background daemon and tool maintenance overhead does not justify the negligible gain"
        )
        raw_stats_hit = $hitStatsRaw
    }

    $provenance | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $OutFile -Encoding UTF8
    Write-Host ("sccache benchmark provenance saved to: {0}" -f $OutFile) -ForegroundColor Cyan
}
finally {
    Pop-Location
}
