# ============================================================
# K.R.I.A. — GPU Dockerfile (NVIDIA CUDA)
# Multi-stage build: Rust builder → Python env → CUDA runtime
# ============================================================

# ── Stage 1: Rust builder ────────────────────────────────────
FROM rust:1.82-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev libasound2-dev && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ crates/

# Build only kria-server in release mode
RUN cargo build --release -p kria-server && \
    strip /build/target/release/kria-server

# ── Stage 2: Python environment ──────────────────────────────
FROM python:3.12-slim-bookworm AS python-env

COPY --from=ghcr.io/astral-sh/uv:latest /uv /usr/local/bin/uv

WORKDIR /pyenv
COPY kria-modules/ /build-modules/

RUN uv venv /pyenv/venv && \
    . /pyenv/venv/bin/activate && \
    uv pip install /build-modules/ && \
    rm -rf /build-modules/ /root/.cache

# ── Stage 3: CUDA runtime ───────────────────────────────────
FROM nvidia/cuda:12.6.3-base-ubuntu24.04

ARG LLAMA_CPP_TAG=b5300

RUN apt-get update && apt-get install -y --no-install-recommends \
    libcublas-12-6 libcudart-12-6 \
    python3 python3-venv libpython3.12 \
    curl gosu ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Download pre-compiled CUDA llama-server
RUN ARCH="$(dpkg --print-architecture)" && \
    if [ "$ARCH" = "amd64" ]; then LLAMA_ARCH="x64"; else LLAMA_ARCH="$ARCH"; fi && \
    curl -fSL "https://github.com/ggml-org/llama.cpp/releases/download/${LLAMA_CPP_TAG}/llama-${LLAMA_CPP_TAG}-bin-linux-${LLAMA_ARCH}.zip" \
      -o /tmp/llama.zip && \
    apt-get update && apt-get install -y --no-install-recommends unzip && \
    unzip -q /tmp/llama.zip -d /tmp/llama && \
    find /tmp/llama -name "llama-server" -type f -exec cp {} /usr/local/bin/llama-server \; && \
    chmod +x /usr/local/bin/llama-server && \
    rm -rf /tmp/llama /tmp/llama.zip && \
    apt-get purge -y unzip && apt-get autoremove -y && \
    rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN groupadd -g 1000 kria && \
    useradd -u 1000 -g kria -m -s /bin/bash kria

# Application layout
RUN mkdir -p /app/data /app/models /app/config && \
    chown -R kria:kria /app

WORKDIR /app

# Copy artifacts from build stages
COPY --from=builder --chown=kria:kria /build/target/release/kria-server /app/kria-server
COPY --from=python-env --chown=kria:kria /pyenv/venv /app/python-env
COPY --chown=kria:kria config/default.toml config/mcp_servers.json /app/config/
COPY --chown=kria:kria scripts/docker-entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh

# Environment
ENV KRIA_DATA_DIR=/app/data \
    KRIA_MODELS_DIR=/app/models \
    KRIA_CONFIG=/app/config/default.toml \
    KRIA_PYTHON_VENV=/app/python-env \
    KRIA_LLAMA_SERVER=/usr/local/bin/llama-server \
    NVIDIA_VISIBLE_DEVICES=all \
    NVIDIA_DRIVER_CAPABILITIES=compute,utility

EXPOSE 3001

VOLUME ["/app/data", "/app/models"]

ENTRYPOINT ["/app/entrypoint.sh"]
CMD ["--bind", "0.0.0.0:3001"]
