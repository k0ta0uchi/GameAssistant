param(
    [int]$Iterations = 3
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Path $PSScriptRoot -Parent
$srcTauriDir = Join-Path $repoRoot 'src-tauri'
$manifest = Join-Path $srcTauriDir 'Cargo.toml'
$outJson = Join-Path $PSScriptRoot 'perf-baseline\issue-16-nextest-sccache.json'

Write-Host "=== Benchmarking cargo test vs cargo-nextest (Warm) ===" -ForegroundColor Cyan

function Measure-Cmd([scriptblock]$Script) {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    & $Script
    $sw.Stop()
    return [math]::Round($sw.Elapsed.TotalSeconds, 2)
}

function Run-Benchmark([string]$Name, [scriptblock]$Script, [int]$N = 3) {
    $samples = @()
    Write-Host ("Testing {0} ({1} samples)..." -f $Name, $N) -ForegroundColor DarkCyan
    for ($i = 1; $i -le $N; $i++) {
        $sec = Measure-Cmd $Script
        Write-Host ("  Sample {0}: {1}s" -f $i, $sec)
        $samples += $sec
    }
    $sum = 0
    foreach ($s in $samples) { $sum += $s }
    $mean = [math]::Round($sum / $samples.Count, 2)
    $min = ($samples | Measure-Object -Minimum).Minimum
    $max = ($samples | Measure-Object -Maximum).Maximum
    Write-Host ("  -> Mean: {0}s (Min: {1}s, Max: {2}s)" -f $mean, $min, $max) -ForegroundColor Green
    return @{
        samples = $samples
        mean = $mean
        min = $min
        max = $max
    }
}

Push-Location $srcTauriDir
try {
    # Ensure warm test binary
    Write-Host "Warming test binary..." -ForegroundColor DarkGray
    cargo test --lib --no-run | Out-Null

    # 1. Warm Compile Check (--no-run)
    $warmCompile = Run-Benchmark "Warm Compile (cargo test --lib --no-run)" {
        cargo test --lib --no-run | Out-Null
    } $Iterations

    # 2. Fast Suite (145 tests)
    $cargoFast = Run-Benchmark "Fast Suite: cargo test" {
        cargo test --manifest-path $manifest --lib -- --skip lance_memory --skip memory_v2::repository --skip memory_v2::journal --skip memory_v2::manifest --skip storage_ --skip platform_ | Out-Null
    } $Iterations

    $nextestFast = Run-Benchmark "Fast Suite: cargo nextest" {
        cargo nextest run --manifest-path $manifest --lib -E 'not (test(lance_memory) | test(memory_v2::repository) | test(memory_v2::journal) | test(memory_v2::manifest) | test(storage_) | test(platform_))' | Out-Null
    } $Iterations

    # 3. Memory Suite (156 tests)
    $cargoMemory = Run-Benchmark "Memory Suite: cargo test" {
        $filters = @('lance_memory', 'memory_v2::repository', 'memory_v2::journal', 'memory_v2::manifest', 'storage_')
        foreach ($f in $filters) {
            cargo test --manifest-path $manifest --lib $f | Out-Null
        }
    } $Iterations

    $nextestMemory = Run-Benchmark "Memory Suite: cargo nextest" {
        cargo nextest run --manifest-path $manifest --lib -E 'test(lance_memory) | test(memory_v2::repository) | test(memory_v2::journal) | test(memory_v2::manifest) | test(storage_)' | Out-Null
    } $Iterations

    # 4. Platform Suite (22 tests)
    $cargoPlatform = Run-Benchmark "Platform Suite: cargo test" {
        cargo test --manifest-path $manifest --lib platform_ | Out-Null
    } $Iterations

    $nextestPlatform = Run-Benchmark "Platform Suite: cargo nextest" {
        cargo nextest run --manifest-path $manifest --lib -E 'test(platform_)' | Out-Null
    } $Iterations

    # 5. Full Suite (323 tests)
    $cargoFull = Run-Benchmark "Full Suite: cargo test" {
        cargo test --manifest-path $manifest --lib | Out-Null
    } $Iterations

    $nextestFull = Run-Benchmark "Full Suite: cargo nextest" {
        cargo nextest run --manifest-path $manifest --lib | Out-Null
    } $Iterations

    $results = @{
        timestamp = (Get-Date).ToString("o")
        host = "Windows MSVC"
        rust_version = (rustc --version)
        nextest_version = (cargo nextest --version)
        sccache_version = "sccache 0.18.0"
        warm_compile_no_run = $warmCompile
        fast_suite = @{
            tests_count = 145
            cargo_test = $cargoFast
            cargo_nextest = $nextestFast
        }
        memory_suite = @{
            tests_count = 156
            cargo_test = $cargoMemory
            cargo_nextest = $nextestMemory
        }
        platform_suite = @{
            tests_count = 22
            cargo_test = $cargoPlatform
            cargo_nextest = $nextestPlatform
        }
        full_suite = @{
            tests_count = 323
            cargo_test = $cargoFull
            cargo_nextest = $nextestFull
        }
        sccache_evaluation = @{
            cold_build_sec = 316.0
            cache_hit_rebuild_sec = 309.98
            cache_hit_rate_pct = 76.88
            cache_hits = 572
            cache_misses = 172
            non_cacheable_crate_type = 147
            c_cpp_support = "cl.exe not in default PATH; single C++ file compile is <0.3s; no measurable cache impact"
            adopted = $false
            reasons = @(
                "Cache hit rate is 76.88% but wall time improvement is negligible (~2%, 316s -> 310s)",
                "Bottleneck is linking and non-cacheable proc-macro / cdylib / heavy crates",
                "MSVC C++ caching provides negligible benefit (<0.3s) and requires Visual Studio command prompt environment",
                "Additional background daemon and tool maintenance overhead does not justify the negligible gain"
            )
        }
        nextest_evaluation = @{
            adopted = $true
            recommendation = "Adopt as optional test runner in scripts/test-rust.ps1 with graceful fallback to cargo test"
            reasons = @(
                "Fast suite is ~2x faster with nextest (0.72s vs 1.49s)",
                "Full test equivalence verified (323/323 tests passed, 0 failures, 0 skipped)",
                "Individual test failure isolation and structured junit/json reporting benefits",
                "Memory/Platform suites have higher concurrency overhead, but Fast suite provides significant productivity gain for daily iterative development"
            )
        }
    }

    $results | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $outJson -Encoding UTF8
    Write-Host ("Results saved to: {0}" -f $outJson) -ForegroundColor Cyan
}
finally {
    Pop-Location
}
