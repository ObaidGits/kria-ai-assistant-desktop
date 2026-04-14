#!/bin/bash
set -euo pipefail

MODEL_DIR="/models"
CONFIG_DIR="/configs"

echo "=== K.R.I.A. Brain Container Starting ==="
echo "GPU Info:"
nvidia-smi --query-gpu=name,memory.total,memory.free --format=csv,noheader 2>/dev/null || echo "  (nvidia-smi not available — running in CPU mode)"

# Number of layers to offload to GPU (0 = CPU only, 99 = all layers to GPU)
GPU_LAYERS="${LLAMA_GPU_LAYERS:-0}"

# ── Model Selection ───────────────────────────────────────────────
# LLAMA_BRAIN_ROLE:  "primary" (default) or "secondary"
#   primary   → Phi-4-mini-instruct 3.8B  (fast, low VRAM)
#   secondary → Qwen2.5-VL-7B-Instruct    (smart, more VRAM)
#
# For backward compat LLAMA_MODEL_SIZE ("0.6b"/"8b") is also accepted.

BRAIN_ROLE="${LLAMA_BRAIN_ROLE:-primary}"

# Legacy: remap old LLAMA_MODEL_SIZE values
if [[ -n "${LLAMA_MODEL_SIZE:-}" ]]; then
    case "${LLAMA_MODEL_SIZE}" in
        "0.6b") BRAIN_ROLE="primary" ;;
        "8b")   BRAIN_ROLE="secondary" ;;
    esac
fi

# Check for persistent config file written by kria-core model-switch API
# Only respect this for the PRIMARY container — the secondary container
# always uses its env-var role so the API can't accidentally flip it.
MODEL_CONFIG_FILE="/data/model_size"
if [[ "${LLAMA_BRAIN_ROLE:-}" == "" && -f "${MODEL_CONFIG_FILE}" ]]; then
    CONFIGURED_ROLE=$(tr -d '[:space:]' < "${MODEL_CONFIG_FILE}")
    case "${CONFIGURED_ROLE}" in
        "primary"|"0.6b")   BRAIN_ROLE="primary" ;;
        "secondary"|"8b")   BRAIN_ROLE="secondary" ;;
        *) echo "WARNING: Invalid role '${CONFIGURED_ROLE}' in ${MODEL_CONFIG_FILE} — ignoring" ;;
    esac
fi

if [[ "${BRAIN_ROLE}" == "secondary" ]]; then
    LLM_MODEL="${MODEL_DIR}/llm/Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf"
    MMPROJ_FILE="${MODEL_DIR}/llm/mmproj-F16.gguf"
    LLAMA_PORT="${LLAMA_PORT:-8085}"
    CTX_SIZE="${LLAMA_CTX_SIZE:-16384}"
    echo "Role: SECONDARY — Qwen2.5-VL-7B-Instruct (Vision) [Unsloth]"
else
    LLM_MODEL="${MODEL_DIR}/llm/microsoft_Phi-4-mini-instruct-Q4_K_M.gguf"
    # Fallback: if Phi-4-mini not yet downloaded, try legacy Qwen3-0.6B
    if [[ ! -f "${LLM_MODEL}" && -f "${MODEL_DIR}/llm/Qwen3-0.6B-Q8_0.gguf" ]]; then
        echo "NOTE: Phi-4-mini not found — falling back to Qwen3-0.6B"
        LLM_MODEL="${MODEL_DIR}/llm/Qwen3-0.6B-Q8_0.gguf"
    fi
    LLAMA_PORT="${LLAMA_PORT:-8080}"
    CTX_SIZE="${LLAMA_CTX_SIZE:-4096}"
    echo "Role: PRIMARY — Phi-4-mini-instruct 3.8B"
fi

echo "Using model: ${LLM_MODEL} (port=${LLAMA_PORT}, ctx=${CTX_SIZE}, gpu-layers=${GPU_LAYERS})"

# ── Guard: LLM model must exist ──────────────────────────────────
if [[ ! -f "${LLM_MODEL}" ]]; then
    echo "ERROR: LLM model not found at ${LLM_MODEL}"
    echo "       Download models with:  python scripts/download_models.py"
    echo "       Expected files:"
    echo "         models/llm/microsoft_Phi-4-mini-instruct-Q4_K_M.gguf   (primary)"
    echo "         models/llm/Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf  (secondary, Unsloth Q4_K_M)"
    echo "       Listing ${MODEL_DIR}/llm/ :"
    ls -la "${MODEL_DIR}/llm/" 2>/dev/null || echo "  (directory not found)"
    exit 1
fi

# ── Start llama.cpp server ────────────────────────────────────────
# Build optional multimodal projector flag for vision models
MMPROJ_FLAG=""
if [[ -n "${MMPROJ_FILE:-}" && -f "${MMPROJ_FILE}" ]]; then
    MMPROJ_FLAG="--mmproj ${MMPROJ_FILE}"
    echo "Vision encoder: ${MMPROJ_FILE}"
elif [[ -n "${MMPROJ_FILE:-}" ]]; then
    echo "WARNING: Vision encoder not found at ${MMPROJ_FILE} — vision disabled"
    echo "         Download with: python scripts/download_models.py"
fi

echo "Starting llama.cpp server (port=${LLAMA_PORT}, gpu-layers=${GPU_LAYERS})..."
# shellcheck disable=SC2086
llama-server \
    --model "${LLM_MODEL}" \
    --host 0.0.0.0 \
    --port "${LLAMA_PORT}" \
    --ctx-size "${CTX_SIZE}" \
    --n-gpu-layers "${GPU_LAYERS}" \
    --threads 8 \
    --batch-size 512 \
    --flash-attn on \
    --cont-batching \
    --metrics \
    ${MMPROJ_FLAG} \
    &

LLAMA_PID=$!
echo "llama-server started (PID: ${LLAMA_PID})"

# ── Start whisper.cpp server (primary brain only) ─────────────────
# Whisper STT only runs on the primary container (port 8081).
# The secondary container is inference-only — no STT needed.
WHISPER_MODEL_FILE="${MODEL_DIR}/stt/${WHISPER_MODEL:-ggml-small.en.bin}"
WHISPER_PID=""

if [[ "${BRAIN_ROLE}" == "primary" && -f "${WHISPER_MODEL_FILE}" ]]; then
    WHISPER_THREADS="${WHISPER_THREADS:-8}"
    echo "Starting whisper.cpp server (model: ${WHISPER_MODEL_FILE}, threads=${WHISPER_THREADS})..."
    whisper-server \
        --model "${WHISPER_MODEL_FILE}" \
        --host 0.0.0.0 \
        --port 8081 \
        --threads "${WHISPER_THREADS}" \
        &
    WHISPER_PID=$!
    echo "whisper-server started (PID: ${WHISPER_PID})"
elif [[ "${BRAIN_ROLE}" == "primary" ]]; then
    echo "WARNING: Whisper model not found at ${WHISPER_MODEL_FILE} — STT disabled"
    echo "         To enable speech-to-text, download a model to models/stt/"
fi

# ── Wait ─────────────────────────────────────────────────────────
echo "=== Brain container ready ==="
if [[ -n "${WHISPER_PID}" ]]; then
    wait -n ${LLAMA_PID} ${WHISPER_PID}
else
    wait ${LLAMA_PID}
fi
exit $?
