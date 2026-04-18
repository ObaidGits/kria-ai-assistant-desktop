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
info "Step 1/4 — Frontend dependencies…"
cd "$PROJECT_ROOT/ui"
npm install --no-audit --no-fund
npm run build
ok "Frontend built (ui/dist/)"

# ── 2. Stage bundled resources ───────────────────────────────
info "Step 2/4 — Staging bundled resources…"
RESOURCES_DIR="$PROJECT_ROOT/crates/kria-desktop/resources"
mkdir -p "$RESOURCES_DIR"

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64)  LLAMA_ARCH="x64"; UV_ARCH="x86_64" ;;
  aarch64|arm64) LLAMA_ARCH="arm64"; UV_ARCH="aarch64" ;;
  *) fail "Unsupported architecture: $ARCH" ;;
esac

case "$OS" in
  Linux)  LLAMA_OS="linux"; UV_OS="linux" ;;
  Darwin) LLAMA_OS="macos"; UV_OS="macos" ;;
  *) fail "Unsupported OS for build: $OS" ;;
esac

# llama-server CPU binary
if [[ ! -f "$RESOURCES_DIR/llama-server" ]]; then
  info "  Downloading llama-server CPU binary…"
  LLAMA_TAG="b5300"
  LLAMA_URL="https://github.com/ggml-org/llama.cpp/releases/download/${LLAMA_TAG}/llama-${LLAMA_TAG}-bin-${LLAMA_OS}-${LLAMA_ARCH}.zip"
  LLAMA_TMP=$(mktemp -d)
  curl -fSL "$LLAMA_URL" -o "$LLAMA_TMP/llama.zip" || fail "Failed to download llama-server"
  unzip -q "$LLAMA_TMP/llama.zip" -d "$LLAMA_TMP/extract"
  find "$LLAMA_TMP/extract" -name "llama-server" -type f -exec cp {} "$RESOURCES_DIR/llama-server" \;
  chmod +x "$RESOURCES_DIR/llama-server"
  rm -rf "$LLAMA_TMP"
  ok "llama-server staged"
else
  ok "llama-server already present"
fi

# uv binary
if [[ ! -f "$RESOURCES_DIR/uv" ]]; then
  info "  Downloading uv binary…"
  UV_TAG="0.7.12"
  UV_URL="https://github.com/astral-sh/uv/releases/download/${UV_TAG}/uv-${UV_ARCH}-unknown-${UV_OS}-gnu.tar.gz"
  if [[ "$OS" == "Darwin" ]]; then
    UV_URL="https://github.com/astral-sh/uv/releases/download/${UV_TAG}/uv-${UV_ARCH}-apple-darwin.tar.gz"
  fi
  UV_TMP=$(mktemp -d)
  curl -fSL "$UV_URL" -o "$UV_TMP/uv.tar.gz" || fail "Failed to download uv"
  tar -xzf "$UV_TMP/uv.tar.gz" -C "$UV_TMP"
  find "$UV_TMP" -name "uv" -type f -exec cp {} "$RESOURCES_DIR/uv" \;
  chmod +x "$RESOURCES_DIR/uv"
  rm -rf "$UV_TMP"
  ok "uv staged"
else
  ok "uv already present"
fi

# kria-modules wheel
WHEEL_DIR="$PROJECT_ROOT/kria-modules/dist"
if [[ ! -f "$RESOURCES_DIR"/kria_modules*.whl ]] || [[ -d "$PROJECT_ROOT/kria-modules" ]]; then
  if has python3 && python3 -c "import build" 2>/dev/null; then
    info "  Building kria-modules wheel…"
    cd "$PROJECT_ROOT/kria-modules"
    python3 -m build --wheel --outdir "$WHEEL_DIR" 2>/dev/null || true
    WHEEL=$(find "$WHEEL_DIR" -name "kria_modules*.whl" 2>/dev/null | head -1)
    if [[ -n "$WHEEL" ]]; then
      cp "$WHEEL" "$RESOURCES_DIR/"
      ok "kria-modules wheel staged"
    else
      info "  Skipping kria-modules wheel (build failed — will install at runtime)"
    fi
  else
    info "  Skipping kria-modules wheel (python3 build module not available)"
  fi
fi

ok "Resources staged in $RESOURCES_DIR"

# ── 3. Build the Tauri app ──────────────────────────────────
info "Step 3/4 — Building Tauri release (this may take several minutes)…"
cd "$PROJECT_ROOT/crates/kria-desktop"

# Build TAURI_CONFIG override to include staged resources
EXTRA_RESOURCES='[]'
HAS_EXTRA=false
for f in "$RESOURCES_DIR"/llama-server* "$RESOURCES_DIR"/uv* "$RESOURCES_DIR"/kria_modules*.whl; do
  if [[ -f "$f" ]]; then
    BASENAME="$(basename "$f")"
    EXTRA_RESOURCES=$(echo "$EXTRA_RESOURCES" | python3 -c "import sys,json; r=json.load(sys.stdin); r.append('resources/$BASENAME'); print(json.dumps(r))" 2>/dev/null || echo "$EXTRA_RESOURCES")
    HAS_EXTRA=true
  fi
done

if $HAS_EXTRA; then
  export TAURI_CONFIG="{\"bundle\":{\"resources\":$EXTRA_RESOURCES}}"
  info "  Extra resources: $EXTRA_RESOURCES"
fi

cargo tauri build
ok "Tauri build finished"

# ── 4. Locate outputs ───────────────────────────────────────
info "Step 4/4 — Locating bundles…"
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
