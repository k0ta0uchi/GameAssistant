# Reproducible cargo build/test performance baseline for Issue #13.
#
# Windows / MSVC assumed. Run from a developer prompt where cargo + the
# MSVC linker are on PATH (rustup + VS Build Tools), or wrap with the
# repo's vcvars64 step:
#   powershell -ExecutionPolicy Bypass -File scripts\measure-cargo-baseline.ps1 -Label main -Clean
#
# Phases (-Clean reorders them cold-first so the warm phases always measure
# the same freshly-warmed target produced by the cold pipeline):
#   -Clean:           1. cargo clean
#                     2. clean-build      cargo build --timings      (cold)
#                     3. test-compile-c   cargo test --lib --no-run   (cold)
#                     4. test-run-cold    cargo test --lib            (cold)
#                     5. warm-build       cargo build --timings       (warm)
#                     6. test-compile-w   cargo test --lib --no-run   (warm)
#                     7. test-run-warm    cargo test --lib            (warm)
#   without -Clean:   warm-build -> test-compile-w -> test-run-warm only
#                     (depends on whatever target state existed before the
#                     run; results are machine-state dependent)
#
# Definitions:
#   warm build = rebuild using the existing target directory (no-op to
#     partial). It is NOT a controlled-source-edit incremental rebuild
#     benchmark; measuring that would be a separate issue.
#
# cargo clean runs ONLY when -Clean is passed; the script always finishes
# with a fully warm target dir so the developer cache is preserved.
#
# Outputs (results dir: scripts/perf-baseline/<label>-<timestamp>/):
#   results.json                    per-phase wall-clock seconds + env snapshot
#   cargo-timing-<phase>.html       cargo --timings interactive report (local only)
#   top-crates-<phase>.txt          top N units by compile/build seconds
#   <phase>-cargo.log               full cargo output

param(
    # Label for the results directory (defaults to the git short sha).
    [string]$Label = "",
    # Include the cold measurements (runs `cargo clean`; the cache is re-warmed
    # by the cold phases themselves).
    [switch]$Clean,
    # Skip the warm phases (use together with -Clean for a cold-only run).
    [switch]$SkipWarm
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Path $PSScriptRoot -Parent
$manifest = Join-Path $repoRoot 'src-tauri\Cargo.toml'
$timingsDir = Join-Path $repoRoot 'src-tauri\target\cargo-timings'

# Cargo discovers .cargo/config.toml (which vendors PROTOC for the
# lance-encoding build script) by walking up from the CURRENT directory,
# not from --manifest-path; run every cargo invocation from src-tauri so
# the vendored protoc setup applies.
Push-Location (Split-Path -Parent $manifest)

if (-not $Label) {
    $Label = (git -C $repoRoot rev-parse --short HEAD)
    if ($LASTEXITCODE -ne 0) { throw 'git rev-parse failed; pass -Label explicitly.' }
}

$timestamp = (Get-Date).ToUniversalTime().ToString('yyyyMMdd-HHmmss')
$resultsDir = Join-Path $repoRoot "scripts\perf-baseline\$Label-$timestamp"
New-Item -ItemType Directory -Path $resultsDir -Force | Out-Null

function Format-Seconds {
    param([double]$Seconds)
    if ($Seconds -ge 60) {
        $minutes = [math]::Floor($Seconds / 60)
        $rest = $Seconds - ($minutes * 60)
        return ('{0}m {1:N1}s' -f $minutes, $rest)
    }
    return ('{0:N1}s' -f $Seconds)
}

function Invoke-MeasuredPhase {
    param(
        [string]$Name,
        [string[]]$CargoArgs,
        [switch]$ExpectTimings
    )
    Write-Host "=== [$Name] cargo $($CargoArgs -join ' ')"
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    # Windows PowerShell 5.1 wraps native stderr lines as ErrorRecords; with
    # the script-wide 'Stop' preference the first "Compiling ..." line would
    # throw. Relax it for the duration of the cargo invocation; the exit code
    # check below is still the failure gate.
    $ErrorActionPreference = 'Continue'
    & cargo @CargoArgs 2>&1 | Tee-Object -FilePath (Join-Path $resultsDir "$Name-cargo.log") | Out-Null
    $ErrorActionPreference = 'Stop'
    if ($LASTEXITCODE -ne 0) {
        throw "phase '$Name' failed: cargo exited with $LASTEXITCODE (see $Name-cargo.log)"
    }
    $sw.Stop()
    $seconds = [math]::Round($sw.Elapsed.TotalSeconds, 2)

    if ($ExpectTimings) {
        # cargo --timings writes to src-tauri/target/cargo-timings/ and
        # overwrites on every invocation; snapshot the html per phase.
        Copy-Item (Join-Path $timingsDir 'cargo-timing.html') `
            (Join-Path $resultsDir "cargo-timing-$Name.html") -Force
        $top = Get-TopCrates -HtmlPath (Join-Path $resultsDir "cargo-timing-$Name.html")
        $top | ForEach-Object {
            '{0,8:N1}s  {1}  {2}' -f $_.Seconds, $_.Mode, $_.Unit
        } | Set-Content -Path (Join-Path $resultsDir "top-crates-$Name.txt")
    }
    Write-Host ("=== [$Name] done: " + (Format-Seconds $seconds))
    return $seconds
}

function Get-TopCrates {
    param([string]$HtmlPath, [int]$Top = 15)
    if (-not (Test-Path -LiteralPath $HtmlPath -PathType Leaf)) { return @() }
    # This cargo's timings html embeds the data as a bare JS array:
    #   const UNIT_DATA = [ { "name": ..., "mode": ..., "duration": ... }, ... ];
    $html = Get-Content -LiteralPath $HtmlPath -Raw
    if ($html -notmatch '(?s)const UNIT_DATA = (\[.*?\]);') {
        Write-Warning "UNIT_DATA not found in $HtmlPath"
        return @()
    }
    $units = $Matches[1] | ConvertFrom-Json
    $units | ForEach-Object {
        $duration = $_.duration
        if ($null -ne $duration -and [double]$duration -gt 0) {
            [pscustomobject]@{
                Unit    = ('{0} {1}' -f $_.name, $_.version).Trim()
                Mode    = [string]$_.mode
                Seconds = [double]$duration
            }
        }
    } | Sort-Object Seconds -Descending | Select-Object -First $Top
}

if (-not (Test-Path $manifest)) { throw "manifest not found: $manifest" }

$envSnapshot = [ordered]@{
    label            = $Label
    git_sha          = (git -C $repoRoot rev-parse HEAD)
    timestamp_utc    = $timestamp
    cargo_version    = (cargo --version)
    rustc_version    = (rustc --version)
    cargo_build_jobs = $env:CARGO_BUILD_JOBS
    logical_cpus     = $env:NUMBER_OF_PROCESSORS
    os               = $env:OS
    phases           = [ordered]@{}
}
Write-Host ('cargo: ' + $envSnapshot.cargo_version + ' / rustc: ' + $envSnapshot.rustc_version)
Write-Host ('CARGO_BUILD_JOBS=' + $envSnapshot.cargo_build_jobs + ' CPUs=' + $envSnapshot.logical_cpus)

$results = @{}

if ($Clean) {
    # Cold-first ordering: the warm phases below then always measure the
    # same freshly-warmed target produced by this cold pipeline, keeping
    # before/after comparisons reproducible.
    Write-Host '=== [clean] cargo clean (cold measurement requested via -Clean)'
    & cargo clean --manifest-path $manifest
    if ($LASTEXITCODE -ne 0) { throw "cargo clean failed with $LASTEXITCODE" }

    $results['clean_build'] = Invoke-MeasuredPhase `
        -Name 'clean-build' `
        -CargoArgs @('build', '--manifest-path', $manifest, '--timings') `
        -ExpectTimings
    $results['test_compile_cold'] = Invoke-MeasuredPhase `
        -Name 'test-compile-cold' `
        -CargoArgs @('test', '--manifest-path', $manifest, '--lib', '--no-run', '--timings') `
        -ExpectTimings
    $results['test_run_cold'] = Invoke-MeasuredPhase `
        -Name 'test-run-cold' `
        -CargoArgs @('test', '--manifest-path', $manifest, '--lib')
}

if (-not $SkipWarm) {
    $results['warm_build'] = Invoke-MeasuredPhase `
        -Name 'warm-build' `
        -CargoArgs @('build', '--manifest-path', $manifest, '--timings') `
        -ExpectTimings
    $results['test_compile_warm'] = Invoke-MeasuredPhase `
        -Name 'test-compile-warm' `
        -CargoArgs @('test', '--manifest-path', $manifest, '--lib', '--no-run', '--timings') `
        -ExpectTimings
    $results['test_run_warm'] = Invoke-MeasuredPhase `
        -Name 'test-run-warm' `
        -CargoArgs @('test', '--manifest-path', $manifest, '--lib')
}

$envSnapshot.phases = $results
$jsonPath = Join-Path $resultsDir 'results.json'
$envSnapshot | ConvertTo-Json -Depth 4 | Set-Content -Path $jsonPath

Write-Host ''
Write-Host '==== baseline summary ===='
foreach ($entry in $results.GetEnumerator()) {
    Write-Host ('{0,-18} {1}' -f $entry.Key, (Format-Seconds ([double]$entry.Value)))
}
Write-Host ('results: ' + $resultsDir)
Write-Host ('timings html: cargo-timing-<phase>.html in the results dir (local only, not committed)')
