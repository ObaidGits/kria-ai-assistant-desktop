# ============================================================
# K.R.I.A. — Setup Script (Windows PowerShell)
# Idempotent: safe to re-run at any time.
# Run as:  powershell -ExecutionPolicy Bypass -File scripts\setup.ps1
# ============================================================
$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$ProjectRoot = (Resolve-Path "$ScriptDir\..").Path

function Write-Step  { param($msg) Write-Host "[INFO]  $msg" -ForegroundColor Cyan }
function Write-Ok    { param($msg) Write-Host "[OK]    $msg" -ForegroundColor Green }
function Write-Warn  { param($msg) Write-Host "[WARN]  $msg" -ForegroundColor Yellow }
function Write-Fail  { param($msg) Write-Host "[FAIL]  $msg" -ForegroundColor Red; exit 1 }
function Has-Command { param($cmd) return [bool](Get-Command $cmd -ErrorAction SilentlyContinue) }

Write-Host ""
Write-Host "K.R.I.A. Setup — Windows" -ForegroundColor Cyan
Write-Host "========================" -ForegroundColor Cyan
Write-Host ""

# ── 1. System dependencies (via winget) ─────────────────────
Write-Step "Step 1/6 — Checking system package manager…"

$hasWinget = Has-Command "winget"
if (-not $hasWinget) {
    Write-Warn "winget not found. Install App Installer from the Microsoft Store, then re-run."
    Write-Warn "Continuing — will check for Rust and Node.js manually."
}

# ── 2. Rust toolchain ───────────────────────────────────────
Write-Step "Step 2/6 — Checking Rust toolchain…"

if (Has-Command "rustup") {
    Write-Ok "rustup already installed"
} else {
    Write-Step "Installing Rust via rustup…"
    if ($hasWinget) {
        winget install --id Rustlang.Rustup -e --accept-package-agreements --accept-source-agreements
    } else {
        Write-Step "Downloading rustup-init.exe…"
        $rustupUrl = "https://win.rustup.rs/x86_64"
        $rustupExe = "$env:TEMP\rustup-init.exe"
        Invoke-WebRequest -Uri $rustupUrl -OutFile $rustupExe -UseBasicParsing
        & $rustupExe -y --default-toolchain stable
        Remove-Item $rustupExe -ErrorAction SilentlyContinue
    }
}

# Refresh PATH to pick up cargo
$cargoDir = "$env:USERPROFILE\.cargo\bin"
if (Test-Path $cargoDir) {
    $env:PATH = "$cargoDir;$env:PATH"
}

if (-not (Has-Command "cargo")) {
    Write-Fail "cargo not found after Rust install. Please restart your terminal and re-run."
}

rustup default stable
Write-Ok "Rust $(rustc --version) ready"

# ── 3. Install Tauri CLI ────────────────────────────────────
Write-Step "Step 3/6 — Checking Tauri CLI…"

$tauriInstalled = $false
try {
    cargo tauri --version 2>$null | Out-Null
    $tauriInstalled = $true
} catch {}

if ($tauriInstalled) {
    Write-Ok "cargo-tauri already installed"
} else {
    Write-Step "Installing cargo-tauri (this may take a few minutes)…"
    cargo install tauri-cli --version "^2" --locked
    Write-Ok "cargo-tauri installed"
}

# ── 4. Node.js ──────────────────────────────────────────────
Write-Step "Step 4/6 — Checking Node.js…"

$MinNode = 18
if (Has-Command "node") {
    $nodeVer = (node -v) -replace 'v','' -split '\.' | Select-Object -First 1
    if ([int]$nodeVer -ge $MinNode) {
        Write-Ok "Node.js $(node -v) ready"
    } else {
        Write-Warn "Node.js $(node -v) is below v$MinNode — please upgrade"
    }
} else {
    Write-Step "Installing Node.js…"
    if ($hasWinget) {
        winget install --id OpenJS.NodeJS.LTS -e --accept-package-agreements --accept-source-agreements
    } else {
        Write-Fail "Node.js not found and winget unavailable. Install Node.js from https://nodejs.org"
    }
    # Refresh PATH
    $env:PATH = [Environment]::GetEnvironmentVariable("PATH", "Machine") + ";" + [Environment]::GetEnvironmentVariable("PATH", "User")
    if (-not (Has-Command "node")) {
        Write-Fail "Node.js not found after install. Restart your terminal and re-run."
    }
    Write-Ok "Node.js $(node -v) installed"
}

if (-not (Has-Command "npm")) {
    Write-Fail "npm not found. Please install Node.js from https://nodejs.org"
}

# ── 5. Frontend dependencies ────────────────────────────────
Write-Step "Step 5/6 — Installing frontend dependencies…"

Push-Location "$ProjectRoot\ui"
npm install --no-audit --no-fund
Pop-Location
Write-Ok "Frontend dependencies ready"

# ── 6. Build workspace ──────────────────────────────────────
Write-Step "Step 6/6 — Building Rust workspace…"

Push-Location $ProjectRoot
cargo build --workspace
Pop-Location
Write-Ok "Workspace built successfully"

# ── 7. Config ────────────────────────────────────────────────
$KriaHome = "$env:USERPROFILE\.kria"
$configDest = "$KriaHome\config.toml"

if (-not (Test-Path $configDest)) {
    New-Item -ItemType Directory -Path $KriaHome -Force | Out-Null
    Copy-Item "$ProjectRoot\config\default.toml" $configDest
    Write-Ok "Default config copied to $configDest"
} else {
    Write-Ok "Config already exists at $configDest"
}

# ── Done ─────────────────────────────────────────────────────
Write-Host ""
Write-Host "========================================" -ForegroundColor Green
Write-Host "  K.R.I.A. setup complete!" -ForegroundColor Green
Write-Host "========================================" -ForegroundColor Green
Write-Host ""
Write-Host "Quick start:"
Write-Host "  Desktop app :  cd crates\kria-desktop; cargo tauri dev"
Write-Host "  Server only :  cargo run -p kria-server"
Write-Host "  Run tests   :  cargo test --workspace"
Write-Host ""
