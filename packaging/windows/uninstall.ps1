#Requires -RunAsAdministrator
<#
.SYNOPSIS
    QuantsMind — Windows uninstall script
#>

param(
    [string]$InstallDir = "$env:ProgramFiles\QuantsMind",
    [string]$DataDir = "$env:USERPROFILE\.qmind"
)

$ErrorActionPreference = "Stop"

Write-Host "=== QuantsMind uninstall (Windows) ===" -ForegroundColor Cyan

# Stop service if running
$Service = Get-Service -Name "QuantsMind" -ErrorAction SilentlyContinue
if ($Service -and $Service.Status -eq "Running") {
    Stop-Service -Name "QuantsMind"
    Write-Host "Stopped QuantsMind service"
}

# Remove service
if ($Service) {
    Remove-Service -Name "QuantsMind" -ErrorAction SilentlyContinue
    # Fallback: sc.exe delete
    sc.exe delete QuantsMind 2>$null | Out-Null
    Write-Host "Removed QuantsMind service"
}

# Remove binaries
if (Test-Path $InstallDir) {
    Remove-Item -Recurse -Force $InstallDir
    Write-Host "Removed $InstallDir"
}

# Remove from PATH
$CurrentPath = [Environment]::GetEnvironmentVariable("Path", "Machine")
if ($CurrentPath -like "*$InstallDir*") {
    $NewPath = ($CurrentPath -split ";" | Where-Object { $_ -ne $InstallDir }) -join ";"
    [Environment]::SetEnvironmentVariable("Path", $NewPath, "Machine")
    Write-Host "Removed from system PATH"
}

# Remove data
if (Test-Path $DataDir) {
    $Choice = Read-Host "Remove data directory $DataDir? (y/N)"
    if ($Choice -eq "y" -or $Choice -eq "Y") {
        Remove-Item -Recurse -Force $DataDir
        Write-Host "Removed $DataDir"
    }
}

Write-Host "=== Uninstall complete ===" -ForegroundColor Green
