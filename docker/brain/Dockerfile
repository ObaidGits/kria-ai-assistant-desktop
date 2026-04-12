# K.R.I.A. Brain Container — LLM + STT (CPU mode)
#
# Uses the official pre-built llama-server binary (Ubuntu x64, CPU-only from
# github.com/ggml-org/llama.cpp releases) and builds whisper-server from source
# (no Linux pre-built binaries exist for whisper.cpp).
#
# For GPU/CUDA support use Dockerfile.gpu instead:
#   docker compose -f docker-compose.yml -f docker-compose.gpu.yml up -d

# ── Stage 1: Build whisper-server from source (CPU) ───────────────
FROM ubuntu:24.04 AS whisper-builder
ENV DEBIAN_FRONTEND=noninteractive
ARG WHISPER_VERSION=v1.8.4
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential cmake git ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
RUN git clone --depth 1 --branch ${WHISPER_VERSION} \
        https://github.com/ggml-org/whisper.cpp . \
    && cmake -B build \
        -DCMAKE_BUILD_TYPE=Release \
        -DWHISPER_BUILD_TESTS=OFF \
    && cmake --build build --target whisper-server -j$(nproc)

# ── Stage 2: Runtime image ────────────────────────────────────────
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
ARG LLAMA_CPP_VERSION=b8762

RUN apt-get update && apt-get install -y --no-install-recommends \
    curl \
    ca-certificates \
    libgomp1 \
    && rm -rf /var/lib/apt/lists/*

# Download the pre-built llama-server CPU binary from the official release.
# The tarball contains both the llama-server binary and shared libs (libmtmd.so, etc.)
# All files go to /usr/local/ so the dynamic linker finds them.
# IMPORTANT: libggml-cpu-*.so backend plugins must also be in the SAME directory as
# the binary (/usr/local/bin/) because ggml_backend_load_best() searches
# get_executable_path() (i.e. dirname of the binary), not standard lib paths.
RUN mkdir /tmp/llama \
    && curl -fsSL \
       "https://github.com/ggml-org/llama.cpp/releases/download/${LLAMA_CPP_VERSION}/llama-${LLAMA_CPP_VERSION}-bin-ubuntu-x64.tar.gz" \
       | tar -xz -C /tmp/llama/ \
    && find /tmp/llama -name '*.so*' -exec cp -P {} /usr/local/lib/ \; \
    && find /tmp/llama -name 'libggml-*.so' -exec cp {} /usr/local/bin/ \; \
    && find /tmp/llama -maxdepth 3 -name 'llama-server' -exec install -m755 {} /usr/local/bin/llama-server \; \
    && ldconfig \
    && rm -rf /tmp/llama

COPY --from=whisper-builder /src/build/bin/whisper-server /usr/local/bin/whisper-server
# Copy all shared libraries produced by the whisper.cpp build
COPY --from=whisper-builder /src/build/src/ /tmp/whisper-libs/
RUN find /tmp/whisper-libs -name '*.so*' -exec cp -P {} /usr/local/lib/ \; \
    && ldconfig && chmod +x /usr/local/bin/whisper-server \
    && rm -rf /tmp/whisper-libs

# Model directory (mounted as volume)
VOLUME /models

# Copy startup configuration
COPY configs/ /configs/
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

EXPOSE 8080 8081

HEALTHCHECK --interval=30s --timeout=10s --retries=3 --start-period=60s \
    CMD curl -f http://localhost:8080/health || exit 1

ENTRYPOINT ["/entrypoint.sh"]
