# kria-init: One-shot container that downloads models before other services start
FROM python:3.12-slim-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
    wget curl aria2 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY scripts/download_models.py /app/download_models.py

RUN pip install --no-cache-dir httpx tqdm

ENV MODELS_DIR=/models

CMD ["python", "/app/download_models.py"]
