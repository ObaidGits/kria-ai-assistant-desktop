"""
K.R.I.A. Local Multimodal Preprocessing Pipeline
==================================================
Minimize token consumption for a strict 6 GB VRAM / 4096 context environment.

Public API
----------
- ``preprocess(source, content, **opts)`` — main entry point (dispatcher)
- ``PreprocessedPayload``                 — standard result dataclass
- Per-module functions for direct use:
  ``preprocess_image``, ``preprocess_document``, ``preprocess_url``,
  ``preprocess_video``, ``preprocess_audio``, ``preprocess_code``
- Token utilities: ``estimate_tokens``, ``smart_crop``, ``chunk_text``
"""
from kria.preprocessing.dispatcher import PreprocessedPayload, preprocess  # noqa: F401
from kria.preprocessing.token_budget import (  # noqa: F401
    chunk_text,
    estimate_tokens,
    estimate_image_tokens,
    smart_crop,
)

__all__ = [
    "preprocess",
    "PreprocessedPayload",
    "estimate_tokens",
    "estimate_image_tokens",
    "smart_crop",
    "chunk_text",
]
