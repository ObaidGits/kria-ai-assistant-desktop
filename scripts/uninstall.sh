#!/usr/bin/env bash
# ============================================================
# K.R.I.A. — Uninstall Script (Linux / macOS)
# Removes build artifacts, config, and optionally dependencies.
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; NC='\033[0m'
info()  { printf "${CYAN}[INFO]${NC}  %s\n" "$*"; }
ok()    { printf "${GREEN}[OK]${NC}    %s\n" "$*"; }
warn()  { printf "${YELLOW}[WARN]${NC}  %s\n" "$*"; }

REMOVE_CONFIG=false
REMOVE_TOOLCHAINS=false

# ── Parse flags ──────────────────────────────────────────────
for arg in "$@"; do
  case "$arg" in
    --all)          REMOVE_CONFIG=true; REMOVE_TOOLCHAINS=true ;;
    --config)       REMOVE_CONFIG=true ;;
    --toolchains)   REMOVE_TOOLCHAINS=true ;;
    --help|-h)
      echo "Usage: uninstall.sh [OPTIONS]"
      echo ""
      echo "Options:"
      echo "  (none)        Remove build artifacts only (target/, node_modules/)"
      echo "  --config      Also remove ~/.kria/ config directory"
      echo "  --toolchains  Also remove cargo-tauri"
      echo "  --all         Remove everything above"
      echo "  -h, --help    Show this help"
      exit 0
      ;;
    *) warn "Unknown flag: $arg (ignored)" ;;
  esac
done

echo ""
info "K.R.I.A. Uninstall"
echo ""

# ── 1. Rust build artifacts ─────────────────────────────────
if [[ -d "$PROJECT_ROOT/target" ]]; then
  info "Removing Rust build artifacts (target/)…"
  rm -rf "$PROJECT_ROOT/target"
  ok "target/ removed"
else
  ok "target/ already clean"
fi

# ── 2. Frontend node_modules ────────────────────────────────
if [[ -d "$PROJECT_ROOT/ui/node_modules" ]]; then
  info "Removing ui/node_modules/…"
  rm -rf "$PROJECT_ROOT/ui/node_modules"
  ok "ui/node_modules/ removed"
else
  ok "ui/node_modules/ already clean"
fi

if [[ -d "$PROJECT_ROOT/ui/dist" ]]; then
  info "Removing ui/dist/…"
  rm -rf "$PROJECT_ROOT/ui/dist"
  ok "ui/dist/ removed"
fi

# ── 3. Config directory ─────────────────────────────────────
KRIA_HOME="$HOME/.kria"
if $REMOVE_CONFIG; then
  if [[ -d "$KRIA_HOME" ]]; then
    info "Removing config directory ($KRIA_HOME)…"
    rm -rf "$KRIA_HOME"
    ok "$KRIA_HOME removed"
  else
    ok "$KRIA_HOME already clean"
  fi
else
  info "Keeping $KRIA_HOME (use --config or --all to remove)"
fi

# ── 4. Tauri CLI ────────────────────────────────────────────
if $REMOVE_TOOLCHAINS; then
  if command -v cargo-tauri &>/dev/null; then
    info "Removing cargo-tauri…"
    cargo uninstall tauri-cli 2>/dev/null || true
    ok "cargo-tauri removed"
  else
    ok "cargo-tauri not installed"
  fi
else
  info "Keeping cargo-tauri (use --toolchains or --all to remove)"
fi

# ── Done ─────────────────────────────────────────────────────
echo ""
printf "${GREEN}Uninstall complete.${NC}\n"
echo ""
