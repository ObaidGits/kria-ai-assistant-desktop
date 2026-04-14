#!/usr/bin/env bash
# K.R.I.A. Setup Script for Linux / macOS
set -euo pipefail

KRIA_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VENV_PATH="$KRIA_ROOT/.venv"
ENV_FILE="$KRIA_ROOT/.env"
ENV_EXAMPLE="$KRIA_ROOT/.env.example"

BOLD="\033[1m"
GREEN="\033[32m"
YELLOW="\033[33m"
CYAN="\033[36m"
RED="\033[31m"
RESET="\033[0m"

header() { echo -e "\n${CYAN}${BOLD}  $1${RESET}"; }
ok()     { echo -e "  ${GREEN}✓${RESET} $1"; }
warn()   { echo -e "  ${YELLOW}!${RESET} $1"; }
fail()   { echo -e "  ${RED}✗${RESET} $1"; }
# ── Progress helpers ─────────────────────────────────────────────
_SPIN=("⠋" "⠙" "⠹" "⠸" "⠼" "⠴" "⠦" "⠧" "⠇" "⠏")
_SPIN_PID=""

_spin_loop() {
    local msg="$1"; local i=0; local t=0
    while true; do
        printf "\r  \033[36m%s\033[0m %s  (%ds)" "${_SPIN[$((i%10))]}" "$msg" "$t"
        sleep 1; i=$((i+1)); t=$((t+1))
    done
}
start_spinner() { _spin_loop "$1" & _SPIN_PID=$!; }
stop_spinner()  {
    if [[ -n "$_SPIN_PID" ]]; then
        kill "$_SPIN_PID" 2>/dev/null || true
        wait "$_SPIN_PID" 2>/dev/null || true
        _SPIN_PID=""
    fi
    printf "\r\033[K"
}
trap 'stop_spinner' EXIT
echo ""
echo -e "${CYAN}${BOLD}  K.R.I.A. Linux/macOS Setup Script${RESET}"
echo -e "${YELLOW}  =====================================${RESET}"
echo ""

# ── Prerequisites ─────────────────────────────────────────────────
header "Checking prerequisites..."

check() {
    if "$@" &>/dev/null; then ok "$1 found"; return 0
    else fail "$1 NOT found"; return 1; fi
}

MISSING=0
check python3 --version || MISSING=$((MISSING+1))
python3 -c "import sys; sys.exit(0 if sys.version_info >= (3,12) else 1)" \
    && ok "Python 3.12+" || { fail "Python 3.12+ required"; MISSING=$((MISSING+1)); }
check docker version     || MISSING=$((MISSING+1))

HAS_GPU=false
if nvidia-smi &>/dev/null; then
    ok "nvidia-smi found — GPU acceleration available"
    HAS_GPU=true
else
    warn "nvidia-smi not found — GPU features unavailable (CPU mode only)"
fi

[ "$MISSING" -gt 0 ] && { fail "Missing prerequisites — install them first"; exit 1; }

# ── Virtual environment ────────────────────────────────────────────
header "Setting up Python virtualenv..."

# If a broken/Windows-format venv exists (Scripts/ instead of bin/), remove it
if [ -d "$VENV_PATH" ] && [ ! -f "$VENV_PATH/bin/activate" ]; then
    warn "Removing broken/incompatible .venv (missing bin/activate)..."
    rm -rf "$VENV_PATH"
fi

if [ ! -d "$VENV_PATH" ]; then
    # --copies is required on NTFS/exFAT mounts where symlinks are unreliable
    python3 -m venv --copies "$VENV_PATH"
fi

source "$VENV_PATH/bin/activate"
start_spinner "Upgrading pip..."
pip install --quiet --upgrade pip
stop_spinner
start_spinner "Installing build tools (setuptools, wheel)..."
pip install --quiet setuptools wheel
stop_spinner
start_spinner "Installing KRIA package and dependencies (this may take a minute)..."
pip install --quiet -e "$KRIA_ROOT[dev]"
stop_spinner
start_spinner "Installing extra tools (httpx, tqdm)..."
pip install --quiet httpx tqdm  # required by download_models.py
stop_spinner
ok "Virtualenv ready at $VENV_PATH"

# ── .env ──────────────────────────────────────────────────────────
header "Configuring .env..."

if [ ! -f "$ENV_FILE" ]; then
    if [ -f "$ENV_EXAMPLE" ]; then
        cp "$ENV_EXAMPLE" "$ENV_FILE"
        ok "Created .env from .env.example"
        warn "Review $ENV_FILE and adjust values if needed"
    else
        warn ".env.example not found — skipping .env creation"
    fi
else
    ok ".env already exists — skipping"
fi

# ── Directories ────────────────────────────────────────────────────
header "Creating data directories..."

mkdir -p \
    "$HOME/.kria/rollback" \
    "$HOME/.kria/logs" \
    "$KRIA_ROOT/models/llm" \
    "$KRIA_ROOT/models/stt" \
    "$KRIA_ROOT/models/piper"
ok "Directories ready"

# ── Bridge secret ─────────────────────────────────────────────────
header "Configuring bridge secret..."

SECRET_FILE="$HOME/.kria/bridge_secret.txt"
if [ ! -f "$SECRET_FILE" ]; then
    python3 -c "import secrets; print(secrets.token_hex(32))" > "$SECRET_FILE"
    chmod 600 "$SECRET_FILE"
    ok "Generated new bridge secret → $SECRET_FILE"
else
    ok "Bridge secret already exists — skipping"
fi

# Update docker-compose.yml with the correct secret path
sed -i "s|file: .*bridge_secret.txt|file: ${SECRET_FILE}|" \
    "$KRIA_ROOT/docker/docker-compose.yml"
ok "docker-compose.yml updated with secret path: $SECRET_FILE"

# Make sure KRIA_BRIDGE_SECRET in .env matches
if [ -f "$ENV_FILE" ]; then
    BRIDGE_SECRET="$(cat "$SECRET_FILE")"
    if grep -q "^KRIA_BRIDGE_SECRET=" "$ENV_FILE" 2>/dev/null; then
        sed -i "s|^KRIA_BRIDGE_SECRET=.*|KRIA_BRIDGE_SECRET=${BRIDGE_SECRET}|" "$ENV_FILE"
    else
        echo "KRIA_BRIDGE_SECRET=${BRIDGE_SECRET}" >> "$ENV_FILE"
    fi
    ok "KRIA_BRIDGE_SECRET written to .env"
fi

# ── Make app scripts executable ───────────────────────────────────
header "Setting script permissions..."

chmod +x "$KRIA_ROOT/scripts/app-start.sh" \
         "$KRIA_ROOT/scripts/app-stop.sh" \
         "$KRIA_ROOT/scripts/app-restart.sh" 2>/dev/null || true
ok "App scripts are executable"

# ── Docker images ─────────────────────────────────────────────────
if [ "${SKIP_DOCKER:-0}" != "1" ]; then
    header "Building Docker images..."
    cd "$KRIA_ROOT/docker"

    COMPOSE_FILES="-f docker-compose.yml"
    if [ -f "docker-compose.override.yml" ]; then
        COMPOSE_FILES="$COMPOSE_FILES -f docker-compose.override.yml"
    fi
    if $HAS_GPU && [ -f "docker-compose.gpu.yml" ]; then
        COMPOSE_FILES="$COMPOSE_FILES -f docker-compose.gpu.yml"
        ok "GPU detected — building with GPU support"
    fi

    docker compose $COMPOSE_FILES build
    ok "Docker images built"
    cd "$KRIA_ROOT"
fi

# ── Summary ───────────────────────────────────────────────────────
echo ""
echo -e "${GREEN}${BOLD}  Setup complete!${RESET}"
echo ""
echo "  Next steps:"
echo "    1. Download models (first time only):"
echo "       python3 scripts/download_models.py"
echo ""
echo "    2. Start KRIA:"
echo "       bash scripts/app-start.sh"
echo ""
echo "    3. Open Dashboard:"
echo "       http://localhost:3000"
echo ""
echo "    4. (Optional) Start host bridge for mic/speaker access:"
echo "       python3 scripts/kria_bridge.py"
echo ""
echo "  Other commands:"
echo "    bash scripts/app-stop.sh        Stop all services"
echo "    bash scripts/app-restart.sh     Restart with latest changes"
echo "    bash scripts/app-restart.sh --quick   Restart without rebuilding"
echo ""
