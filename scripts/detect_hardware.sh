#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────
# K.R.I.A. — Hardware Detection & Tier Selection
# ─────────────────────────────────────────────────────────────────
# Sourced by setup.sh to auto-detect host hardware and select the
# optimal model tier.  Exports KRIA_* variables consumed by:
#   - setup.sh  (generates docker-compose.override.yml)
#   - download_models.py  (downloads tier-specific models)
#   - entrypoint.sh  (reads env vars set in override.yml)
#
# Manual override:  KRIA_TIER=performance  source detect_hardware.sh
# Re-detect:        KRIA_REDETECT=1       source detect_hardware.sh
# ─────────────────────────────────────────────────────────────────

_DH_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
_DH_CACHE_DIR="$HOME/.kria"
_DH_CACHE_FILE="$_DH_CACHE_DIR/hardware_tier.env"

# ── Use cached result unless re-detect requested ──────────────────
if [[ -z "${KRIA_TIER:-}" && "${KRIA_REDETECT:-0}" != "1" && -f "$_DH_CACHE_FILE" ]]; then
    # shellcheck disable=SC1090
    source "$_DH_CACHE_FILE"
    if [[ -n "${KRIA_TIER:-}" ]]; then
        return 0 2>/dev/null || exit 0
    fi
fi

# ── Detect RAM ────────────────────────────────────────────────────
# /proc/meminfo gives KiB; convert to GB (integer, rounded down)
if [[ -f /proc/meminfo ]]; then
    _MEM_KB=$(awk '/^MemTotal:/ {print $2}' /proc/meminfo)
    KRIA_TOTAL_RAM_GB=$(( _MEM_KB / 1048576 ))
else
    # macOS fallback
    KRIA_TOTAL_RAM_GB=$(( $(sysctl -n hw.memsize 2>/dev/null || echo 0) / 1073741824 ))
fi

# ── Detect GPU ────────────────────────────────────────────────────
KRIA_HAS_GPU=false
KRIA_GPU_VRAM_MB=0
KRIA_GPU_NAME="none"

if command -v nvidia-smi &>/dev/null; then
    _VRAM_LINE=$(nvidia-smi --query-gpu=memory.total,name --format=csv,noheader,nounits 2>/dev/null | head -1)
    if [[ -n "$_VRAM_LINE" ]]; then
        KRIA_HAS_GPU=true
        KRIA_GPU_VRAM_MB=$(echo "$_VRAM_LINE" | cut -d',' -f1 | tr -d ' ')
        KRIA_GPU_NAME=$(echo "$_VRAM_LINE" | cut -d',' -f2 | sed 's/^ *//')
    fi
fi

# ── Detect CPU ────────────────────────────────────────────────────
KRIA_CPU_THREADS=$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)

# ── Tier Selection ────────────────────────────────────────────────
# Allow manual override: KRIA_TIER=performance source detect_hardware.sh
if [[ -z "${KRIA_TIER:-}" ]]; then
    if [[ "$KRIA_HAS_GPU" == "true" && "$KRIA_GPU_VRAM_MB" -ge 8000 && "$KRIA_TOTAL_RAM_GB" -ge 16 ]]; then
        KRIA_TIER="high"
    elif [[ "$KRIA_HAS_GPU" == "true" && "$KRIA_GPU_VRAM_MB" -ge 4000 && "$KRIA_TOTAL_RAM_GB" -ge 12 ]]; then
        KRIA_TIER="performance"
    elif [[ "$KRIA_TOTAL_RAM_GB" -ge 8 ]]; then
        KRIA_TIER="standard"
    else
        KRIA_TIER="lite"
    fi
fi

# ── Tier → Model Mapping ─────────────────────────────────────────
case "$KRIA_TIER" in
    lite)
        KRIA_LLM_MODEL="Qwen2.5-3B-Instruct-Q4_K_M.gguf"
        KRIA_LLM_REPO="Qwen/Qwen2.5-3B-Instruct-GGUF"
        KRIA_LLM_DISPLAY="Qwen2.5-3B-Instruct (Lite)"
        KRIA_MMPROJ_FILE=""
        KRIA_STT_MODEL="ggml-small-q5_1.bin"
        KRIA_CTX_SIZE=1024
        KRIA_MAX_TOKENS=512
        KRIA_GPU_LAYERS=0
        KRIA_BATCH_SIZE=128
        KRIA_WHISPER_GPU_LAYERS=0
        KRIA_CONTAINER_MEM="3500m"
        KRIA_CORE_MEM="1g"
        KRIA_LLAMA_THREADS=$(( KRIA_CPU_THREADS < 4 ? KRIA_CPU_THREADS : 4 ))
        KRIA_WHISPER_THREADS=$(( KRIA_CPU_THREADS < 4 ? KRIA_CPU_THREADS : 4 ))
        KRIA_VISION_ENABLED=false
        KRIA_ACTIVE_MODEL_KEY="qwen2.5-3b"
        ;;
    standard)
        KRIA_LLM_MODEL="microsoft_Phi-4-mini-instruct-Q4_K_M.gguf"
        KRIA_LLM_REPO="bartowski/microsoft_Phi-4-mini-instruct-GGUF"
        KRIA_LLM_DISPLAY="Phi-4-mini-instruct 3.8B"
        KRIA_MMPROJ_FILE=""
        KRIA_STT_MODEL="ggml-medium-q5_0.bin"
        KRIA_CTX_SIZE=2048
        KRIA_MAX_TOKENS=1024
        KRIA_GPU_LAYERS=0
        KRIA_BATCH_SIZE=256
        KRIA_WHISPER_GPU_LAYERS=0
        KRIA_CONTAINER_MEM="5g"
        KRIA_CORE_MEM="1500m"
        KRIA_LLAMA_THREADS=$(( KRIA_CPU_THREADS < 6 ? KRIA_CPU_THREADS : 6 ))
        KRIA_WHISPER_THREADS=$(( KRIA_CPU_THREADS < 6 ? KRIA_CPU_THREADS : 6 ))
        KRIA_VISION_ENABLED=false
        KRIA_ACTIVE_MODEL_KEY="phi-4-mini"
        ;;
    performance)
        KRIA_LLM_MODEL="Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf"
        KRIA_LLM_REPO="unsloth/Qwen2.5-VL-7B-Instruct-GGUF"
        KRIA_LLM_DISPLAY="Qwen2.5-VL-7B-Instruct"
        KRIA_MMPROJ_FILE="mmproj-F16.gguf"
        KRIA_STT_MODEL="ggml-large-v3-turbo-q5_0.bin"
        KRIA_CTX_SIZE=4096
        KRIA_MAX_TOKENS=2048
        KRIA_GPU_LAYERS=15
        KRIA_BATCH_SIZE=256
        KRIA_WHISPER_GPU_LAYERS=0
        KRIA_CONTAINER_MEM="8g"
        KRIA_CORE_MEM="2g"
        KRIA_LLAMA_THREADS=$(( KRIA_CPU_THREADS < 8 ? KRIA_CPU_THREADS : 8 ))
        KRIA_WHISPER_THREADS=$(( KRIA_CPU_THREADS < 8 ? KRIA_CPU_THREADS : 8 ))
        KRIA_VISION_ENABLED=true
        KRIA_ACTIVE_MODEL_KEY="qwen2.5-vl-7b"
        ;;
    high)
        KRIA_LLM_MODEL="Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf"
        KRIA_LLM_REPO="unsloth/Qwen2.5-VL-7B-Instruct-GGUF"
        KRIA_LLM_DISPLAY="Qwen2.5-VL-7B-Instruct"
        KRIA_MMPROJ_FILE="mmproj-F16.gguf"
        KRIA_STT_MODEL="ggml-large-v3-turbo-q5_0.bin"
        KRIA_CTX_SIZE=8192
        KRIA_MAX_TOKENS=4096
        KRIA_GPU_LAYERS=99
        KRIA_BATCH_SIZE=512
        KRIA_WHISPER_GPU_LAYERS=0
        KRIA_CONTAINER_MEM="12g"
        KRIA_CORE_MEM="2g"
        KRIA_LLAMA_THREADS=$(( KRIA_CPU_THREADS < 12 ? KRIA_CPU_THREADS : 12 ))
        KRIA_WHISPER_THREADS=$(( KRIA_CPU_THREADS < 8 ? KRIA_CPU_THREADS : 8 ))
        KRIA_VISION_ENABLED=true
        KRIA_ACTIVE_MODEL_KEY="qwen2.5-vl-7b"
        ;;
    *)
        echo "[detect_hardware] ERROR: Unknown tier '${KRIA_TIER}'. Valid: lite, standard, performance, high" >&2
        return 1 2>/dev/null || exit 1
        ;;
esac

# ── Export all variables ──────────────────────────────────────────
export KRIA_TIER KRIA_TOTAL_RAM_GB KRIA_HAS_GPU KRIA_GPU_VRAM_MB KRIA_GPU_NAME KRIA_CPU_THREADS
export KRIA_LLM_MODEL KRIA_LLM_REPO KRIA_LLM_DISPLAY KRIA_MMPROJ_FILE
export KRIA_STT_MODEL KRIA_CTX_SIZE KRIA_MAX_TOKENS
export KRIA_GPU_LAYERS KRIA_BATCH_SIZE KRIA_WHISPER_GPU_LAYERS
export KRIA_CONTAINER_MEM KRIA_CORE_MEM
export KRIA_LLAMA_THREADS KRIA_WHISPER_THREADS
export KRIA_VISION_ENABLED KRIA_ACTIVE_MODEL_KEY

# ── Cache to disk ─────────────────────────────────────────────────
mkdir -p "$_DH_CACHE_DIR"
cat > "$_DH_CACHE_FILE" <<ENVEOF
# K.R.I.A. Hardware Tier — auto-generated by detect_hardware.sh
# Re-detect:  KRIA_REDETECT=1 bash scripts/setup.sh
# Manual override:  KRIA_TIER=performance bash scripts/setup.sh
KRIA_TIER="${KRIA_TIER}"
KRIA_TOTAL_RAM_GB=${KRIA_TOTAL_RAM_GB}
KRIA_HAS_GPU=${KRIA_HAS_GPU}
KRIA_GPU_VRAM_MB=${KRIA_GPU_VRAM_MB}
KRIA_GPU_NAME="${KRIA_GPU_NAME}"
KRIA_CPU_THREADS=${KRIA_CPU_THREADS}
KRIA_LLM_MODEL="${KRIA_LLM_MODEL}"
KRIA_LLM_REPO="${KRIA_LLM_REPO}"
KRIA_LLM_DISPLAY="${KRIA_LLM_DISPLAY}"
KRIA_MMPROJ_FILE="${KRIA_MMPROJ_FILE}"
KRIA_STT_MODEL="${KRIA_STT_MODEL}"
KRIA_CTX_SIZE=${KRIA_CTX_SIZE}
KRIA_MAX_TOKENS=${KRIA_MAX_TOKENS}
KRIA_GPU_LAYERS=${KRIA_GPU_LAYERS}
KRIA_BATCH_SIZE=${KRIA_BATCH_SIZE}
KRIA_WHISPER_GPU_LAYERS=${KRIA_WHISPER_GPU_LAYERS}
KRIA_CONTAINER_MEM="${KRIA_CONTAINER_MEM}"
KRIA_CORE_MEM="${KRIA_CORE_MEM}"
KRIA_LLAMA_THREADS=${KRIA_LLAMA_THREADS}
KRIA_WHISPER_THREADS=${KRIA_WHISPER_THREADS}
KRIA_VISION_ENABLED=${KRIA_VISION_ENABLED}
KRIA_ACTIVE_MODEL_KEY="${KRIA_ACTIVE_MODEL_KEY}"
ENVEOF
chmod 600 "$_DH_CACHE_FILE"

# ── Summary ───────────────────────────────────────────────────────
_tier_upper=$(echo "$KRIA_TIER" | tr '[:lower:]' '[:upper:]')
echo ""
echo -e "  \033[36m\033[1m┌────────────────────────────────────────────┐\033[0m"
echo -e "  \033[36m\033[1m│  Hardware Tier: ${_tier_upper}$(printf '%*s' $((27 - ${#_tier_upper})) '')│\033[0m"
echo -e "  \033[36m\033[1m└────────────────────────────────────────────┘\033[0m"
echo -e "  RAM:  ${KRIA_TOTAL_RAM_GB} GB    CPU: ${KRIA_CPU_THREADS} threads    GPU: ${KRIA_GPU_NAME} (${KRIA_GPU_VRAM_MB} MB VRAM)"
echo -e "  LLM:  ${KRIA_LLM_DISPLAY}"
echo -e "  STT:  ${KRIA_STT_MODEL}"
echo -e "  Ctx:  ${KRIA_CTX_SIZE}    Vision: ${KRIA_VISION_ENABLED}    GPU Layers: ${KRIA_GPU_LAYERS}"
echo -e "  Container memory limit: ${KRIA_CONTAINER_MEM}"
echo ""
