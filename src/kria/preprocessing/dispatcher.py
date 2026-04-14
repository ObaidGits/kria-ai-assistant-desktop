"""
Preprocessing Dispatcher
=========================
Routes content to the appropriate preprocessing module based on MIME type /
file extension / URL pattern.  Single entry point: ``preprocess()``.
"""
from __future__ import annotations

import logging
import mimetypes
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

logger = logging.getLogger("kria.preprocessing.dispatcher")

# ---------------------------------------------------------------------------
# Result envelope
# ---------------------------------------------------------------------------


@dataclass
class PreprocessedPayload:
    """Standard output from every preprocessing module."""

    text: str = ""
    images: list[bytes] = field(default_factory=list)
    token_estimate: int = 0
    source_type: str = ""          # image | document | web | video | audio | code
    metadata: dict = field(default_factory=dict)
    truncated: bool = False


# ---------------------------------------------------------------------------
# Extension → source-type mapping
# ---------------------------------------------------------------------------

_IMAGE_EXTS = {".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".tiff", ".tif"}
_DOCUMENT_EXTS = {".pdf", ".docx", ".doc", ".xlsx", ".xls", ".pptx", ".csv"}
_VIDEO_EXTS = {".mp4", ".mkv", ".avi", ".mov", ".webm", ".flv", ".wmv", ".m4v"}
_AUDIO_EXTS = {".mp3", ".wav", ".ogg", ".flac", ".m4a", ".aac", ".wma", ".opus"}
_CODE_EXTS = {
    ".py", ".js", ".ts", ".jsx", ".tsx", ".java", ".c", ".cpp", ".h", ".hpp",
    ".go", ".rs", ".rb", ".php", ".cs", ".swift", ".kt", ".scala", ".lua",
    ".sh", ".bash", ".zsh", ".ps1", ".r", ".m", ".zig", ".nim", ".hs",
}


def detect_source_type(
    path: Optional[str] = None,
    url: Optional[str] = None,
    content_type: Optional[str] = None,
) -> str:
    """Return one of: image, document, web, video, audio, code, text, unknown."""
    if url and url.startswith(("http://", "https://")):
        return "web"

    if content_type:
        major = content_type.split("/")[0]
        if major == "image":
            return "image"
        if major == "video":
            return "video"
        if major == "audio":
            return "audio"

    if path:
        ext = Path(path).suffix.lower()
        if ext in _IMAGE_EXTS:
            return "image"
        if ext in _DOCUMENT_EXTS:
            return "document"
        if ext in _VIDEO_EXTS:
            return "video"
        if ext in _AUDIO_EXTS:
            return "audio"
        if ext in _CODE_EXTS:
            return "code"
        # Guess via mimetypes
        mime, _ = mimetypes.guess_type(path)
        if mime:
            major = mime.split("/")[0]
            if major == "text":
                return "text"
        # Fallback: small text-like extensions
        if ext in {".txt", ".md", ".rst", ".log", ".ini", ".cfg", ".toml", ".yaml", ".yml", ".json", ".xml", ".html", ".htm", ".css"}:
            return "text"

    return "unknown"


# ---------------------------------------------------------------------------
# Main entry point
# ---------------------------------------------------------------------------

async def preprocess(
    source: str,
    content: Optional[bytes] = None,
    *,
    max_tokens: int = 3500,
    image_max_edge: int = 1280,
    image_grayscale: bool = False,
    keyframe_max: int = 5,
    scene_threshold: float = 0.3,
) -> PreprocessedPayload:
    """Preprocess *source* (file path or URL) and return a token-budgeted payload.

    Parameters
    ----------
    source : str
        A file path or URL to preprocess.
    content : bytes, optional
        Raw bytes (when the file is already in memory, e.g. upload).
    max_tokens : int
        Maximum token budget for the text portion of the payload.
    image_max_edge : int
        Cap the longest edge of images at this many pixels.
    image_grayscale : bool
        Convert images to grayscale (reduces visual tokens, default off).
    keyframe_max : int
        Maximum number of keyframes to extract from video.
    scene_threshold : float
        Scene-change sensitivity for keyframe extraction (0–1).
    """
    source_type = detect_source_type(path=source, url=source)

    try:
        if source_type == "image":
            from kria.preprocessing.image import preprocess_image
            return await preprocess_image(
                source, content=content,
                max_edge=image_max_edge, grayscale=image_grayscale,
                max_tokens=max_tokens,
            )

        if source_type == "document":
            from kria.preprocessing.document import preprocess_document
            return await preprocess_document(source, content=content, max_tokens=max_tokens)

        if source_type == "web":
            from kria.preprocessing.web import preprocess_url
            return await preprocess_url(source, max_tokens=max_tokens)

        if source_type == "video":
            from kria.preprocessing.video_audio import preprocess_video
            return await preprocess_video(
                source, content=content, max_tokens=max_tokens,
                keyframe_max=keyframe_max, scene_threshold=scene_threshold,
                image_max_edge=image_max_edge,
            )

        if source_type == "audio":
            from kria.preprocessing.video_audio import preprocess_audio
            return await preprocess_audio(source, content=content, max_tokens=max_tokens)

        if source_type == "code":
            from kria.preprocessing.code import preprocess_code
            return await preprocess_code(source, content=content, max_tokens=max_tokens)

        # Fallback: plain text
        if source_type == "text" or source_type == "unknown":
            return await _preprocess_text(source, content=content, max_tokens=max_tokens)

    except Exception as exc:
        logger.error("Preprocessing failed for %s (%s): %s", source, source_type, exc)
        # Graceful fallback — return raw text if possible
        return await _preprocess_text(source, content=content, max_tokens=max_tokens)

    return PreprocessedPayload(source_type="unknown")


async def _preprocess_text(
    source: str,
    content: Optional[bytes] = None,
    max_tokens: int = 3500,
) -> PreprocessedPayload:
    """Fallback for plain text files."""
    import asyncio

    from kria.preprocessing.token_budget import estimate_tokens, smart_crop

    def _read():
        if content:
            return content.decode("utf-8", errors="replace")
        try:
            return Path(source).read_text(encoding="utf-8", errors="replace")
        except Exception:
            return ""

    text = await asyncio.to_thread(_read)
    text, truncated = smart_crop(text, max_tokens)
    return PreprocessedPayload(
        text=text,
        token_estimate=estimate_tokens(text),
        source_type="text",
        metadata={"path": source},
        truncated=truncated,
    )
