#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────
# K.R.I.A. — Restart Services
# Rebuilds images to pick up code changes, clears ephemeral caches,
# and restarts all services.  Persistent data is PRESERVED:
#   ✓ Chat history & sessions  (kria-data volume — SQLite)
#   ✓ User preferences         (kria-data volume — SQLite)
#   ✓ Semantic memory           (kria-chroma-data volume)
#   ✓ Model config              (kria-data volume — /data/model_size)
#   ✓ Redis conversation cache  (kria-redis-data volume)
#
# Usage:
#   app-restart.sh              Rebuild & restart all services
#   app-restart.sh --quick      Restart without rebuilding images
#   app-restart.sh brain        Restart only kria-brain
#   app-restart.sh core         Restart only kria-core
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

QUICK=false
TARGET=""

for arg in "$@"; do
    case "$arg" in
        --quick) QUICK=true ;;
        brain|core|voice|data|dashboard)
            TARGET="kria-$arg" ;;
        kria-brain|kria-core|kria-voice|kria-data|kria-dashboard)
            TARGET="$arg" ;;
        *)
            fail "Unknown argument: $arg"
            echo "Usage: app-restart.sh [--quick] [brain|core|voice|data|dashboard]"
            exit 1 ;;
    esac
done

# ── Compose files ────────────────────────────────────────────────
COMPOSE_FILES="-f $COMPOSE_DIR/docker-compose.yml"
if [[ -f "$COMPOSE_DIR/docker-compose.override.yml" ]]; then
    COMPOSE_FILES="$COMPOSE_FILES -f $COMPOSE_DIR/docker-compose.override.yml"
fi
if nvidia-smi &>/dev/null && [[ -f "$COMPOSE_DIR/docker-compose.gpu.yml" ]]; then
    COMPOSE_FILES="$COMPOSE_FILES -f $COMPOSE_DIR/docker-compose.gpu.yml"
    info "GPU detected — using GPU compose override"
fi

# ── Single-service restart ───────────────────────────────────────
if [[ -n "$TARGET" ]]; then
    info "Restarting $TARGET..."
    if ! $QUICK; then
        info "Rebuilding $TARGET image..."
        docker compose $COMPOSE_FILES build "$TARGET" 2>/dev/null || \
            docker compose $COMPOSE_FILES build "${TARGET#kria-}" 2>/dev/null || true
    fi
    start_spinner "Restarting container $TARGET..."
    docker restart "$TARGET" >/dev/null 2>&1
    stop_spinner
    ok "$TARGET restarted."

    # Wait for health
    info "Waiting for $TARGET to become healthy..."
    for i in $(seq 1 60); do
        STATUS=$(docker inspect --format='{{.State.Health.Status}}' "$TARGET" 2>/dev/null || echo "starting")
        if [[ "$STATUS" == "healthy" ]]; then
            printf "\r\033[K"
            ok "$TARGET is healthy."
            exit 0
        fi
        printf "\r  \033[36m%s\033[0m %s  [%ds / 180s]  status: \033[33m%s\033[0m\033[K" \
            "${_SPIN[$((i%10))]}" "$TARGET" "$((i*3))" "$STATUS"
        sleep 3
    done
    printf "\r\033[K"
    warn "$TARGET did not become healthy within 180s. Check logs: docker logs $TARGET --tail=50"
    exit 1
fi

# ── Full restart ─────────────────────────────────────────────────
info "Restarting all KRIA services..."

# Stop containers but keep volumes
start_spinner "Stopping all containers (timeout: 15s)..."
docker compose $COMPOSE_FILES down --timeout 15 >/dev/null 2>&1
stop_spinner
ok "All containers stopped."

# Clear ephemeral cache (not persistent data)
if docker volume inspect kria-core-cache &>/dev/null; then
    docker volume rm kria-core-cache &>/dev/null || true
    ok "Cleared core cache volume."
fi

# Rebuild to pick up code changes
if ! $QUICK; then
    info "Rebuilding images (picking up latest code changes)..."
    docker compose $COMPOSE_FILES build
    # Prune old dangling images from previous builds
    docker image prune -f --filter "label=com.docker.compose.project=docker" &>/dev/null || true
    ok "Images rebuilt."
else
    info "Quick mode — skipping image rebuild."
fi

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
ok "Persistent data preserved (chat history, memory, preferences)."
echo ""
