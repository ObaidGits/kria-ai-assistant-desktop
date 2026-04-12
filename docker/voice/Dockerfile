# kria-voice: Piper TTS + audio server
# Minimal image — no GPU required
FROM python:3.12-slim-bookworm

# Install system audio dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    wget curl \
    libportaudio2 libportaudiocpp0 portaudio19-dev \
    libasound2 libasound2-dev \
    libsndfile1 libopus0 \
    ffmpeg \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Install Piper TTS and HTTP server dependencies
RUN pip install --no-cache-dir piper-tts fastapi uvicorn[standard] pydantic

# Lightweight HTTP wrapper for Piper
COPY server.py /app/server.py

ENV PIPER_MODEL_PATH=/models/piper
ENV PORT=8082

EXPOSE 8082

HEALTHCHECK --interval=20s --timeout=5s --retries=3 \
    CMD curl -f http://localhost:${PORT}/health || exit 1

CMD ["python", "server.py"]
