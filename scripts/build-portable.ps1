<#
.SYNOPSIS
    Builds the portable release executable for GameAssistant (Issue #25).

.DESCRIPTION
    Executes the standard portable release build:
      1. Validates and extracts the project version across package.json, Cargo.toml, and tauri.conf.json.
      2. Ensures build prerequisites (uv.exe, protoc) are prepared.
      3. Builds the frontend assets via `npm run build`.
      4. Builds the release Tauri executable via `npm run tauri -- build --no-bundle --ci`.
      5. Copies the resulting executable to `dist_release/GameAssistant-v<version>-portable.exe`.
      6. Verifies file presence, non-zero size, and computes SHA-256 hash.
      7. Emits GITHUB_OUTPUT parameters if running inside GitHub Actions.

.PARAMETER SkipFrontend
    Skip building frontend assets (assumes dist/ is already built and up-to-date).

.PARAMETER OutputDir
    Custom output directory for the portable executable (defaults to dist_release).

.EXAMPLE
    .\scripts\build-portable.ps1
    .\scripts\build-portable.ps1 -SkipFrontend
#>
param(
    [switch]$SkipFrontend = $false,
    [string]$OutputDir = ""
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Path $PSScriptRoot -Parent
$pkgJsonPath = Join-Path $repoRoot "package.json"
$cargoTomlPath = Join-Path $repoRoot "src-tauri\Cargo.toml"
$tauriConfPath = Join-Path $repoRoot "src-tauri\tauri.conf.json"

# 1. Version validation
Write-Host "[1/6] Validating project version..." -ForegroundColor Cyan

if (-not (Test-Path -LiteralPath $pkgJsonPath)) { throw "package.json not found: $pkgJsonPath" }
if (-not (Test-Path -LiteralPath $cargoTomlPath)) { throw "Cargo.toml not found: $cargoTomlPath" }
if (-not (Test-Path -LiteralPath $tauriConfPath)) { throw "tauri.conf.json not found: $tauriConfPath" }

$pkgJson = Get-Content -LiteralPath $pkgJsonPath -Raw | ConvertFrom-Json
$verPkg = [string]$pkgJson.version

$cargoToml = Get-Content -LiteralPath $cargoTomlPath -Raw
if ($cargoToml -match '(?m)^version\s*=\s*"([^"]+)"') {
    $verCargo = $matches[1]
} else {
    throw "Version field not found in $cargoTomlPath"
}

$tauriConf = Get-Content -LiteralPath $tauriConfPath -Raw | ConvertFrom-Json
$verTauri = [string]$tauriConf.version

if ($verPkg -ne $verCargo -or $verPkg -ne $verTauri) {
    throw "Version mismatch detected! package.json: '$verPkg', Cargo.toml: '$verCargo', tauri.conf.json: '$verTauri'"
}

$version = $verPkg
Write-Host "  Project version confirmed: v$version" -ForegroundColor Green

# 2. Prepare prerequisites
Write-Host "[2/6] Checking build prerequisites..." -ForegroundColor Cyan

# Ensure PROTOC environment variable is set if local tools/protoc exists
$localProtoc = Join-Path $repoRoot "tools\protoc\bin\protoc.exe"
$localProtocInclude = Join-Path $repoRoot "tools\protoc\include"
if (-not $env:PROTOC -and (Test-Path -LiteralPath $localProtoc)) {
    $env:PROTOC = $localProtoc
    $env:PROTOC_INCLUDE = $localProtocInclude
    Write-Host "  Set PROTOC=$($env:PROTOC)" -ForegroundColor Gray
}

# Ensure uv.exe is present in src-tauri/resources/
$uvDest = Join-Path $repoRoot "src-tauri\resources\uv.exe"
if (-not (Test-Path -LiteralPath $uvDest)) {
    $uvCmd = Get-Command "uv.exe" -ErrorAction SilentlyContinue
    if ($uvCmd) {
        Write-Host "  Copying uv.exe from PATH ($($uvCmd.Source)) to resources..." -ForegroundColor Gray
        $null = New-Item -ItemType Directory -Force -Path (Split-Path -Path $uvDest -Parent)
        Copy-Item -Path $uvCmd.Source -Destination $uvDest -Force
    } else {
        throw "Prerequisite missing: uv.exe not found at $uvDest and not in PATH."
    }
}

# 3. Frontend build
if (-not $SkipFrontend) {
    Write-Host "[3/6] Building frontend assets (npm run build)..." -ForegroundColor Cyan
    Push-Location $repoRoot
    try {
        & npm run build
        if ($LASTEXITCODE -ne 0) {
            throw "npm run build failed with exit code $LASTEXITCODE"
        }
    } finally {
        Pop-Location
    }
} else {
    Write-Host "[3/6] Skipping frontend build as requested (-SkipFrontend)" -ForegroundColor Yellow
    $distHtml = Join-Path $repoRoot "dist\index.html"
    if (-not (Test-Path -LiteralPath $distHtml)) {
        throw "Frontend dist not found at $distHtml! Cannot skip frontend build."
    }
}

# 4. Tauri / Rust release build
Write-Host "[4/6] Building Tauri release executable (--no-bundle)..." -ForegroundColor Cyan
Push-Location $repoRoot
try {
    & npx tauri build --no-bundle --ci
    if ($LASTEXITCODE -ne 0) {
        throw "Tauri release build failed with exit code $LASTEXITCODE"
    }
} finally {
    Pop-Location
}

# 5. Locate source binary and copy to dist_release
Write-Host "[5/6] Packaging portable executable..." -ForegroundColor Cyan

$sourceExe = Join-Path $repoRoot "src-tauri\target\release\gameassistant.exe"
if (-not (Test-Path -LiteralPath $sourceExe)) {
    throw "Expected release executable was not produced at: $sourceExe"
}

$targetDir = if ($OutputDir) {
    if (-not (Test-Path -LiteralPath $OutputDir)) {
        (New-Item -ItemType Directory -Force -Path $OutputDir).FullName
    } else {
        (Resolve-Path -LiteralPath $OutputDir).Path
    }
} else {
    $defaultDist = Join-Path $repoRoot "dist_release"
    if (-not (Test-Path -LiteralPath $defaultDist)) {
        (New-Item -ItemType Directory -Force -Path $defaultDist).FullName
    } else {
        $defaultDist
    }
}

$destFileName = "GameAssistant-v$version-portable.exe"
$destExe = Join-Path $targetDir $destFileName

Copy-Item -LiteralPath $sourceExe -Destination $destExe -Force
if (-not (Test-Path -LiteralPath $destExe)) {
    throw "Failed to copy executable to: $destExe"
}

# 6. Verify final artifact
Write-Host "[6/6] Verifying artifact..." -ForegroundColor Cyan
$artifact = Get-Item -LiteralPath $destExe
if ($artifact.Length -le 0) {
    throw "Generated artifact has invalid size: $($artifact.Length) bytes"
}

$hash = (Get-FileHash -LiteralPath $destExe -Algorithm SHA256).Hash
$sizeMb = [math]::Round($artifact.Length / 1MB, 2)

Write-Host "==========================================================" -ForegroundColor Green
Write-Host " Portable executable generated successfully!" -ForegroundColor Green
Write-Host "   Path:   $destExe" -ForegroundColor Green
Write-Host "   Size:   $sizeMb MB ($($artifact.Length) bytes)" -ForegroundColor Green
Write-Host "   SHA256: $hash" -ForegroundColor Green
Write-Host "==========================================================" -ForegroundColor Green

# Output for GitHub Actions if running in CI
if ($env:GITHUB_OUTPUT) {
    Add-Content -Path $env:GITHUB_OUTPUT -Value "version=$version"
    Add-Content -Path $env:GITHUB_OUTPUT -Value "artifact_name=GameAssistant-v$version-portable"
    Add-Content -Path $env:GITHUB_OUTPUT -Value "exe_name=$destFileName"
    Add-Content -Path $env:GITHUB_OUTPUT -Value "exe_path=$destExe"
    Add-Content -Path $env:GITHUB_OUTPUT -Value "exe_sha256=$hash"
}
