# QuantsMind — cross-compile build script for Windows PowerShell
# Usage: .\scripts\build-all.ps1 [-Target <triple>]
# Without -Target: builds for the current platform.
# With -Target: cross-compiles.

param(
    [string]$Target = "",
    [switch]$Test
)

$ErrorActionPreference = "Stop"

# Read version from workspace Cargo.toml
$version = (Select-String -Path "Cargo.toml" -Pattern '^version\s*=' | Select-Object -First 1).Line -replace '.*"(.+)".*','$1'
if (-not $version) { $version = "0.1.0" }

$OutDir = "dist"

Write-Host "=== QuantsMind build v$version ===" -ForegroundColor Cyan

function Build-Native {
    Write-Host "[1/3] Building release binaries (native)..." -ForegroundColor Green
    cargo build --release
    if (-not (Test-Path $OutDir)) { New-Item -ItemType Directory -Path $OutDir | Out-Null }

    $bins = @("qmind-server.exe", "qmind-cli.exe")
    foreach ($bin in $bins) {
        $src = "target\release\$bin"
        if (Test-Path $src) {
            Copy-Item $src "$OutDir\" -Force
            Write-Host "  -> $OutDir\$bin" -ForegroundColor Gray
        }
    }
}

function Build-Cross([string]$triple) {
    Write-Host "[1/3] Cross-compiling for $triple..." -ForegroundColor Green
    rustup target add $triple 2>$null

    switch ($triple) {
        "x86_64-pc-windows-gnu" {
            cargo build --release --target $triple
        }
        default {
            cargo build --release --target $triple
        }
    }

    $targetDir = "$OutDir\$triple"
    if (-not (Test-Path $targetDir)) { New-Item -ItemType Directory -Path $targetDir -Force | Out-Null }

    Get-ChildItem "target\$triple\release" -File |
        Where-Object { $_.Name -match '^qmind-(server|cli)' -and $_.Extension -notin '.d', '.pdb' } |
        ForEach-Object { Copy-Item $_.FullName "$targetDir\" -Force; Write-Host "  -> $targetDir\$($_.Name)" -ForegroundColor Gray }
}

# Build
if ($Target) {
    Build-Cross $Target
} else {
    Build-Native
}

# Test
if ($Test) {
    Write-Host "[2/3] Running tests..." -ForegroundColor Green
    cargo test --workspace --release 2>&1 | Select-Object -Last 5
} else {
    Write-Host "[2/3] Skipping tests (use -Test to enable)" -ForegroundColor Yellow
}

# Summary
Write-Host "[3/3] Build complete. Artifacts:" -ForegroundColor Green
if (Test-Path $OutDir) {
    Get-ChildItem $OutDir -Recurse -File | ForEach-Object {
        $rel = $_.FullName.Replace((Get-Location).Path + "\", "")
        $size = [math]::Round($_.Length / 1MB, 1)
        Write-Host "  $rel (${size} MB)" -ForegroundColor Gray
    }
}
Write-Host "=== Done ===" -ForegroundColor Cyan
