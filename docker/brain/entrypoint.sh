#!/bin/bash
set -euo pipefail

MODEL_DIR="/models"
CONFIG_DIR="/configs"

echo "=== K.R.I.A. Brain Container Starting ==="
echo "GPU Info:"
nvidia-smi --query-gpu=name,memory.total,memory.free --format=csv,noheader 2>/dev/null || echo "  (nvidia-smi not available — running in CPU mode)"

# Number of layers to offload to GPU (0 = CPU only, 99 = all layers to GPU)
GPU_LAYERS="${LLAMA_GPU_LAYERS:-0}"

# Model selection: default to 0.6B (fits in ~2GB RAM); set LLAMA_MODEL_SIZE=8b for 8B.
# NOTE: 8B requires ~10 GB RAM or GPU mode (set LLAMA_GPU_LAYERS=99 for full GPU offload).
if [[ "${LLAMA_MODEL_SIZE:-0.6b}" == "8b" ]]; then
    LLM_MODEL="${MODEL_DIR}/llm/Qwen3-8B-Q4_K_M.gguf"
    CTX_SIZE="${LLAMA_CTX_SIZE:-4096}"
else
    LLM_MODEL="${MODEL_DIR}/llm/Qwen3-0.6B-Q8_0.gguf"
    CTX_SIZE="${LLAMA_CTX_SIZE:-8192}"
fi

echo "Using model: ${LLM_MODEL} (ctx=${CTX_SIZE}, gpu-layers=${GPU_LAYERS})"

# ── Start llama.cpp server ──
echo "Starting llama.cpp server (gpu-layers=${GPU_LAYERS})..."
llama-server \
    --model "${LLM_MODEL}" \
    --host 0.0.0.0 \
    --port 8080 \
    --ctx-size "${CTX_SIZE}" \
    --n-gpu-layers "${GPU_LAYERS}" \
    --threads 8 \
    --batch-size 512 \
    --flash-attn on \
    --cont-batching \
    --metrics \
    &

LLAMA_PID=$!
echo "llama-server started (PID: ${LLAMA_PID})"

# ── Start whisper.cpp server ──
# WHISPER_MODEL env var selects model. Defaults to small.en (fast on CPU).
# With GPU: use medium.en or large-v3-turbo for much better accuracy.
WHISPER_MODEL_FILE="${MODEL_DIR}/stt/${WHISPER_MODEL:-ggml-small.en.bin}"
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

# ── Wait for both processes ──
echo "=== Brain container ready ==="
wait -n ${LLAMA_PID} ${WHISPER_PID}
exit $?
