# ============================================================
# K.R.I.A. — Production Release Build (Windows PowerShell)
# Produces platform-native bundles via Tauri (.msi / .exe).
# Run as:  powershell -ExecutionPolicy Bypass -File scripts\build-release.ps1
# ============================================================
$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$ProjectRoot = (Resolve-Path "$ScriptDir\..").Path

function Write-Step { param($msg) Write-Host "[INFO]  $msg" -ForegroundColor Cyan }
function Write-Ok   { param($msg) Write-Host "[OK]    $msg" -ForegroundColor Green }
function Write-Fail { param($msg) Write-Host "[FAIL]  $msg" -ForegroundColor Red; exit 1 }
function Has-Command { param($cmd) return [bool](Get-Command $cmd -ErrorAction SilentlyContinue) }

Write-Host ""
Write-Step "Building KRIA release for Windows"
Write-Host ""

# Ensure cargo is on PATH
$cargoDir = "$env:USERPROFILE\.cargo\bin"
if (Test-Path $cargoDir) { $env:PATH = "$cargoDir;$env:PATH" }

# ── Pre-flight checks ───────────────────────────────────────
if (-not (Has-Command "cargo")) { Write-Fail "cargo not found. Run scripts\setup.ps1 first." }
if (-not (Has-Command "node"))  { Write-Fail "node not found. Run scripts\setup.ps1 first." }
if (-not (Has-Command "npm"))   { Write-Fail "npm not found. Run scripts\setup.ps1 first." }

try { cargo tauri --version 2>$null | Out-Null }
catch { Write-Fail "cargo-tauri not found. Run: cargo install tauri-cli --version '^2' --locked" }

# ── 1. Install / update frontend deps ───────────────────────
Write-Step "Step 1/3 — Frontend dependencies…"
Push-Location "$ProjectRoot\ui"
npm install --no-audit --no-fund
npm run build
Pop-Location
Write-Ok "Frontend built (ui\dist\)"

# ── 2. Build the Tauri app ──────────────────────────────────
Write-Step "Step 2/3 — Building Tauri release (this may take several minutes)…"
Push-Location "$ProjectRoot\crates\kria-desktop"
cargo tauri build
Pop-Location
Write-Ok "Tauri build finished"

# ── 3. Locate outputs ───────────────────────────────────────
Write-Step "Step 3/3 — Locating bundles…"
$BundleDir = "$ProjectRoot\target\release\bundle"

Write-Host ""
Write-Host "========================================" -ForegroundColor Green
Write-Host "  Release build complete!" -ForegroundColor Green
Write-Host "========================================" -ForegroundColor Green
Write-Host ""

if (Test-Path $BundleDir) {
    Write-Step "Bundles:"
    Get-ChildItem -Path $BundleDir -Recurse -Include "*.msi","*.exe","*.nsis" -File -ErrorAction SilentlyContinue | ForEach-Object {
        $size = "{0:N1} MB" -f ($_.Length / 1MB)
        Write-Host "  $size  $($_.FullName)"
    }
} else {
    Write-Step "Bundle directory: $BundleDir"
}

Write-Host ""
Write-Step "The standalone server can also be found at:"
Write-Host "  $ProjectRoot\target\release\kria-server.exe"
Write-Host ""
