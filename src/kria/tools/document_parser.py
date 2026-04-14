"""
Document Parser (GREEN tier — read-only)
=========================================
Parse PDF, DOCX, XLSX, and CSV files to extract structured content.
Delegates to the preprocessing pipeline for token-budgeted extraction.
"""
import logging
from pathlib import Path

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.document_parser")


@isolated
async def parse_pdf(path: str, max_pages: int = 50) -> dict:
    """Extract text content from a PDF file via the preprocessing pipeline."""
    from kria.preprocessing.document import preprocess_document

    payload = await preprocess_document(path)
    return {
        "path": path,
        "content": payload.text,
        "token_estimate": payload.token_estimate,
        "truncated": payload.truncated,
        **payload.metadata,
    }


@isolated
async def parse_docx(path: str) -> dict:
    """Extract text and tables from a DOCX file via the preprocessing pipeline."""
    from kria.preprocessing.document import preprocess_document

    payload = await preprocess_document(path)
    return {
        "path": path,
        "content": payload.text,
        "token_estimate": payload.token_estimate,
        "truncated": payload.truncated,
        **payload.metadata,
    }


@isolated
async def parse_xlsx(path: str, sheet: str = "", max_rows: int = 100) -> dict:
    """Extract data from an Excel spreadsheet via the preprocessing pipeline."""
    from kria.preprocessing.document import preprocess_document

    payload = await preprocess_document(path)
    return {
        "path": path,
        "content": payload.text,
        "token_estimate": payload.token_estimate,
        "truncated": payload.truncated,
        **payload.metadata,
    }


@isolated
async def parse_csv(path: str, max_rows: int = 100) -> dict:
    """Read and analyze a CSV file via the preprocessing pipeline."""
    from kria.preprocessing.document import preprocess_document

    payload = await preprocess_document(path)
    return {
        "path": path,
        "content": payload.text,
        "token_estimate": payload.token_estimate,
        "truncated": payload.truncated,
        **payload.metadata,
    }


@isolated
async def summarize_document(path: str, max_length: int = 200) -> dict:
    """Summarize a document file (PDF, DOCX, TXT, MD, CSV) using the preprocessing pipeline + LLM."""
    from kria.preprocessing import preprocess

    payload = await preprocess(path)
    text = payload.text[:10000]

    from kria.agent.llm_client import llm_client
    result = await llm_client.chat_completion(
        messages=[
            {"role": "system", "content": f"Summarize the following document in {max_length} words or fewer. Focus on key points."},
            {"role": "user", "content": text},
        ],
        max_tokens=500,
    )
    summary = "Summarization failed."
    if result:
        choice = result.get("choices", [{}])[0].get("message", {})
        summary = choice.get("content") or choice.get("reasoning_content") or summary
    return {"path": path, "summary": summary, "token_estimate": payload.token_estimate}


@isolated
async def preprocess_file(path: str) -> dict:
    """Preprocess any file (document, code, image, audio, video) for LLM consumption.

    Uses the full preprocessing pipeline: token-budgeted extraction,
    image resizing, code skeleton maps, audio transcription, etc.
    """
    from kria.preprocessing import preprocess

    payload = await preprocess(path)
    result = {
        "path": path,
        "source_type": payload.source_type,
        "content": payload.text,
        "token_estimate": payload.token_estimate,
        "truncated": payload.truncated,
        **payload.metadata,
    }
    if payload.images:
        result["image_count"] = len(payload.images)
    return result


@isolated
async def extract_video_text(path: str) -> dict:
    """Extract text from a video or audio file (transcription + keyframes).

    Extracts the audio track via FFmpeg, transcribes with faster-whisper,
    and extracts sparse keyframes for scene changes.
    """
    from kria.preprocessing import preprocess

    payload = await preprocess(path)
    return {
        "path": path,
        "source_type": payload.source_type,
        "transcript": payload.text,
        "token_estimate": payload.token_estimate,
        "truncated": payload.truncated,
        "keyframe_count": len(payload.images),
        **payload.metadata,
    }


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("parse_pdf", parse_pdf,
    description="Extract text content from a PDF file.",
    parameters_schema={
        "path": {"type": "string", "description": "Path to PDF file"},
        "max_pages": {"type": "integer", "default": 50},
    })

tool_registry.register("parse_docx", parse_docx,
    description="Extract text and tables from a DOCX file.",
    parameters_schema={
        "path": {"type": "string", "description": "Path to DOCX file"},
    })

tool_registry.register("parse_xlsx", parse_xlsx,
    description="Extract data from an Excel spreadsheet.",
    parameters_schema={
        "path": {"type": "string", "description": "Path to XLSX file"},
        "sheet": {"type": "string", "description": "Sheet name (default: first)", "default": ""},
        "max_rows": {"type": "integer", "default": 100},
    })

tool_registry.register("parse_csv", parse_csv,
    description="Read and analyze a CSV file.",
    parameters_schema={
        "path": {"type": "string", "description": "Path to CSV file"},
        "max_rows": {"type": "integer", "default": 100},
    })

tool_registry.register("summarize_document", summarize_document,
    description="Summarize a document file (PDF, DOCX, TXT, MD, CSV).",
    parameters_schema={
        "path": {"type": "string", "description": "Path to document"},
        "max_length": {"type": "integer", "description": "Max summary words", "default": 200},
    })

tool_registry.register("preprocess_file", preprocess_file,
    description="Preprocess any file (document, code, image, audio, video) for LLM consumption. "
                "Token-budgeted extraction: documents → markdown, code → skeleton maps, "
                "audio/video → transcription, images → resized.",
    parameters_schema={
        "path": {"type": "string", "description": "Path to the file to preprocess"},
    })

tool_registry.register("extract_video_text", extract_video_text,
    description="Extract text from a video or audio file. Transcribes audio via faster-whisper "
                "and extracts sparse keyframes from video for scene changes.",
    parameters_schema={
        "path": {"type": "string", "description": "Path to video or audio file"},
    })
