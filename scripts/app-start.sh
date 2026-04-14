#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────
# K.R.I.A. — Fresh Start
# Builds images (if needed), starts all services, and waits for
# health checks to pass.  Persistent data (chat history, memory,
# preferences, ChromaDB embeddings) is preserved across starts.
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

_print_health_table() {
    local elapsed=$1 max=$2 si=$3
    printf "  \033[36m%s\033[0m Waiting for services...  %ds / %ds\033[K\n" \
        "${_SPIN[$((si%10))]}" "$elapsed" "$max"
    for svc in $SERVICES; do
        local sv
        sv=$(docker inspect --format='{{.State.Health.Status}}' "$svc" 2>/dev/null || echo "missing")
        local short="${svc#kria-}"
        case "$sv" in
            healthy)   printf "     \033[32m✓\033[0m  %-14s  healthy\033[K\n"      "$short" ;;
            starting)  printf "     \033[33m○\033[0m  %-14s  starting...\033[K\n"  "$short" ;;
            unhealthy) printf "     \033[31m✗\033[0m  %-14s  UNHEALTHY\033[K\n"    "$short" ;;
            missing)   printf "     \033[31m~\033[0m  %-14s  not found\033[K\n"    "$short" ;;
            *)         printf "     \033[35m~\033[0m  %-14s  %s\033[K\n"           "$short" "$sv" ;;
        esac
    done
}

# ── Detect GPU mode ──────────────────────────────────────────────
COMPOSE_FILES="-f $COMPOSE_DIR/docker-compose.yml"
if [[ -f "$COMPOSE_DIR/docker-compose.override.yml" ]]; then
    COMPOSE_FILES="$COMPOSE_FILES -f $COMPOSE_DIR/docker-compose.override.yml"
fi
if nvidia-smi &>/dev/null && [[ -f "$COMPOSE_DIR/docker-compose.gpu.yml" ]]; then
    COMPOSE_FILES="$COMPOSE_FILES -f $COMPOSE_DIR/docker-compose.gpu.yml"
    info "GPU detected — using GPU compose override"
else
    info "Running in CPU mode"
fi

# ── Pre-flight checks ───────────────────────────────────────────
info "Checking prerequisites..."

if ! command -v docker &>/dev/null; then
    fail "Docker is not installed. Run setup.sh first."
    exit 1
fi

if ! docker info &>/dev/null; then
    fail "Docker daemon is not running. Start Docker first."
    exit 1
fi

# Check models exist
if [[ ! -f "$KRIA_ROOT/models/llm/microsoft_Phi-4-mini-instruct-Q4_K_M.gguf" ]]; then
    warn "Primary LLM model (Phi-4-mini) not found. Run: python3 scripts/download_models.py"
    warn "Starting anyway — brain will fall back to any available model."
fi
if [[ ! -f "$KRIA_ROOT/models/llm/Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf" ]]; then
    info "Secondary model (Qwen2.5-VL-7B) not found — secondary brain will not start."
    info "To download: python3 scripts/download_models.py"
fi

# ── Check if already running ─────────────────────────────────────
RUNNING=$(docker ps --filter "name=kria-" --format "{{.Names}}" 2>/dev/null | wc -l)
if [[ "$RUNNING" -gt 0 ]]; then
    warn "KRIA services are already running ($RUNNING containers)."
    warn "Use app-restart.sh to restart, or app-stop.sh first."
    exit 1
fi

# ── Build & start ────────────────────────────────────────────────
info "Building images (if needed)..."
docker compose $COMPOSE_FILES build

start_spinner "Starting all services..."
docker compose $COMPOSE_FILES up -d >/dev/null 2>&1
stop_spinner
ok "Services started."

# ── Wait for health ──────────────────────────────────────────────
info "Waiting for services to become healthy..."
SERVICES="kria-data kria-brain kria-core kria-dashboard"
MAX_WAIT=300
ELAPSED=0
INTERVAL=5

all_healthy() {
    for svc in $SERVICES; do
        STATUS=$(docker inspect --format='{{.State.Health.Status}}' "$svc" 2>/dev/null || echo "missing")
        if [[ "$STATUS" != "healthy" ]]; then
            return 1
        fi
    done
    return 0
}

SVC_COUNT=0; for _s in $SERVICES; do SVC_COUNT=$((SVC_COUNT+1)); done
TABLE_H=$((SVC_COUNT+1))
_print_health_table 0 $MAX_WAIT 0
SPIN_I=1
while ! all_healthy; do
    if [[ $ELAPSED -ge $MAX_WAIT ]]; then
        printf "\033[%dA\033[J" $TABLE_H
        fail "Timed out waiting for services (${MAX_WAIT}s)."
        echo ""
        docker compose $COMPOSE_FILES ps
        echo ""
        fail "Check logs: docker compose $COMPOSE_FILES logs --tail=50"
        exit 1
    fi
    sleep $INTERVAL
    ELAPSED=$((ELAPSED + INTERVAL))
    printf "\033[%dA" $TABLE_H
    _print_health_table $ELAPSED $MAX_WAIT $SPIN_I
    SPIN_I=$((SPIN_I+1))
done
printf "\033[%dA\033[J" $TABLE_H

# ── Show status ──────────────────────────────────────────────────
ok "All services are healthy!"
echo ""
docker compose $COMPOSE_FILES ps --format "table {{.Name}}\t{{.Status}}\t{{.Ports}}"
echo ""
ok "Dashboard:  http://localhost:3000"
ok "API docs:   http://localhost:8088/docs"
ok "Health:     http://localhost:3000/api/v1/health"
echo ""
