# ============================================================
# K.R.I.A. — Uninstall Script (Windows PowerShell)
# Removes build artifacts, config, and optionally dependencies.
# Run as:  powershell -ExecutionPolicy Bypass -File scripts\uninstall.ps1 [-All] [-Config] [-Toolchains]
# ============================================================
param(
    [switch]$All,
    [switch]$Config,
    [switch]$Toolchains,
    [switch]$Help
)

$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$ProjectRoot = (Resolve-Path "$ScriptDir\..").Path

function Write-Step { param($msg) Write-Host "[INFO]  $msg" -ForegroundColor Cyan }
function Write-Ok   { param($msg) Write-Host "[OK]    $msg" -ForegroundColor Green }

if ($Help) {
    Write-Host "Usage: uninstall.ps1 [OPTIONS]"
    Write-Host ""
    Write-Host "Options:"
    Write-Host "  (none)        Remove build artifacts only (target\, node_modules\)"
    Write-Host "  -Config       Also remove ~\.kria\ config directory"
    Write-Host "  -Toolchains   Also remove cargo-tauri"
    Write-Host "  -All          Remove everything above"
    Write-Host "  -Help         Show this help"
    exit 0
}

if ($All) { $Config = $true; $Toolchains = $true }

Write-Host ""
Write-Step "K.R.I.A. Uninstall"
Write-Host ""

# ── 1. Rust build artifacts ─────────────────────────────────
$targetDir = "$ProjectRoot\target"
if (Test-Path $targetDir) {
    Write-Step "Removing Rust build artifacts (target\)…"
    Remove-Item -Recurse -Force $targetDir
    Write-Ok "target\ removed"
} else {
    Write-Ok "target\ already clean"
}

# ── 2. Frontend node_modules ────────────────────────────────
$nodeModules = "$ProjectRoot\ui\node_modules"
if (Test-Path $nodeModules) {
    Write-Step "Removing ui\node_modules\…"
    Remove-Item -Recurse -Force $nodeModules
    Write-Ok "ui\node_modules\ removed"
} else {
    Write-Ok "ui\node_modules\ already clean"
}

$uiDist = "$ProjectRoot\ui\dist"
if (Test-Path $uiDist) {
    Write-Step "Removing ui\dist\…"
    Remove-Item -Recurse -Force $uiDist
    Write-Ok "ui\dist\ removed"
}

# ── 3. Config directory ─────────────────────────────────────
$KriaHome = "$env:USERPROFILE\.kria"
if ($Config) {
    if (Test-Path $KriaHome) {
        Write-Step "Removing config directory ($KriaHome)…"
        Remove-Item -Recurse -Force $KriaHome
        Write-Ok "$KriaHome removed"
    } else {
        Write-Ok "$KriaHome already clean"
    }
} else {
    Write-Step "Keeping $KriaHome (use -Config or -All to remove)"
}

# ── 4. Tauri CLI ────────────────────────────────────────────
if ($Toolchains) {
    try {
        cargo tauri --version 2>$null | Out-Null
        Write-Step "Removing cargo-tauri…"
        cargo uninstall tauri-cli 2>$null
        Write-Ok "cargo-tauri removed"
    } catch {
        Write-Ok "cargo-tauri not installed"
    }
} else {
    Write-Step "Keeping cargo-tauri (use -Toolchains or -All to remove)"
}

# ── Done ─────────────────────────────────────────────────────
Write-Host ""
Write-Host "Uninstall complete." -ForegroundColor Green
Write-Host ""
