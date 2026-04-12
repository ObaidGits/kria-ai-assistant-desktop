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
check nvidia-smi || warn "nvidia-smi not found — GPU features may be unavailable"

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
pip install --quiet --upgrade pip
pip install --quiet -e "$KRIA_ROOT[dev]"
ok "Virtualenv ready at $VENV_PATH"

# ── .env ──────────────────────────────────────────────────────────
header "Configuring .env..."

if [ ! -f "$ENV_FILE" ]; then
    cp "$ENV_EXAMPLE" "$ENV_FILE"
    ok "Created .env from .env.example"
    warn "Review $ENV_FILE and set KRIA_BRIDGE_SECRET"
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
    ok "Generated new bridge secret → $SECRET_FILE"
else
    ok "Bridge secret already exists — skipping"
fi

# Write the same secret into docker-compose.yml (uses $HOME path, not NTFS path)
# so Docker can bind-mount it from the Linux filesystem.
sed -i "s|file: .*bridge_secret.txt|file: ${SECRET_FILE}|" \
    "$KRIA_ROOT/docker/docker-compose.yml"
ok "docker-compose.yml updated with secret path: $SECRET_FILE"

# Make sure KRIA_BRIDGE_SECRET in .env matches
BRIDGE_SECRET="$(cat "$SECRET_FILE")"
if grep -q "^KRIA_BRIDGE_SECRET=" "$ENV_FILE" 2>/dev/null; then
    sed -i "s|^KRIA_BRIDGE_SECRET=.*|KRIA_BRIDGE_SECRET=${BRIDGE_SECRET}|" "$ENV_FILE"
else
    echo "KRIA_BRIDGE_SECRET=${BRIDGE_SECRET}" >> "$ENV_FILE"
fi
ok "KRIA_BRIDGE_SECRET written to .env"

# ── Docker ────────────────────────────────────────────────────────
if [ "${SKIP_DOCKER:-0}" != "1" ]; then
    header "Pulling Docker images..."
    cd "$KRIA_ROOT/docker"
    docker compose pull
    ok "Docker images pulled"
fi

# ── Summary ───────────────────────────────────────────────────────
echo ""
echo -e "${GREEN}${BOLD}  Setup complete!${RESET}"
echo ""
echo "  Next steps:"
echo "    1. Edit .env and set KRIA_BRIDGE_SECRET"
echo "    2. python scripts/download_models.py"
echo "    3. cd docker && docker compose up -d"
echo "    4. python scripts/kria_bridge.py  (run on host, not in Docker)"
echo "    5. http://localhost:3000  ← Dashboard"
echo "    6. http://localhost:8000/docs  ← API docs"
echo ""
