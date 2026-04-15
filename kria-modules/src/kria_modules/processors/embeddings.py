"""
Embeddings processor — Pre-Cognitive text embedding generation.

Generates semantic embeddings using sentence-transformers for
RAG and similarity search. Tier-aware model selection and batching.
"""

import logging
from typing import Any

logger = logging.getLogger("kria.processors.embeddings")

METHODS = ["embed_text", "embed_batch", "chunk_and_embed"]

# Tier → model name
_MODELS = {
    "lite": "all-MiniLM-L6-v2",
    "standard": "all-MiniLM-L6-v2",
    "performance": "all-mpnet-base-v2",
    "high": "all-mpnet-base-v2",
}

_CHUNK_SIZES = {"lite": 256, "standard": 512, "performance": 768, "high": 1024}

# Lazy-loaded model cache
_model_cache: dict[str, Any] = {}


def _get_model(tier: str) -> Any:
    model_name = _MODELS.get(tier, "all-MiniLM-L6-v2")
    if model_name not in _model_cache:
        from sentence_transformers import SentenceTransformer
        logger.info("Loading embedding model: %s", model_name)
        _model_cache[model_name] = SentenceTransformer(model_name)
    return _model_cache[model_name]


def embed_text(params: dict) -> dict:
    """
    Embed a single text string.

    Params:
        text: str — text to embed
    """
    text = params.get("text", "")
    if not text:
        raise ValueError("'text' is required")

    tier = params.get("_tier", "standard")
    model = _get_model(tier)
    embedding = model.encode(text, normalize_embeddings=True)

    return {
        "embedding": embedding.tolist(),
        "dimensions": len(embedding),
        "model": _MODELS.get(tier, "all-MiniLM-L6-v2"),
    }


def embed_batch(params: dict) -> dict:
    """
    Embed a batch of text strings.

    Params:
        texts: list[str] — texts to embed
        batch_size: int — processing batch size (optional)
    """
    texts = params.get("texts", [])
    if not texts:
        raise ValueError("'texts' list is required and non-empty")

    tier = params.get("_tier", "standard")
    batch_size = params.get("batch_size", 32)
    model = _get_model(tier)
    embeddings = model.encode(texts, batch_size=batch_size, normalize_embeddings=True)

    return {
        "embeddings": [e.tolist() for e in embeddings],
        "count": len(embeddings),
        "dimensions": len(embeddings[0]) if len(embeddings) > 0 else 0,
        "model": _MODELS.get(tier, "all-MiniLM-L6-v2"),
    }


def chunk_and_embed(params: dict) -> dict:
    """
    Split text into chunks and embed each chunk.

    Params:
        text: str — long text to chunk and embed
        chunk_size: int — chars per chunk (optional, tier-based default)
        overlap: int — overlap chars between chunks (optional, default 50)
    """
    text = params.get("text", "")
    if not text:
        raise ValueError("'text' is required")

    tier = params.get("_tier", "standard")
    chunk_size = params.get("chunk_size", _CHUNK_SIZES.get(tier, 512))
    overlap = params.get("overlap", 50)

    # Chunk by sentences, respecting chunk_size
    chunks = _chunk_text(text, chunk_size, overlap)

    model = _get_model(tier)
    embeddings = model.encode(chunks, normalize_embeddings=True)

    return {
        "chunks": [
            {
                "text": chunk,
                "embedding": emb.tolist(),
                "char_offset": _find_offset(text, chunk),
            }
            for chunk, emb in zip(chunks, embeddings)
        ],
        "chunk_count": len(chunks),
        "dimensions": len(embeddings[0]) if len(embeddings) > 0 else 0,
        "model": _MODELS.get(tier, "all-MiniLM-L6-v2"),
    }


def _chunk_text(text: str, chunk_size: int, overlap: int) -> list[str]:
    """Split text into overlapping chunks at sentence boundaries."""
    sentences = []
    current = ""
    for char in text:
        current += char
        if char in ".!?\n" and len(current.strip()) > 10:
            sentences.append(current)
            current = ""
    if current.strip():
        sentences.append(current)

    chunks = []
    buf = ""
    for sent in sentences:
        if len(buf) + len(sent) > chunk_size and buf:
            chunks.append(buf.strip())
            # Keep overlap from end of previous chunk
            buf = buf[-overlap:] if overlap > 0 else ""
        buf += sent

    if buf.strip():
        chunks.append(buf.strip())

    return chunks if chunks else [text[:chunk_size]]


def _find_offset(text: str, chunk: str) -> int:
    idx = text.find(chunk[:50])
    return max(idx, 0)
