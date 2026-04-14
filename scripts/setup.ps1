#!/usr/bin/env pwsh
<#
.SYNOPSIS
    K.R.I.A. Setup Script for Windows
.DESCRIPTION
    Checks prerequisites, installs Python dependencies,
    creates required directories, generates .env file,
    and builds Docker images.
.PARAMETER SkipDocker
    Skip Docker image build
.PARAMETER DownloadModels
    Download AI models after setup
#>
param(
    [switch]$SkipDocker,
    [switch]$DownloadModels
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$KRIA_ROOT = $PSScriptRoot | Split-Path -Parent
$VENV_PATH  = Join-Path $KRIA_ROOT ".venv"
$ENV_FILE   = Join-Path $KRIA_ROOT ".env"
$ENV_EXAMPLE= Join-Path $KRIA_ROOT ".env.example"

Write-Host ""
Write-Host "  ██╗  ██╗██████╗ ██╗ █████╗ " -ForegroundColor Cyan
Write-Host "  ██║ ██╔╝██╔══██╗██║██╔══██╗" -ForegroundColor Cyan
Write-Host "  █████╔╝ ██████╔╝██║███████║" -ForegroundColor Cyan
Write-Host "  ██╔═██╗ ██╔══██╗██║██╔══██║" -ForegroundColor Cyan
Write-Host "  ██║  ██╗██║  ██║██║██║  ██║" -ForegroundColor Cyan
Write-Host "  ╚═╝  ╚═╝╚═╝  ╚═╝╚═╝╚═╝  ╚═╝" -ForegroundColor Cyan
Write-Host ""
Write-Host "  K.R.I.A. Windows Setup Script" -ForegroundColor Yellow
Write-Host "  =================================" -ForegroundColor Yellow
Write-Host ""

# ── Check prerequisites ────────────────────────────────────────────
function Test-Prerequisite([string]$Name, [scriptblock]$Test) {
    Write-Host "  Checking $Name... " -NoNewline
    try {
        & $Test | Out-Null
        Write-Host "OK" -ForegroundColor Green
        return $true
    } catch {
        Write-Host "MISSING" -ForegroundColor Red
        return $false
    }
}

$ok = $true
$ok = $ok -and (Test-Prerequisite "Python 3.12+" { python --version | Where-Object { $_ -match "3\.(1[2-9]|[2-9]\d)" } })
if (-not $SkipDocker) {
    $ok = $ok -and (Test-Prerequisite "Docker Desktop" { docker info })
}

$hasGpu = $false
try {
    nvidia-smi --query-gpu=name --format=csv,noheader | Out-Null
    Write-Host "  GPU detected — NVIDIA acceleration available" -ForegroundColor Green
    $hasGpu = $true
} catch {
    Write-Host "  [!] nvidia-smi not found — GPU features unavailable (CPU mode)" -ForegroundColor Yellow
}

if (-not $ok) {
    Write-Host ""
    Write-Host "  [!] One or more prerequisites are missing." -ForegroundColor Red
    Write-Host "      Install them and re-run this script." -ForegroundColor Red
    exit 1
}

# ── Python virtual environment ─────────────────────────────────────
Write-Host ""
Write-Host "  [1/5] Setting up Python virtualenv..." -ForegroundColor Yellow

if (-not (Test-Path $VENV_PATH)) {
    python -m venv $VENV_PATH
}

$pip = Join-Path $VENV_PATH "Scripts\pip.exe"
$python = Join-Path $VENV_PATH "Scripts\python.exe"

Write-Progress -Activity "Installing Python packages" -Status "Upgrading pip..." -PercentComplete 0
& $pip install --quiet --upgrade pip
Write-Progress -Activity "Installing Python packages" -Status "Installing setuptools, wheel..." -PercentComplete 25
& $pip install --quiet setuptools wheel
Write-Progress -Activity "Installing Python packages" -Status "Installing KRIA package and dependencies..." -PercentComplete 50
& $pip install --quiet -e "$KRIA_ROOT[dev,windows]"
Write-Progress -Activity "Installing Python packages" -Status "Installing httpx, tqdm..." -PercentComplete 85
& $pip install --quiet httpx tqdm
Write-Progress -Activity "Installing Python packages" -Completed

Write-Host "        Done." -ForegroundColor Green

# ── Environment file ───────────────────────────────────────────────
Write-Host "  [2/5] Configuring .env..." -ForegroundColor Yellow

if (-not (Test-Path $ENV_FILE)) {
    if (Test-Path $ENV_EXAMPLE) {
        Copy-Item $ENV_EXAMPLE $ENV_FILE
        Write-Host "        Created .env from .env.example" -ForegroundColor Green
        Write-Host "        [!] Review $ENV_FILE and adjust values if needed" -ForegroundColor Yellow
    } else {
        Write-Host "        [!] .env.example not found — skipping" -ForegroundColor Yellow
    }
} else {
    Write-Host "        .env already exists — skipping" -ForegroundColor Cyan
}

# ── Directories ────────────────────────────────────────────────────
Write-Host "  [3/5] Creating data directories..." -ForegroundColor Yellow
$dirs = @(
    "$env:USERPROFILE\.kria",
    "$env:USERPROFILE\.kria\rollback",
    "$env:USERPROFILE\.kria\logs",
    "$KRIA_ROOT\models\llm",
    "$KRIA_ROOT\models\stt",
    "$KRIA_ROOT\models\piper"
)
foreach ($d in $dirs) {
    New-Item -ItemType Directory -Force -Path $d | Out-Null
}
Write-Host "        Done." -ForegroundColor Green

# ── Bridge secret ──────────────────────────────────────────────────
Write-Host "  [4/5] Configuring bridge secret..." -ForegroundColor Yellow

$secretFile = "$env:USERPROFILE\.kria\bridge_secret.txt"
if (-not (Test-Path $secretFile)) {
    $bridgeSecret = [System.Convert]::ToHexString([System.Security.Cryptography.RandomNumberGenerator]::GetBytes(32)).ToLower()
    Set-Content -Path $secretFile -Value $bridgeSecret -NoNewline
    Write-Host "        Generated bridge secret → $secretFile" -ForegroundColor Green
} else {
    $bridgeSecret = Get-Content $secretFile
    Write-Host "        Bridge secret already exists" -ForegroundColor Cyan
}

# Write/update KRIA_BRIDGE_SECRET in .env
if (Test-Path $ENV_FILE) {
    $envContent = Get-Content $ENV_FILE -Raw
    if ($envContent -match "KRIA_BRIDGE_SECRET=") {
        $envContent = $envContent -replace "KRIA_BRIDGE_SECRET=.*", "KRIA_BRIDGE_SECRET=$bridgeSecret"
    } else {
        $envContent += "`nKRIA_BRIDGE_SECRET=$bridgeSecret"
    }
    Set-Content -Path $ENV_FILE -Value $envContent -NoNewline
    Write-Host "        KRIA_BRIDGE_SECRET written to .env" -ForegroundColor Green
}

# Copy bridge secret into docker/secrets/ for Docker secret mount
$dockerSecretDir  = Join-Path $KRIA_ROOT "docker\secrets"
$dockerSecretFile = Join-Path $dockerSecretDir "bridge_secret.txt"
New-Item -ItemType Directory -Force -Path $dockerSecretDir | Out-Null
Copy-Item -Path $secretFile -Destination $dockerSecretFile -Force
Write-Host "        Bridge secret copied to docker\secrets\bridge_secret.txt" -ForegroundColor Green

# ── Docker build ───────────────────────────────────────────────────
if (-not $SkipDocker) {
    Write-Host "  [5/5] Building Docker images..." -ForegroundColor Yellow
    Push-Location (Join-Path $KRIA_ROOT "docker")

    $composeFiles = @("-f", "docker-compose.yml")
    if (Test-Path "docker-compose.override.yml") {
        $composeFiles += @("-f", "docker-compose.override.yml")
    }
    if ($hasGpu -and (Test-Path "docker-compose.gpu.yml")) {
        $composeFiles += @("-f", "docker-compose.gpu.yml")
        Write-Host "        GPU detected — building with GPU support" -ForegroundColor Green
    }

    Write-Host "        Building images (live output below)..." -ForegroundColor Cyan
    docker compose @composeFiles build
    Pop-Location
    Write-Host "        Done." -ForegroundColor Green
} else {
    Write-Host "  [5/5] Skipping Docker build (-SkipDocker)" -ForegroundColor Cyan
}

if ($DownloadModels) {
    Write-Host ""
    Write-Host "  Downloading AI models (this may take a while)..." -ForegroundColor Yellow
    & $python (Join-Path $KRIA_ROOT "scripts\download_models.py")
}

Write-Host ""
Write-Host "  =============================================" -ForegroundColor Green
Write-Host "  K.R.I.A. setup complete!" -ForegroundColor Green
Write-Host ""
Write-Host "  Next steps:" -ForegroundColor White
Write-Host "    1. Download models (first time only):"
Write-Host "       python scripts\download_models.py"
Write-Host ""
Write-Host "    2. Start KRIA (Linux/macOS/WSL):"
Write-Host "       bash scripts/app-start.sh"
Write-Host ""
Write-Host "    3. Open Dashboard:"
Write-Host "       http://localhost:3000"
Write-Host ""
Write-Host "    4. (Optional) Start host bridge for mic/speaker:"
Write-Host "       python scripts\kria_bridge.py"
Write-Host ""
Write-Host "  Other commands:" -ForegroundColor White
Write-Host "    bash scripts/app-stop.sh           Stop all services"
Write-Host "    bash scripts/app-restart.sh        Restart with latest changes"
Write-Host "    bash scripts/app-restart.sh --quick Restart without rebuilding"
Write-Host ""
