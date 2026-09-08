$ErrorActionPreference = 'Stop'

# Regression test for the Windows resource-link failure:
# CVTRES CVT1100 / LINK LNK1123 while linking the debug Tauri binary.
# The command must build the real application binary, not only the library.
$cargoArgs = @(
    'build',
    '--manifest-path', 'src-tauri/Cargo.toml',
    '--bin', 'gameassistant',
    '--no-default-features'
)

& cargo @cargoArgs
if ($LASTEXITCODE -ne 0) {
    throw "Tauri Windows binary link regression: cargo exited with $LASTEXITCODE"
}

$binary = Join-Path $PSScriptRoot '..\src-tauri\target\debug\gameassistant.exe'
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw "Tauri Windows binary link regression: expected binary was not produced ($binary)"
}

Write-Output 'PASS: gameassistant debug binary links successfully.'

# The same manifest must be present in Cargo's lib test harnesses. This
# process-start check catches STATUS_ENTRYPOINT_NOT_FOUND before any test runs.
& cargo test --manifest-path src-tauri/Cargo.toml --lib -- --list | Out-Host
if ($LASTEXITCODE -ne 0) {
    throw "Tauri Windows test-harness manifest regression: cargo exited with $LASTEXITCODE"
}

Write-Output 'PASS: gameassistant lib test harness starts with Common Controls v6.'
