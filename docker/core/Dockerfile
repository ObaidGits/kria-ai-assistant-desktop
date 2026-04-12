# kria-core: K.R.I.A. FastAPI orchestrator
FROM python:3.12-slim-bookworm

# System dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy project metadata first (layer caching)
COPY pyproject.toml /app/pyproject.toml
COPY src/ /app/src/

# Install the project and runtime dependencies
RUN pip install --no-cache-dir -e /app

# Non-root user for security
RUN adduser --disabled-password --gecos "" kria \
    && mkdir -p /data /home/kria/.kria/rollback /home/kria/.cache \
    && chown -R kria:kria /app /data /home/kria

USER kria

ENV KRIA_ENV=production
ENV KRIA_SQLITE_PATH=/data/kria.db
ENV KRIA_LOG_DIR=/data/logs
ENV PYTHONPATH=/app/src

EXPOSE 8000

HEALTHCHECK --interval=30s --timeout=10s --retries=3 \
    CMD curl -f http://localhost:8000/health || exit 1

CMD ["uvicorn", "kria.main:app", "--host", "0.0.0.0", "--port", "8000", \
     "--workers", "1", "--loop", "uvloop"]
