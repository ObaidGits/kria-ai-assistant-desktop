#!/usr/bin/env bash
# ============================================================
# K.R.I.A. — Production Release Build (Linux / macOS)
# Produces platform-native bundles via Tauri.
# Output: crates/kria-desktop/target/release/bundle/
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; NC='\033[0m'
info()  { printf "${CYAN}[INFO]${NC}  %s\n" "$*"; }
ok()    { printf "${GREEN}[OK]${NC}    %s\n" "$*"; }
fail()  { printf "${RED}[FAIL]${NC}  %s\n" "$*"; exit 1; }
has()   { command -v "$1" &>/dev/null; }

OS="$(uname -s)"
info "Building KRIA release for $OS $(uname -m)"

# ── Pre-flight checks ───────────────────────────────────────
export PATH="$HOME/.cargo/bin:$PATH"

has cargo   || fail "cargo not found. Run scripts/setup.sh first."
has node    || fail "node not found. Run scripts/setup.sh first."
has npm     || fail "npm not found. Run scripts/setup.sh first."

# Check tauri-cli
if ! has cargo-tauri; then
  fail "cargo-tauri not found. Run: cargo install tauri-cli --version '^2' --locked"
fi

# ── 1. Install / update frontend deps ───────────────────────
info "Step 1/3 — Frontend dependencies…"
cd "$PROJECT_ROOT/ui"
npm install --no-audit --no-fund
npm run build
ok "Frontend built (ui/dist/)"

# ── 2. Build the Tauri app ──────────────────────────────────
info "Step 2/3 — Building Tauri release (this may take several minutes)…"
cd "$PROJECT_ROOT/crates/kria-desktop"
cargo tauri build
ok "Tauri build finished"

# ── 3. Locate outputs ───────────────────────────────────────
info "Step 3/3 — Locating bundles…"
BUNDLE_DIR="$PROJECT_ROOT/target/release/bundle"

echo ""
printf "${GREEN}════════════════════════════════════════════${NC}\n"
printf "${GREEN}  Release build complete!${NC}\n"
printf "${GREEN}════════════════════════════════════════════${NC}\n"
echo ""

if [[ -d "$BUNDLE_DIR" ]]; then
  info "Bundles:"
  find "$BUNDLE_DIR" -maxdepth 2 -type f \( -name "*.deb" -o -name "*.AppImage" -o -name "*.dmg" -o -name "*.app" -o -name "*.rpm" \) 2>/dev/null | while read -r f; do
    SIZE=$(du -h "$f" | cut -f1)
    echo "  $SIZE  $f"
  done
else
  info "Bundle directory: $BUNDLE_DIR"
fi

echo ""
info "The standalone server can also be found at:"
echo "  $PROJECT_ROOT/target/release/kria-server"
echo ""
