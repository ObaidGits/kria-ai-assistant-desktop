#!/usr/bin/env pwsh
<#
.SYNOPSIS
    K.R.I.A. Setup Script for Windows
.DESCRIPTION
    Checks prerequisites, installs Python dependencies,
    creates required directories, generates .env file,
    and optionally starts Docker services.
.PARAMETER SkipDocker
    Skip Docker service startup
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
    $ok = $ok -and (Test-Prerequisite "nvidia-smi" { nvidia-smi --query-gpu=name --format=csv,noheader })
}

if (-not $ok) {
    Write-Host ""
    Write-Host "  [!] One or more prerequisites are missing." -ForegroundColor Red
    Write-Host "      Install them and re-run this script." -ForegroundColor Red
    exit 1
}

# ── Python virtual environment ─────────────────────────────────────
Write-Host ""
Write-Host "  [1/4] Setting up Python virtualenv..." -ForegroundColor Yellow

if (-not (Test-Path $VENV_PATH)) {
    python -m venv $VENV_PATH
}

$pip = Join-Path $VENV_PATH "Scripts\pip.exe"
$python = Join-Path $VENV_PATH "Scripts\python.exe"

& $pip install --quiet --upgrade pip
& $pip install --quiet setuptools wheel  # ensure build backend is available
& $pip install --quiet -e "$KRIA_ROOT[dev,windows]"
& $pip install --quiet httpx tqdm  # required by download_models.py

Write-Host "        Done." -ForegroundColor Green

# ── Environment file ───────────────────────────────────────────────
Write-Host "  [2/4] Configuring .env..." -ForegroundColor Yellow

if (-not (Test-Path $ENV_FILE)) {
    Copy-Item $ENV_EXAMPLE $ENV_FILE
    Write-Host "        Created .env from .env.example" -ForegroundColor Green
    Write-Host "        [!] Review $ENV_FILE and set KRIA_BRIDGE_SECRET" -ForegroundColor Yellow
} else {
    Write-Host "        .env already exists — skipping" -ForegroundColor Cyan
}

# ── Directories ────────────────────────────────────────────────────
Write-Host "  [3/4] Creating data directories..." -ForegroundColor Yellow
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
# ── Docker / model download ────────────────────────────────────────
if (-not $SkipDocker) {
    Write-Host "  [4/4] Pulling Docker images..." -ForegroundColor Yellow
    Push-Location (Join-Path $KRIA_ROOT "docker")
    docker compose pull
    Pop-Location
    Write-Host "        Done." -ForegroundColor Green
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
Write-Host "  Quick start (Docker):" -ForegroundColor White
Write-Host "    1. Download models:   python scripts\download_models.py"
Write-Host "    2. Start stack (CPU): Set-Location docker; docker compose up -d"
Write-Host "       Start stack (GPU): Set-Location docker; docker compose -f docker-compose.yml -f docker-compose.gpu.yml up -d"
Write-Host "    3. Start bridge:      python scripts\kria_bridge.py   (in a separate terminal)"
Write-Host "    4. Dashboard:         http://localhost:3000"
Write-Host "    5. API docs:          http://localhost:8000/docs"
Write-Host ""
