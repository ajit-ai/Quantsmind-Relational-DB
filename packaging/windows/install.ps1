#Requires -RunAsAdministrator
<#
.SYNOPSIS
    QuantsMind — Windows install script
.DESCRIPTION
    Installs qmind-server and qmind-cli to Program Files
    Supports: winget, scoop, choco, manual binary install
.PARAMETER Version
    Version to install (default: 0.1.0)
.PARAMETER InstallDir
    Installation directory (default: C:\Program Files\QuantsMind)
.EXAMPLE
    .\install.ps1
    .\install.ps1 -Version 0.2.0 -InstallDir "D:\QuantsMind"
#>

param(
    [string]$Version = "0.1.0",
    [string]$InstallDir = "$env:ProgramFiles\QuantsMind",
    [string]$DataDir = "$env:USERPROFILE\.qmind"
)

$ErrorActionPreference = "Stop"
$Repo = "ajit-ai/Quantsmind-Relational-DB"

Write-Host "=== QuantsMind v$Version installer (Windows) ===" -ForegroundColor Cyan
Write-Host ""

# ── Download ──
Write-Host "[1/4] Downloading binaries..." -ForegroundColor Yellow
$Asset = "qmind-windows-x64-$Version.zip"
$Url = "https://github.com/$Repo/releases/download/v$Version/$Asset"
$TmpDir = Join-Path $env:TEMP "qmind-install-$(Get-Random)"
New-Item -ItemType Directory -Path $TmpDir -Force | Out-Null

$ZipPath = Join-Path $TmpDir $Asset
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -Uri $Url -OutFile $ZipPath -UseBasicParsing
} catch {
    Write-Host "Failed to download: $Url" -ForegroundColor Red
    Write-Host "Error: $_" -ForegroundColor Red
    exit 1
}

Write-Host "  Extracting $Asset..."
Expand-Archive -Path $ZipPath -DestinationPath $TmpDir -Force

# ── Install ──
Write-Host "[2/4] Installing to $InstallDir..." -ForegroundColor Yellow
New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
Copy-Item (Join-Path $TmpDir "qmind-server.exe") $InstallDir -Force
Copy-Item (Join-Path $TmpDir "qmind-cli.exe") $InstallDir -Force
Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue

# ── Add to PATH ──
Write-Host "[3/4] Adding to PATH..." -ForegroundColor Yellow
$CurrentPath = [Environment]::GetEnvironmentVariable("Path", "Machine")
if ($CurrentPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$CurrentPath;$InstallDir", "Machine")
    $env:Path += ";$InstallDir"
    Write-Host "  Added $InstallDir to system PATH"
}

# ── Create data directory ──
Write-Host "[4/4] Setting up data directory..." -ForegroundColor Yellow
New-Item -ItemType Directory -Path $DataDir -Force | Out-Null

# ── Create Windows Service ──
Write-Host ""
Write-Host "Creating Windows service (optional)..." -ForegroundColor Yellow
$ServiceExists = Get-Service -Name "QuantsMind" -ErrorAction SilentlyContinue
if (-not $ServiceExists) {
    $Choice = Read-Host "Install as Windows service? (y/N)"
    if ($Choice -eq "y" -or $Choice -eq "Y") {
        $BinaryPath = Join-Path $InstallDir "qmind-server.exe"
        New-Service -Name "QuantsMind" `
            -DisplayName "QuantsMind Database Server" `
            -BinaryPathName "`"$BinaryPath`" 5432" `
            -StartupType Manual `
            -Description "QuantsMind Relational Database Engine"
        Write-Host "  Service 'QuantsMind' created. Start with:" -ForegroundColor Green
        Write-Host "    Start-Service QuantsMind"
    }
} else {
    Write-Host "  Service 'QuantsMind' already exists"
}

Write-Host ""
Write-Host "=== Installation complete ===" -ForegroundColor Green
Write-Host ""
Write-Host "Server:  $InstallDir\qmind-server.exe [PORT]"
Write-Host "CLI:     $InstallDir\qmind-cli.exe [HOST:PORT]"
Write-Host "Data:    $DataDir"
Write-Host ""
Write-Host "Quick start:"
Write-Host "  qmind-server 5432            # start server on port 5432"
Write-Host "  qmind-cli 127.0.0.1:5432     # connect with CLI"
