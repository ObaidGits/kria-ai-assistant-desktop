#!/usr/bin/env bash
# Setup Python environment for KRIA's Pre-Cognitive sidecar.
# Installs uv (if needed), creates a virtualenv, syncs dependencies,
# and runs the sidecar self-test.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
KRIA_MODULES="$PROJECT_ROOT/kria-modules"
VENV_DIR="$HOME/.kria/python-env"

echo "=== KRIA Python Sidecar Setup ==="

# ── 1. Check Python ≥ 3.11 ──────────────────────────────────
PYTHON=""
for cmd in python3.12 python3.11 python3; do
    if command -v "$cmd" &>/dev/null; then
        ver=$("$cmd" -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')
        major=$(echo "$ver" | cut -d. -f1)
        minor=$(echo "$ver" | cut -d. -f2)
        if [ "$major" -ge 3 ] && [ "$minor" -ge 11 ]; then
            PYTHON="$cmd"
            break
        fi
    fi
done

if [ -z "$PYTHON" ]; then
    echo "ERROR: Python >= 3.11 required but not found."
    echo "Install Python 3.11+ and retry."
    exit 1
fi
echo "Using Python: $PYTHON ($($PYTHON --version))"

# ── 2. Install uv (fast Python package installer) ───────────
if ! command -v uv &>/dev/null; then
    echo "Installing uv..."
    curl -LsSf https://astral.sh/uv/install.sh | sh
    export PATH="$HOME/.cargo/bin:$PATH"
fi
echo "uv: $(uv --version)"

# ── 3. Create virtual environment ───────────────────────────
if [ ! -d "$VENV_DIR" ]; then
    echo "Creating virtual environment at $VENV_DIR..."
    uv venv "$VENV_DIR" --python "$PYTHON"
fi

# Activate
# shellcheck disable=SC1091
source "$VENV_DIR/bin/activate"
echo "Virtualenv: $VENV_DIR"

# ── 4. Install kria-modules ─────────────────────────────────
echo "Installing kria-modules..."
cd "$KRIA_MODULES"

# Install in editable mode with uv
uv pip install -e "."

# Optional: install GPU extras if NVIDIA GPU detected
if command -v nvidia-smi &>/dev/null; then
    echo "NVIDIA GPU detected — installing GPU extras..."
    uv pip install -e ".[gpu]" || echo "GPU extras install failed (non-fatal)"
fi

# ── 5. Run self-test ────────────────────────────────────────
echo ""
echo "Running sidecar self-test..."
if python -m kria_modules.bridge --selftest; then
    echo ""
    echo "=== Setup Complete ==="
    echo "Sidecar executable: $VENV_DIR/bin/kria-sidecar"
    echo "Python env: $VENV_DIR"
else
    echo ""
    echo "WARNING: Self-test failed. Some processors may be unavailable."
    echo "This is usually due to missing system packages (e.g. tesseract-ocr)."
    echo "The sidecar will still work with available processors."
fi
