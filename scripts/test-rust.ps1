<#
.SYNOPSIS
    Hierarchical Rust test runner for GameAssistant (Issue #15).

.DESCRIPTION
    Runs Rust tests categorized into tiers:
      - Fast:     Pure unit tests, parsers, validators, serializers, state machines (no filesystem/DB/network/audio/process).
      - Memory:   LanceDB, journal, repository, storage integration, recovery/replay tests.
      - Platform: Windows API, process management, audio capture, devices, native resources, live network.
      - Full:     All tests without omission (equivalent to traditional `cargo test --lib`).

.PARAMETER Suite
    Target test tier to run. Options: Fast, Memory, Platform, Full. Default: Full.

.PARAMETER List
    List matching tests without running them.

.PARAMETER CargoArgs
    Additional arguments passed to `cargo test` (e.g. --release, --nocapture).

.EXAMPLE
    scripts\test-rust.ps1 -Suite Fast
    scripts\test-rust.ps1 -Suite Memory
    scripts\test-rust.ps1 -Suite Platform
    scripts\test-rust.ps1 -Suite Full
#>
param(
    [ValidateSet('Fast', 'Memory', 'Platform', 'Full')]
    [string]$Suite = 'Full',
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

# Run from src-tauri so .cargo/config.toml (vendored PROTOC) is discovered
Push-Location $srcTauriDir
try {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    Write-Host ("=== [test-rust] Starting Suite: {0} ===" -f $Suite) -ForegroundColor Cyan

    if ($List) {
        $extraArgs = @('--', '--list')
    } else {
        $extraArgs = @()
    }

    switch ($Suite) {
        'Fast' {
            # Skip LanceDB, repository, journal, manifest, storage_ and platform_ tests
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
            # Run all platform_* tests (Windows locks, child processes, ASR/audio, TTS wav assets, Twitch live)
            $targetArgs = @('platform_')
            if ($List) {
                & cargo test --manifest-path $manifest --lib @targetArgs @CargoArgs -- --list
            } else {
                & cargo test --manifest-path $manifest --lib @targetArgs @CargoArgs
            }
            if ($LASTEXITCODE -ne 0) { throw "Platform suite failed with exit code $LASTEXITCODE" }
        }

        'Memory' {
            # Run LanceDB, MemoryRepository, Journal, Manifest, and storage_* integration tests
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
            # Run entire test suite (all 323 tests)
            if ($List) {
                & cargo test --manifest-path $manifest --lib @CargoArgs -- --list
            } else {
                & cargo test --manifest-path $manifest --lib @CargoArgs
            }
            if ($LASTEXITCODE -ne 0) { throw "Full suite failed with exit code $LASTEXITCODE" }
        }
    }

    $sw.Stop()
    $elapsedSec = [math]::Round($sw.Elapsed.TotalSeconds, 2)
    Write-Host ("=== [test-rust] Suite '{0}' completed in {1}s (SUCCESS) ===" -f $Suite, $elapsedSec) -ForegroundColor Green
}
finally {
    Pop-Location
}
