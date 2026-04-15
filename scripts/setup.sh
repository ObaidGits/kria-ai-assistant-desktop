#!/usr/bin/env bash
# ============================================================
# K.R.I.A. — Setup Script (Linux / macOS)
# Idempotent: safe to re-run at any time.
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ── Colours ──────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; NC='\033[0m'
info()  { printf "${CYAN}[INFO]${NC}  %s\n" "$*"; }
ok()    { printf "${GREEN}[OK]${NC}    %s\n" "$*"; }
warn()  { printf "${YELLOW}[WARN]${NC}  %s\n" "$*"; }
fail()  { printf "${RED}[FAIL]${NC}  %s\n" "$*"; exit 1; }

# ── Detect platform ─────────────────────────────────────────
OS="$(uname -s)"
ARCH="$(uname -m)"
info "Detected platform: $OS $ARCH"

case "$OS" in
  Linux)  PKG_MGR="apt" ;;
  Darwin) PKG_MGR="brew" ;;
  *)      fail "Unsupported OS: $OS. Use setup.ps1 on Windows." ;;
esac

# ── Helper: check if a command exists ────────────────────────
has() { command -v "$1" &>/dev/null; }

# ── 1. System dependencies ──────────────────────────────────
info "Step 1/6 — Installing system dependencies…"

if [[ "$PKG_MGR" == "apt" ]]; then
  PKGS=(
    build-essential pkg-config curl git
    libssl-dev
    libasound2-dev
    libwebkit2gtk-4.1-dev
    libgtk-3-dev
    librsvg2-dev
    patchelf
  )
  MISSING=()
  for pkg in "${PKGS[@]}"; do
    if ! dpkg -s "$pkg" &>/dev/null; then
      MISSING+=("$pkg")
    fi
  done
  if [[ ${#MISSING[@]} -gt 0 ]]; then
    info "Installing: ${MISSING[*]}"
    sudo apt-get update -qq
    sudo apt-get install -y -qq "${MISSING[@]}"
  fi
  ok "System packages ready (apt)"

elif [[ "$PKG_MGR" == "brew" ]]; then
  if ! has brew; then
    fail "Homebrew not found. Install from https://brew.sh"
  fi
  # macOS: Xcode CLI tools usually provide build essentials
  BREW_PKGS=(pkg-config openssl@3)
  for pkg in "${BREW_PKGS[@]}"; do
    if ! brew list "$pkg" &>/dev/null; then
      brew install "$pkg"
    fi
  done
  ok "System packages ready (brew)"
fi

# ── 2. Rust toolchain ───────────────────────────────────────
info "Step 2/6 — Checking Rust toolchain…"

if has rustup; then
  ok "rustup already installed ($(rustup --version 2>/dev/null | head -1))"
else
  info "Installing Rust via rustup…"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
fi

# Ensure cargo is on PATH for the rest of this script
export PATH="$HOME/.cargo/bin:$PATH"

if ! has cargo; then
  fail "cargo not found after Rust install. Try: source \$HOME/.cargo/env"
fi

# Make sure we're on stable and up to date
rustup default stable
rustup update stable --no-self-update 2>/dev/null || true
ok "Rust $(rustc --version | awk '{print $2}') ready"

# ── 3. Install Tauri CLI ────────────────────────────────────
info "Step 3/6 — Checking Tauri CLI…"

if has cargo-tauri; then
  ok "cargo-tauri already installed"
else
  info "Installing cargo-tauri (this may take a few minutes)…"
  cargo install tauri-cli --version "^2" --locked
  ok "cargo-tauri installed"
fi

# ── 4. Node.js ──────────────────────────────────────────────
info "Step 4/6 — Checking Node.js…"

MIN_NODE=18
if has node; then
  NODE_VER="$(node -v | sed 's/v//' | cut -d. -f1)"
  if [[ "$NODE_VER" -ge "$MIN_NODE" ]]; then
    ok "Node.js $(node -v) ready"
  else
    warn "Node.js $(node -v) is below v$MIN_NODE — please upgrade"
  fi
else
  info "Node.js not found. Attempting install…"
  if [[ "$PKG_MGR" == "apt" ]]; then
    # Use NodeSource for a modern version
    if [[ ! -f /etc/apt/sources.list.d/nodesource.list ]]; then
      curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
    fi
    sudo apt-get install -y -qq nodejs
  elif [[ "$PKG_MGR" == "brew" ]]; then
    brew install node
  fi
  ok "Node.js $(node -v) installed"
fi

if ! has npm; then
  fail "npm not found. Please install Node.js manually."
fi

# ── 5. Frontend dependencies ────────────────────────────────
info "Step 5/6 — Installing frontend dependencies…"

cd "$PROJECT_ROOT/ui"
if [[ -d node_modules ]]; then
  ok "node_modules exists — running npm install to sync"
fi
npm install --no-audit --no-fund
ok "Frontend dependencies ready"

# ── 6. Build workspace ──────────────────────────────────────
info "Step 6/6 — Building Rust workspace…"

cd "$PROJECT_ROOT"
cargo build --workspace
ok "Workspace built successfully"

# ── 7. Config ────────────────────────────────────────────────
KRIA_HOME="$HOME/.kria"
if [[ ! -f "$KRIA_HOME/config.toml" ]]; then
  mkdir -p "$KRIA_HOME"
  cp "$PROJECT_ROOT/config/default.toml" "$KRIA_HOME/config.toml"
  ok "Default config copied to $KRIA_HOME/config.toml"
else
  ok "Config already exists at $KRIA_HOME/config.toml"
fi

# ── Done ─────────────────────────────────────────────────────
echo ""
printf "${GREEN}════════════════════════════════════════════${NC}\n"
printf "${GREEN}  K.R.I.A. setup complete!${NC}\n"
printf "${GREEN}════════════════════════════════════════════${NC}\n"
echo ""
echo "Quick start:"
echo "  Desktop app :  cd crates/kria-desktop && cargo tauri dev"
echo "  Server only :  cargo run -p kria-server"
echo "  Run tests   :  cargo test --workspace"
echo ""
