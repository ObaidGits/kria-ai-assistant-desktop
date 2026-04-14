#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────
# K.R.I.A. — Stop All Services
# Stops and removes all KRIA containers, frees ports, and clears
# ephemeral caches.  Persistent data is PRESERVED:
#   ✓ Chat history & sessions  (kria-data volume — SQLite)
#   ✓ User preferences         (kria-data volume — SQLite)
#   ✓ Semantic memory           (kria-chroma-data volume)
#   ✓ Model config              (kria-data volume — /data/model_size)
#   ✓ Redis conversation cache  (kria-redis-data volume)
#
# To also wipe ALL data: app-stop.sh --wipe
# ─────────────────────────────────────────────────────────────────
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
KRIA_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
COMPOSE_DIR="$KRIA_ROOT/docker"

BOLD="\033[1m"
GREEN="\033[32m"
YELLOW="\033[33m"
CYAN="\033[36m"
RED="\033[31m"
RESET="\033[0m"

info()  { echo -e "${CYAN}${BOLD}[KRIA]${RESET} $*"; }
ok()    { echo -e "${GREEN}${BOLD}[KRIA]${RESET} $*"; }
warn()  { echo -e "${YELLOW}${BOLD}[KRIA]${RESET} $*"; }
fail()  { echo -e "${RED}${BOLD}[KRIA]${RESET} $*"; }

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

WIPE=false
if [[ "${1:-}" == "--wipe" ]]; then
    WIPE=true
    warn "⚠️  --wipe flag set: ALL persistent data will be deleted!"
    read -rp "  Are you sure? Type YES to confirm: " CONFIRM
    if [[ "$CONFIRM" != "YES" ]]; then
        info "Aborted."
        exit 0
    fi
fi

# ── Compose files ────────────────────────────────────────────────
COMPOSE_FILES="-f $COMPOSE_DIR/docker-compose.yml"
if [[ -f "$COMPOSE_DIR/docker-compose.override.yml" ]]; then
    COMPOSE_FILES="$COMPOSE_FILES -f $COMPOSE_DIR/docker-compose.override.yml"
fi
if [[ -f "$COMPOSE_DIR/docker-compose.gpu.yml" ]] && nvidia-smi &>/dev/null; then
    COMPOSE_FILES="$COMPOSE_FILES -f $COMPOSE_DIR/docker-compose.gpu.yml"
fi

# ── Stop containers ──────────────────────────────────────────────
RUNNING=$(docker ps --filter "name=kria-" --format "{{.Names}}" 2>/dev/null | wc -l)
if [[ "$RUNNING" -eq 0 ]]; then
    info "No KRIA containers are running."
else
    info "Stopping $RUNNING KRIA container(s)..."
    start_spinner "Waiting for containers to stop (timeout: 15s)..."
    docker compose $COMPOSE_FILES down --timeout 15 >/dev/null 2>&1
    stop_spinner
    ok "All containers stopped and removed."
fi

# ── Clear ephemeral cache volume ─────────────────────────────────
# kria-core-cache holds pip/httpx caches — safe to remove
if docker volume inspect kria-core-cache &>/dev/null; then
    docker volume rm kria-core-cache &>/dev/null || true
    ok "Cleared core cache volume."
fi

# ── Clear Docker build cache ─────────────────────────────────────
info "Pruning dangling Docker images..."
docker image prune -f --filter "label=com.docker.compose.project=docker" &>/dev/null || true

# ── Wipe persistent data (only with --wipe) ──────────────────────
if $WIPE; then
    warn "Removing ALL persistent volumes..."
    for vol in kria-data kria-redis-data kria-chroma-data; do
        if docker volume inspect "$vol" &>/dev/null; then
            docker volume rm "$vol" &>/dev/null || true
            warn "  Removed $vol"
        fi
    done
    warn "All persistent data has been wiped."
else
    ok "Persistent data preserved (chat history, memory, preferences)."
    info "  Volumes kept: kria-data, kria-redis-data, kria-chroma-data"
fi

# ── Verify ports are free ────────────────────────────────────────
info "Verifying ports are free..."
PORTS_IN_USE=false
for PORT in 3000 8080 8081 8082 8083 8085 8088 6379; do
    if ss -tlnp 2>/dev/null | grep -q ":${PORT} " || \
       lsof -iTCP:${PORT} -sTCP:LISTEN &>/dev/null 2>&1; then
        warn "  Port $PORT is still in use by another process"
        PORTS_IN_USE=true
    fi
done
if ! $PORTS_IN_USE; then
    ok "All KRIA ports are free."
fi

echo ""
ok "KRIA stopped."
if ! $WIPE; then
    info "Your data is safe. Run app-start.sh to start again with full history."
fi
echo ""
