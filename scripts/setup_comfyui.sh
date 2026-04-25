#!/usr/bin/env bash
# setup_comfyui.sh — Install ComfyUI sidecar for KRIA image generation (Tier B/A/S)
#
# Installs into: ~/.kria/comfyui/
#   ├── ComfyUI/          ← git clone
#   ├── .venv/            ← Python venv with PyTorch CUDA + ComfyUI deps
#   └── models/           ← model weights (populated by download_models.py --comfyui)
#
# Compatibility goals (read this before "fixing" anything):
#   * Works on any modern Linux x86_64 with an NVIDIA driver >= 525.
#   * Auto-picks the right PyTorch CUDA wheel index from the installed driver
#     (cu118 / cu121 / cu124), with documented fallbacks.
#   * Auto-picks the best Python interpreter for the venv (3.12 > 3.11 > 3.10
#     > system python3). PyTorch wheels for cu121 are NOT published for 3.13,
#     so the script will refuse to use 3.13 unless the user opts in.
#   * Re-runnable: every step is idempotent. Partial / broken venvs are
#     repaired in place instead of failing with cryptic errors.
#   * Does NOT install torchaudio. ComfyUI image generation does not need it,
#     and including it makes installs fragile (torchaudio wheels lag behind
#     torch on every new Python release).
#
# Environment overrides:
#   KRIA_DATA_DIR=/custom/path              # Default ~/.kria
#   CUDA_VERSION=cu118|cu121|cu124          # Default: auto-detect from driver
#   COMFY_PYTHON=/path/to/python3.12        # Default: auto-pick best 3.10-3.12
#   SKIP_TORCH=1                            # Don't (re)install PyTorch
#   FORCE_PY313=1                           # Allow Python 3.13 (use at your own risk)
#   REBUILD_VENV=1                          # Wipe .venv/ and start clean
#   SKIP_SMOKE=1                            # Don't run the 120s smoke test
#
# Usage:
#   bash scripts/setup_comfyui.sh
#
# After setup, download Flux.1-schnell models with:
#   python scripts/download_models.py --comfyui

set -euo pipefail

# ── Config ────────────────────────────────────────────────────────────────────
KRIA_DATA_DIR="${KRIA_DATA_DIR:-${HOME}/.kria}"
COMFY_DIR="${KRIA_DATA_DIR}/comfyui"
COMFY_REPO="${COMFY_DIR}/ComfyUI"
COMFY_VENV="${COMFY_DIR}/.venv"
COMFY_MODELS="${COMFY_DIR}/models"
COMFY_GIT_URL="https://github.com/comfyanonymous/ComfyUI.git"
COMFY_GIT_REF="master"
SKIP_TORCH="${SKIP_TORCH:-0}"
FORCE_PY313="${FORCE_PY313:-0}"
REBUILD_VENV="${REBUILD_VENV:-0}"
SKIP_SMOKE="${SKIP_SMOKE:-0}"

# ── Colour helpers ────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; NC='\033[0m'
info()  { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()    { echo -e "${GREEN}[OK]${NC}    $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
die()   { echo -e "${RED}[ERROR]${NC} $*" >&2; exit 1; }

# ── Preflight ─────────────────────────────────────────────────────────────────
info "KRIA ComfyUI Setup"
echo "  Install dir : ${COMFY_DIR}"
echo

# Verify GPU
if ! command -v nvidia-smi &>/dev/null; then
    die "nvidia-smi not found. Install NVIDIA proprietary drivers first."
fi
GPU_LINE=$(nvidia-smi --query-gpu=name,memory.total --format=csv,noheader 2>/dev/null | head -1) || true
if [[ -z "${GPU_LINE}" ]]; then
    die "No NVIDIA GPU detected by nvidia-smi. Check driver installation."
fi
ok "GPU detected: ${GPU_LINE}"

# ── Driver-aware CUDA wheel selection ────────────────────────────────────────
# PyTorch wheel indexes (as of 2026-04):
#   cu124  — driver >= 550
#   cu121  — driver >= 525
#   cu118  — driver >= 450 (fallback for old laptops / cloud images)
#
# We pick the highest index supported by the installed driver. Users can
# override with CUDA_VERSION=cu118 etc. for forced compatibility.
DRIVER_VER=$(nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null | head -1 | tr -d ' ')
DRIVER_MAJOR="${DRIVER_VER%%.*}"
[[ -z "${DRIVER_MAJOR}" ]] && DRIVER_MAJOR=0

if [[ -n "${CUDA_VERSION:-}" ]]; then
    info "CUDA_VERSION override: ${CUDA_VERSION}"
elif [[ "${DRIVER_MAJOR}" -ge 550 ]]; then
    CUDA_VERSION="cu124"
elif [[ "${DRIVER_MAJOR}" -ge 525 ]]; then
    CUDA_VERSION="cu121"
elif [[ "${DRIVER_MAJOR}" -ge 450 ]]; then
    CUDA_VERSION="cu118"
else
    warn "NVIDIA driver ${DRIVER_VER} is older than 450 — forcing cu118 (may fail)."
    CUDA_VERSION="cu118"
fi
ok "Selected PyTorch wheel index: ${CUDA_VERSION} (driver ${DRIVER_VER})"

# ── Python interpreter selection ─────────────────────────────────────────────
# PyTorch publishes wheels for Python 3.10, 3.11, 3.12 across all CUDA
# variants. Python 3.13 wheels are CUDA-12.4 only and lag behind on every
# release. We prefer 3.12 (latest fully-supported), then fall back.
pick_python() {
    if [[ -n "${COMFY_PYTHON:-}" ]]; then
        if [[ -x "${COMFY_PYTHON}" ]]; then
            echo "${COMFY_PYTHON}"
            return 0
        fi
        die "COMFY_PYTHON=${COMFY_PYTHON} is not an executable file."
    fi
    for candidate in python3.12 python3.11 python3.10; do
        if command -v "${candidate}" &>/dev/null; then
            command -v "${candidate}"
            return 0
        fi
    done
    # Last resort: system python3 (only used if it's 3.10-3.12).
    if command -v python3 &>/dev/null; then
        local sysver
        sysver=$(python3 -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")' 2>/dev/null || echo "0.0")
        case "${sysver}" in
            3.10|3.11|3.12) command -v python3; return 0 ;;
            3.13)
                if [[ "${FORCE_PY313}" == "1" ]]; then
                    warn "Using Python 3.13 because FORCE_PY313=1 (PyTorch wheel coverage is incomplete)."
                    command -v python3
                    return 0
                fi
                return 1
                ;;
        esac
    fi
    return 1
}

PYTHON=$(pick_python || true)
if [[ -z "${PYTHON}" ]]; then
    die "Could not find a usable Python (need 3.10, 3.11, or 3.12).
        Install one with:
          Ubuntu/Debian:  sudo apt install python3.12 python3.12-venv
          Conda:          conda create -n kria-comfy python=3.12 && conda activate kria-comfy
          pyenv:          pyenv install 3.12.7 && pyenv shell 3.12.7
        Or, to force-use your existing Python 3.13 (may fail on torchvision):
          FORCE_PY313=1 bash scripts/setup_comfyui.sh"
fi

PY_VERSION=$("${PYTHON}" -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')
PY_MAJOR="${PY_VERSION%%.*}"
PY_MINOR="${PY_VERSION##*.}"
if [[ "${PY_MAJOR}" -lt 3 ]] || [[ "${PY_MAJOR}" -eq 3 && "${PY_MINOR}" -lt 10 ]]; then
    die "Python 3.10+ required (found ${PY_VERSION})."
fi
ok "Python ${PY_VERSION} at ${PYTHON}"

# Verify git/curl
command -v git  &>/dev/null || die "git not found. Install git."
command -v curl &>/dev/null || die "curl not found. Install curl."

# ── 1. Create directory layout ────────────────────────────────────────────────
info "Creating directory layout under ${COMFY_DIR} ..."
mkdir -p \
    "${COMFY_DIR}" \
    "${COMFY_MODELS}/unet" \
    "${COMFY_MODELS}/clip" \
    "${COMFY_MODELS}/vae" \
    "${COMFY_MODELS}/checkpoints" \
    "${COMFY_MODELS}/loras" \
    "${COMFY_DIR}/output"
ok "Directory layout ready"

# ── 2. Clone or update ComfyUI ────────────────────────────────────────────────
if [[ -d "${COMFY_REPO}/.git" ]]; then
    info "ComfyUI already cloned — pulling latest ${COMFY_GIT_REF} ..."
    git -C "${COMFY_REPO}" fetch --quiet origin
    git -C "${COMFY_REPO}" reset --hard "origin/${COMFY_GIT_REF}" --quiet
    ok "ComfyUI updated"
else
    info "Cloning ComfyUI from ${COMFY_GIT_URL} ..."
    git clone --depth 1 --branch "${COMFY_GIT_REF}" "${COMFY_GIT_URL}" "${COMFY_REPO}"
    ok "ComfyUI cloned"
fi

patch_comfyui_audio_vae() {
    local audio_vae_file="${COMFY_REPO}/comfy/ldm/lightricks/vae/audio_vae.py"
    if [[ ! -f "${audio_vae_file}" ]]; then
        warn "Audio VAE module not found at ${audio_vae_file}"
        return 0
    fi

    if grep -q "def _require_torchaudio() -> None:" "${audio_vae_file}"; then
        return 0
    fi

    info "Patching ComfyUI audio VAE import so image startup does not depend on torchaudio ..."
    "${PYTHON}" - "${audio_vae_file}" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
text = path.read_text()

text = text.replace(
    "import torch\nimport torchaudio\n",
    "import torch\n\ntry:\n    import torchaudio\nexcept Exception:  # noqa: BLE001 - torchaudio wheels can be missing or CUDA-mismatched\n    torchaudio = None\n",
    1,
)

marker = "LATENT_DOWNSAMPLE_FACTOR = 4\n\n\n"
replacement = "LATENT_DOWNSAMPLE_FACTOR = 4\n\n\ndef _require_torchaudio() -> None:\n    if torchaudio is None:\n        raise RuntimeError(\n            \"torchaudio is required only for audio VAE features; it is not needed for image generation.\"\n        )\n\n\n"
if marker in text and "def _require_torchaudio() -> None:" not in text:
    text = text.replace(marker, replacement, 1)

text = text.replace(
    "    def resample(self, waveform: torch.Tensor, source_rate: int) -> torch.Tensor:\n        if source_rate == self.target_sample_rate:\n            return waveform\n",
    "    def resample(self, waveform: torch.Tensor, source_rate: int) -> torch.Tensor:\n        _require_torchaudio()\n        if source_rate == self.target_sample_rate:\n            return waveform\n",
    1,
)
text = text.replace(
    "    def waveform_to_mel(\n        self, waveform: torch.Tensor, waveform_sample_rate: int, device\n    ) -> torch.Tensor:\n        waveform = self.resample(waveform, waveform_sample_rate)\n",
    "    def waveform_to_mel(\n        self, waveform: torch.Tensor, waveform_sample_rate: int, device\n    ) -> torch.Tensor:\n        _require_torchaudio()\n        waveform = self.resample(waveform, waveform_sample_rate)\n",
    1,
)

path.write_text(text)
PY
}

patch_comfyui_audio_vae

# ── 3. Create / repair Python venv ───────────────────────────────────────────
# Two failure modes we explicitly defend against:
#   (a) Existing venv was built with a different Python (e.g. user upgraded
#       to 3.13). pip will then fail with cryptic ABI / wheel errors.
#       → Detect Python mismatch and rebuild.
#   (b) REBUILD_VENV=1 forces a clean rebuild.
needs_rebuild=0
if [[ -d "${COMFY_VENV}" ]]; then
    if [[ "${REBUILD_VENV}" == "1" ]]; then
        warn "REBUILD_VENV=1 — wiping ${COMFY_VENV}"
        needs_rebuild=1
    elif [[ -x "${COMFY_VENV}/bin/python" ]]; then
        existing_ver=$("${COMFY_VENV}/bin/python" -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")' 2>/dev/null || echo "?")
        if [[ "${existing_ver}" != "${PY_VERSION}" ]]; then
            warn "Existing venv uses Python ${existing_ver}, but selected interpreter is ${PY_VERSION}. Rebuilding."
            needs_rebuild=1
        fi
    else
        warn "Existing venv is broken (no bin/python). Rebuilding."
        needs_rebuild=1
    fi
fi
if [[ "${needs_rebuild}" == "1" ]]; then
    rm -rf "${COMFY_VENV}"
fi
if [[ ! -d "${COMFY_VENV}" ]]; then
    info "Creating Python venv at ${COMFY_VENV} (using ${PYTHON}) ..."
    "${PYTHON}" -m venv "${COMFY_VENV}" || die "venv creation failed.
        On Debian/Ubuntu you may need:  sudo apt install python${PY_VERSION}-venv"
    ok "Venv created"
else
    ok "Venv already exists at ${COMFY_VENV} (Python ${PY_VERSION})"
fi

VENV_PY="${COMFY_VENV}/bin/python"
VENV_PIP="${COMFY_VENV}/bin/pip"

# Sanity-check pip exists in the venv (some distros split it out).
if [[ ! -x "${VENV_PIP}" ]]; then
    info "pip missing in venv — bootstrapping with ensurepip ..."
    "${VENV_PY}" -m ensurepip --upgrade || die "ensurepip failed; install python${PY_VERSION}-pip on your distro"
fi

# ── 4. Upgrade pip ───────────────────────────────────────────────────────────
info "Upgrading pip / wheel / setuptools ..."
"${VENV_PIP}" install --quiet --upgrade pip wheel setuptools

# ── 5. Install PyTorch with CUDA ──────────────────────────────────────────────
# We install ONLY torch + torchvision. torchaudio is not used anywhere in
# the image generation path; including it has historically been the #1
# source of "no matching distribution" errors on new Python releases.
if [[ "${SKIP_TORCH}" == "1" ]]; then
    warn "SKIP_TORCH=1 — skipping PyTorch install"
else
    install_torch_from_index() {
        local idx="$1"
        local url="https://download.pytorch.org/whl/${idx}"
        info "Installing torch + torchvision (${idx}) from ${url} ..."
        "${VENV_PIP}" install --upgrade torch torchvision --index-url "${url}"
    }

    # Try the auto-selected index first, then fall back to cu118 (which has
    # the broadest Python/driver coverage), then to the default PyPI index
    # (CPU-only torch — better than nothing for Tier-A SDXL on CPU).
    if install_torch_from_index "${CUDA_VERSION}"; then
        ok "PyTorch (${CUDA_VERSION}) installed"
    else
        warn "PyTorch ${CUDA_VERSION} install failed for Python ${PY_VERSION}."
        if [[ "${CUDA_VERSION}" != "cu118" ]] && install_torch_from_index "cu118"; then
            ok "PyTorch (cu118 fallback) installed"
            CUDA_VERSION="cu118"
        else
            warn "All CUDA wheel indexes failed. Falling back to CPU-only PyTorch."
            warn "Image generation will be VERY slow without GPU acceleration."
            "${VENV_PIP}" install --upgrade torch torchvision || \
                die "PyTorch install failed completely. Try:  REBUILD_VENV=1 COMFY_PYTHON=\$(which python3.12) bash scripts/setup_comfyui.sh"
            CUDA_VERSION="cpu"
        fi
    fi
fi

# ── 6. Install ComfyUI dependencies ──────────────────────────────────────────
info "Installing ComfyUI requirements ..."
"${VENV_PIP}" install -r "${COMFY_REPO}/requirements.txt" || \
    die "ComfyUI requirements failed. Re-run with REBUILD_VENV=1 to start clean."

if "${VENV_PIP}" show torchaudio >/dev/null 2>&1; then
    warn "Removing torchaudio from the venv — image generation does not use it, and mismatched CUDA wheels can break startup."
    "${VENV_PIP}" uninstall -y torchaudio >/dev/null 2>&1 || true
fi
ok "ComfyUI requirements installed"

# ── 7. Install ComfyUI-GGUF custom node (for Flux GGUF models) ───────────────
GGUF_NODE_DIR="${COMFY_REPO}/custom_nodes/ComfyUI-GGUF"
if [[ -d "${GGUF_NODE_DIR}/.git" ]]; then
    info "ComfyUI-GGUF node already installed — pulling latest ..."
    git -C "${GGUF_NODE_DIR}" pull --quiet || warn "git pull failed; continuing with existing checkout"
else
    info "Installing ComfyUI-GGUF custom node ..."
    git clone --depth 1 \
        "https://github.com/city96/ComfyUI-GGUF.git" \
        "${GGUF_NODE_DIR}"
fi
if [[ -f "${GGUF_NODE_DIR}/requirements.txt" ]]; then
    "${VENV_PIP}" install -r "${GGUF_NODE_DIR}/requirements.txt" || \
        warn "GGUF node deps failed; the Flux GGUF workflow will not load."
fi
ok "ComfyUI-GGUF node ready"

# ── 8. Validate PyTorch CUDA ─────────────────────────────────────────────────
info "Validating PyTorch CUDA availability ..."
CUDA_OK=$("${VENV_PY}" -c "import torch; print('yes' if torch.cuda.is_available() else 'no')" 2>/dev/null || echo "error")
if [[ "${CUDA_OK}" == "yes" ]]; then
    GPU_NAME=$("${VENV_PY}" -c "import torch; print(torch.cuda.get_device_name(0))" 2>/dev/null || echo "unknown")
    TORCH_VER=$("${VENV_PY}" -c "import torch; print(torch.__version__)" 2>/dev/null || echo "?")
    ok "PyTorch ${TORCH_VER} CUDA available — GPU: ${GPU_NAME}"
elif [[ "${CUDA_OK}" == "no" ]]; then
    warn "PyTorch installed but CUDA is NOT available."
    warn "Driver ${DRIVER_VER} may not match wheel index ${CUDA_VERSION}."
    warn "Try:  CUDA_VERSION=cu118 REBUILD_VENV=1 bash scripts/setup_comfyui.sh"
else
    warn "Could not validate PyTorch CUDA (import error)."
fi

# ── 9. Write sidecar marker file ──────────────────────────────────────────────
MARKER="${COMFY_DIR}/.kria-setup-done"
cat > "${MARKER}" <<EOF
setup_date=$(date -u +%Y-%m-%dT%H:%M:%SZ)
cuda_version=${CUDA_VERSION}
python_version=${PY_VERSION}
python_path=${PYTHON}
comfyui_ref=${COMFY_GIT_REF}
driver_version=${DRIVER_VER}
EOF
ok "Marker written to ${MARKER}"

# ── 10. Smoke test — launch and probe /system_stats ──────────────────────────
if [[ "${SKIP_SMOKE}" == "1" ]]; then
    info "SKIP_SMOKE=1 — skipping smoke test"
else
    info "Running smoke test (launch ComfyUI, probe /system_stats, then stop) ..."

    PORT=8188
    SMOKE_TIMEOUT=120  # seconds — cold cache + custom node import can take 90s+

    # Kill any existing ComfyUI on this port (best-effort).
    if command -v lsof &>/dev/null; then
        EXISTING_PID=$(lsof -ti "tcp:${PORT}" 2>/dev/null || true)
        if [[ -n "${EXISTING_PID}" ]]; then
            warn "Port ${PORT} in use by PID ${EXISTING_PID} — killing it ..."
            kill "${EXISTING_PID}" 2>/dev/null || true
            sleep 1
        fi
    fi

    # Launch in background. Mirror the Rust sidecar's exact arg list so the
    # smoke test catches arg-compatibility regressions like
    # "--cache-classic + --cache-lru" mutual exclusion.
    "${VENV_PY}" -m main \
        --listen 127.0.0.1 \
        --port "${PORT}" \
        --disable-auto-launch \
        --dont-print-server \
        --output-directory "${COMFY_DIR}/output" \
        --cache-lru 5 \
        > "${COMFY_DIR}/smoke_test.log" 2>&1 &
    SMOKE_PID=$!
    info "ComfyUI started (PID ${SMOKE_PID}) — waiting up to ${SMOKE_TIMEOUT}s ..."

    SMOKE_OK=0
    for i in $(seq 1 "${SMOKE_TIMEOUT}"); do
        sleep 1
        if curl -sf "http://127.0.0.1:${PORT}/system_stats" >/dev/null 2>&1; then
            SMOKE_OK=1
            break
        fi
        # Detect early crash.
        if ! kill -0 "${SMOKE_PID}" 2>/dev/null; then
            warn "ComfyUI process exited early. Tail of smoke_test.log:"
            tail -30 "${COMFY_DIR}/smoke_test.log" | sed 's/^/    /' >&2
            break
        fi
    done

    # Stop the smoke-test process.
    kill "${SMOKE_PID}" 2>/dev/null || true
    wait "${SMOKE_PID}" 2>/dev/null || true

    if [[ "${SMOKE_OK}" -eq 1 ]]; then
        ok "Smoke test passed — ComfyUI responded on http://127.0.0.1:${PORT}/system_stats"
    else
        warn "Smoke test FAILED — ComfyUI did not respond within ${SMOKE_TIMEOUT}s."
        warn "Full log: ${COMFY_DIR}/smoke_test.log"
        warn "Setup is complete but ComfyUI may not start correctly."
    fi
fi

# ── Summary ───────────────────────────────────────────────────────────────────
echo
echo -e "${GREEN}══════════════════════════════════════════════════${NC}"
echo -e "${GREEN}  ComfyUI setup complete${NC}"
echo -e "${GREEN}══════════════════════════════════════════════════${NC}"
echo
echo "  Install  : ${COMFY_DIR}"
echo "  Python   : ${VENV_PY}  (${PY_VERSION})"
echo "  PyTorch  : ${CUDA_VERSION}"
echo "  Models   : ${COMFY_MODELS}"
echo
echo "Next step — download Flux.1-schnell models:"
echo "  python scripts/download_models.py --comfyui"
echo
echo "Then test a full generation:"
echo "  KRIA_IMAGE_MODE=local_only cargo run -p kria-desktop"
echo
