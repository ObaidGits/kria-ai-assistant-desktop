"""
Image processor — Pre-Cognitive image analysis.

Extracts metadata, OCR text, visual features, and a resized thumbnail
before the LLM ever sees the image. Tier-aware processing depth.
"""

import base64
import io
import logging
import os
from pathlib import Path
from typing import Any

logger = logging.getLogger("kria.processors.image")

METHODS = ["analyze"]

# Tier → max thumbnail dimension
_THUMB_SIZE = {"lite": 512, "standard": 1024, "performance": 2048, "high": 0}  # 0 = original


def analyze(params: dict) -> dict:
    """
    Analyze an image file.

    Params:
        file_path: str — path to image
        operations: list[str] — subset of ["metadata", "ocr", "features", "thumbnail"]
        max_tokens: int — token budget hint
    """
    file_path = params.get("file_path", "")
    if not file_path or not os.path.isfile(file_path):
        raise FileNotFoundError(file_path)

    tier = params.get("_tier", "standard")
    operations = params.get("operations", ["metadata", "ocr", "features", "thumbnail"])
    if isinstance(operations, str):
        operations = [operations]

    result: dict[str, Any] = {"file_path": file_path}

    from PIL import Image, ExifTags

    img = Image.open(file_path)

    # ── Metadata (always) ────────────────────────────────────
    if "metadata" in operations:
        meta: dict[str, Any] = {
            "width": img.width,
            "height": img.height,
            "format": img.format or Path(file_path).suffix.lstrip("."),
            "mode": img.mode,
            "size_kb": round(os.path.getsize(file_path) / 1024, 1),
        }
        # EXIF
        exif_data = {}
        try:
            raw_exif = img._getexif()
            if raw_exif:
                for tag_id, value in raw_exif.items():
                    tag = ExifTags.TAGS.get(tag_id, str(tag_id))
                    if isinstance(value, (str, int, float)):
                        exif_data[tag] = value
        except Exception:
            pass
        if exif_data:
            meta["exif"] = exif_data
        result["metadata"] = meta

    # ── OCR (Standard+) ─────────────────────────────────────
    if "ocr" in operations and tier not in ("lite",):
        ocr_text = ""
        try:
            if tier in ("performance", "high"):
                # Try easyocr first on GPU tiers
                try:
                    import easyocr
                    reader = easyocr.Reader(["en"], gpu=True)
                    ocr_results = reader.readtext(file_path)
                    ocr_text = "\n".join(r[1] for r in ocr_results)
                except ImportError:
                    pass

            if not ocr_text:
                import pytesseract
                ocr_text = pytesseract.image_to_string(img).strip()
        except Exception as e:
            logger.warning("OCR failed: %s", e)
            ocr_text = ""

        result["ocr_text"] = ocr_text

    # ── Features (Standard+) ────────────────────────────────
    if "features" in operations and tier not in ("lite",):
        import numpy as np

        features: dict[str, Any] = {}

        try:
            arr = np.array(img.convert("RGB"))

            # Dominant colors via k-means on a small sample
            small = img.copy()
            small.thumbnail((64, 64))
            pixels = np.array(small.convert("RGB")).reshape(-1, 3)
            from collections import Counter
            # Simple: mode of quantized colors
            quantized = (pixels // 32) * 32
            tuples = [tuple(c) for c in quantized]
            top_colors = Counter(tuples).most_common(5)
            features["dominant_colors"] = [
                {"rgb": list(c), "frequency": round(n / len(tuples), 3)}
                for c, n in top_colors
            ]

            # Scene type heuristic
            gray = np.mean(arr, axis=2)
            edge_density = np.mean(np.abs(np.diff(gray, axis=0))) + np.mean(np.abs(np.diff(gray, axis=1)))
            text_density = len(result.get("ocr_text", "")) / max(img.width * img.height, 1) * 1e6

            if text_density > 50:
                features["scene_type"] = "screenshot_or_document"
            elif edge_density > 30:
                features["scene_type"] = "diagram_or_chart"
            else:
                features["scene_type"] = "photo"

            features["edge_density"] = round(float(edge_density), 2)
            features["text_density"] = round(float(text_density), 2)
        except Exception as e:
            logger.warning("Feature extraction failed: %s", e)

        result["features"] = features

    # ── Thumbnail ────────────────────────────────────────────
    if "thumbnail" in operations:
        max_dim = _THUMB_SIZE.get(tier, 1024)
        thumb = img.copy()
        if max_dim > 0 and (img.width > max_dim or img.height > max_dim):
            thumb.thumbnail((max_dim, max_dim), Image.LANCZOS)

        buf = io.BytesIO()
        fmt = "PNG" if img.mode == "RGBA" else "JPEG"
        thumb.save(buf, format=fmt, quality=85)
        b64 = base64.b64encode(buf.getvalue()).decode("ascii")
        result["thumbnail_base64"] = b64
        result["thumbnail_size"] = {"width": thumb.width, "height": thumb.height}

    # ── Summary ──────────────────────────────────────────────
    parts = []
    if "metadata" in result:
        m = result["metadata"]
        parts.append(f"{m['width']}x{m['height']} {m.get('format', '?')} ({m['size_kb']}KB)")
    if result.get("ocr_text"):
        preview = result["ocr_text"][:200]
        parts.append(f"OCR: {preview}")
    if "features" in result:
        parts.append(f"Scene: {result['features'].get('scene_type', '?')}")
    result["summary"] = "; ".join(parts) if parts else "Image analyzed"

    return result
