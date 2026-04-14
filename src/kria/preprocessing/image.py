"""
Image Preprocessing Module
===========================
Adaptive resizing, EXIF rotation, optional grayscale, and JPEG compression
to minimize visual token consumption for Qwen2.5-VL on 6 GB VRAM.
"""
from __future__ import annotations

import asyncio
import logging
from io import BytesIO
from pathlib import Path
from typing import Optional

logger = logging.getLogger("kria.preprocessing.image")


async def preprocess_image(
    source: str,
    *,
    content: Optional[bytes] = None,
    max_edge: int = 1280,
    grayscale: bool = False,
    quality: int = 85,
    max_tokens: int = 3500,
) -> "PreprocessedPayload":
    """Preprocess an image for the vision model.

    1. EXIF auto-rotation.
    2. Adaptive resize (long edge capped at *max_edge*).
    3. Optional grayscale conversion.
    4. JPEG compression at *quality*.
    5. Visual-token estimate for Qwen2.5-VL.
    """
    from kria.preprocessing.dispatcher import PreprocessedPayload
    from kria.preprocessing.token_budget import estimate_image_tokens

    def _process() -> PreprocessedPayload:
        from PIL import Image, ImageOps

        # Load image
        if content:
            img = Image.open(BytesIO(content))
        else:
            img = Image.open(source)

        # EXIF auto-rotation
        img = ImageOps.exif_transpose(img)
        orig_w, orig_h = img.size

        # Adaptive resize — cap long edge
        if max(orig_w, orig_h) > max_edge:
            img.thumbnail((max_edge, max_edge), Image.Resampling.LANCZOS)

        # Optional grayscale
        if grayscale:
            img = img.convert("L")

        # Ensure compatible mode for JPEG
        if img.mode not in {"RGB", "L"}:
            img = img.convert("RGB")

        # Encode to JPEG
        out = BytesIO()
        img.save(out, format="JPEG", quality=quality, optimize=True)
        processed_bytes = out.getvalue()

        new_w, new_h = img.size
        vis_tokens = estimate_image_tokens(new_w, new_h)

        return PreprocessedPayload(
            text="",
            images=[processed_bytes],
            token_estimate=vis_tokens,
            source_type="image",
            metadata={
                "path": source,
                "original_size": [orig_w, orig_h],
                "processed_size": [new_w, new_h],
                "grayscale": grayscale,
                "format": "jpeg",
                "bytes": len(processed_bytes),
            },
            truncated=max(orig_w, orig_h) > max_edge,
        )

    return await asyncio.to_thread(_process)
