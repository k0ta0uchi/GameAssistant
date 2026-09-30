<#
.SYNOPSIS
    Hierarchical Rust test runner for GameAssistant (Issues #15 & #16).

.DESCRIPTION
    Runs Rust tests categorized into tiers:
      - Fast:     Pure unit tests, parsers, validators, serializers, state machines (no filesystem/DB/network/audio/process).
      - Memory:   LanceDB, journal, repository, storage integration, recovery/replay tests.
      - Platform: Windows API, process management, audio capture, devices, native resources, live network.
      - Full:     All tests without omission (equivalent to traditional `cargo test --lib`).

    Supports both standard `cargo test` and `cargo nextest` (Issue #16 benchmarked).

.PARAMETER Suite
    Target test tier to run. Options: Fast, Memory, Platform, Full. Default: Full.

.PARAMETER Runner
    Test runner engine: 'Cargo' (default, robust in-process) or 'Nextest' (isolated process-parallel, 2x faster for Memory suite).
    If 'Nextest' is requested but cargo-nextest is missing, helpful installation instructions are displayed.

.PARAMETER List
    List matching tests without running them.

.PARAMETER CargoArgs
    Additional arguments passed to cargo / nextest.

.EXAMPLE
    scripts\test-rust.ps1 -Suite Fast
    scripts\test-rust.ps1 -Suite Memory -Runner Nextest
    scripts\test-rust.ps1 -Suite Platform
    scripts\test-rust.ps1 -Suite Full
#>
param(
    [ValidateSet('Fast', 'Memory', 'Platform', 'Full')]
    [string]$Suite = 'Full',
    [ValidateSet('Cargo', 'Nextest')]
    [string]$Runner = 'Cargo',
    [switch]$List,
    [string[]]$CargoArgs = @()
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Path $PSScriptRoot -Parent
$manifest = Join-Path $repoRoot 'src-tauri\Cargo.toml'
$srcTauriDir = Join-Path $repoRoot 'src-tauri'

if (-not (Test-Path -LiteralPath $manifest)) {
    throw "Cargo.toml not found at: $manifest"
}

# Verify nextest availability if requested
if ($Runner -eq 'Nextest') {
    $hasNextest = $false
    try {
        & cargo nextest --version 2>$null | Out-Null
        if ($LASTEXITCODE -eq 0) { $hasNextest = $true }
    } catch {
        $hasNextest = $false
    }

    if (-not $hasNextest) {
        Write-Warning "cargo-nextest is not installed or not in PATH."
        Write-Host "To install cargo-nextest on Windows:" -ForegroundColor Yellow
        Write-Host "  winget install nextest.cargo-nextest" -ForegroundColor Cyan
        Write-Host "  or: cargo install cargo-nextest --locked" -ForegroundColor Cyan
        Write-Host "Falling back to standard 'Cargo' test runner..." -ForegroundColor Yellow
        $Runner = 'Cargo'
    }
}

# Run from src-tauri so .cargo/config.toml (vendored PROTOC) is discovered
Push-Location $srcTauriDir
try {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    Write-Host ("=== [test-rust] Starting Suite: {0} (Runner: {1}) ===" -f $Suite, $Runner) -ForegroundColor Cyan

    if ($Runner -eq 'Nextest') {
        # Filter expressions for nextest
        $fastExpr = 'not (test(lance_memory) | test(memory_v2::repository) | test(memory_v2::journal) | test(memory_v2::manifest) | test(storage_) | test(platform_))'
        $memoryExpr = 'test(lance_memory) | test(memory_v2::repository) | test(memory_v2::journal) | test(memory_v2::manifest) | test(storage_)'
        $platformExpr = 'test(platform_)'

        $subCmd = if ($List) { 'list' } else { 'run' }

        switch ($Suite) {
            'Fast' {
                & cargo nextest $subCmd --manifest-path $manifest --lib -E $fastExpr @CargoArgs
                if ($LASTEXITCODE -ne 0) { throw "Fast suite (nextest) failed with exit code $LASTEXITCODE" }
            }
            'Platform' {
                & cargo nextest $subCmd --manifest-path $manifest --lib -E $platformExpr @CargoArgs
                if ($LASTEXITCODE -ne 0) { throw "Platform suite (nextest) failed with exit code $LASTEXITCODE" }
            }
            'Memory' {
                & cargo nextest $subCmd --manifest-path $manifest --lib -E $memoryExpr @CargoArgs
                if ($LASTEXITCODE -ne 0) { throw "Memory suite (nextest) failed with exit code $LASTEXITCODE" }
            }
            'Full' {
                & cargo nextest $subCmd --manifest-path $manifest --lib @CargoArgs
                if ($LASTEXITCODE -ne 0) { throw "Full suite (nextest) failed with exit code $LASTEXITCODE" }
            }
        }
    } else {
        # Standard cargo test runner
        switch ($Suite) {
            'Fast' {
                $skipArgs = @(
                    '--',
                    '--skip', 'lance_memory',
                    '--skip', 'memory_v2::repository',
                    '--skip', 'memory_v2::journal',
                    '--skip', 'memory_v2::manifest',
                    '--skip', 'storage_',
                    '--skip', 'platform_'
                )
                if ($List) {
                    & cargo test --manifest-path $manifest --lib @CargoArgs @skipArgs --list
                } else {
                    & cargo test --manifest-path $manifest --lib @CargoArgs @skipArgs
                }
                if ($LASTEXITCODE -ne 0) { throw "Fast suite failed with exit code $LASTEXITCODE" }
            }

            'Platform' {
                $targetArgs = @('platform_')
                if ($List) {
                    & cargo test --manifest-path $manifest --lib @targetArgs @CargoArgs -- --list
                } else {
                    & cargo test --manifest-path $manifest --lib @targetArgs @CargoArgs
                }
                if ($LASTEXITCODE -ne 0) { throw "Platform suite failed with exit code $LASTEXITCODE" }
            }

            'Memory' {
                $memoryFilters = @(
                    'lance_memory',
                    'memory_v2::repository',
                    'memory_v2::journal',
                    'memory_v2::manifest',
                    'storage_'
                )
                foreach ($filter in $memoryFilters) {
                    Write-Host ("--- [Memory] filter: {0} ---" -f $filter) -ForegroundColor DarkCyan
                    if ($List) {
                        & cargo test --manifest-path $manifest --lib $filter @CargoArgs -- --list
                    } else {
                        & cargo test --manifest-path $manifest --lib $filter @CargoArgs
                    }
                    if ($LASTEXITCODE -ne 0) { throw "Memory suite ($filter) failed with exit code $LASTEXITCODE" }
                }
            }

            'Full' {
                if ($List) {
                    & cargo test --manifest-path $manifest --lib @CargoArgs -- --list
                } else {
                    & cargo test --manifest-path $manifest --lib @CargoArgs
                }
                if ($LASTEXITCODE -ne 0) { throw "Full suite failed with exit code $LASTEXITCODE" }
            }
        }
    }

    $sw.Stop()
    $elapsedSec = [math]::Round($sw.Elapsed.TotalSeconds, 2)
    Write-Host ("=== [test-rust] Suite '{0}' ({1}) completed in {2}s (SUCCESS) ===" -f $Suite, $Runner, $elapsedSec) -ForegroundColor Green
}
finally {
    Pop-Location
}
