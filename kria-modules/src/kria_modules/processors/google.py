"""
Google Workspace context buffer processor.

Receives raw MCP tool output (typically large JSON from Google APIs) and
compresses it into concise, LLM-friendly digests (~500-800 tokens).

Architecture:
  - Called by the Rust side *after* an MCP tool returns raw Google data
  - Does NOT call Google APIs itself — the MCP server handles that
  - Pure transformation: raw JSON → compact summary

Methods:
  - summarize_email_thread: Thread of emails → digest
  - extract_doc: Google Doc JSON → structured extract
  - extract_sheet: Sheets data → compact table representation
  - extract_slides: Slides JSON → outline
  - summarize_drive_folder: Drive listing → organized summary
"""

import json
import logging
from typing import Any

logger = logging.getLogger("kria.processors.google")

METHODS = [
    "summarize_email_thread",
    "extract_doc",
    "extract_sheet",
    "extract_slides",
    "summarize_drive_folder",
]

# ── Token budget control ──────────────────────────────────────────────

MAX_SUMMARY_CHARS = 3000  # ~750 tokens


def _truncate(text: str, max_chars: int = MAX_SUMMARY_CHARS) -> str:
    if len(text) <= max_chars:
        return text
    return text[:max_chars] + "…[truncated]"


# ── Email Thread Summarizer ───────────────────────────────────────────

def summarize_email_thread(params: dict) -> dict:
    """Compress a Gmail thread (list of messages) into a concise digest.

    Expected params:
        raw: str | list — raw MCP output (JSON string or parsed list of messages)
    """
    raw = params.get("raw", "")
    messages = _parse_raw(raw)

    if not messages:
        return {"summary": "(empty thread)", "message_count": 0}

    # If the MCP server returned plain text (not JSON), return it directly.
    if isinstance(messages, str):
        return {"summary": _truncate(messages), "message_count": 0}

    # If it's a single message dict, wrap it
    if isinstance(messages, dict):
        # google-workspace-mcp wraps list results under "messages" key
        if "messages" in messages:
            messages = messages["messages"]
        else:
            messages = [messages]

    # Defensive: ensure it's iterable as a list of dicts
    if not isinstance(messages, list):
        return {"summary": _truncate(str(messages)), "message_count": 0}

    lines = []
    for i, msg in enumerate(messages):
        if not isinstance(msg, dict):
            lines.append(f"[{i+1}] {str(msg)[:200]}")
            continue
        sender = _extract_field(msg, ["from", "sender", "From"])
        subject = _extract_field(msg, ["subject", "Subject"])
        date = _extract_field(msg, ["date", "Date", "internalDate"])
        snippet = _extract_field(msg, ["snippet", "body", "text", "bodyText"])

        header = f"[{i+1}] From: {sender}"
        if subject:
            header += f" | Subject: {subject}"
        if date:
            header += f" | {date}"

        body = _trim_body(snippet, max_chars=400)
        lines.append(f"{header}\n{body}")

    summary = "\n---\n".join(lines)
    return {
        "summary": _truncate(summary),
        "message_count": len(messages),
    }


# ── Google Doc Extractor ──────────────────────────────────────────────

def extract_doc(params: dict) -> dict:
    """Extract structured content from a Google Docs JSON response.

    Expected params:
        raw: str | dict — raw MCP output
    """
    raw = params.get("raw", "")
    doc = _parse_raw(raw)

    if not doc:
        return {"summary": "(empty document)", "title": ""}

    title = _extract_field(doc, ["title", "name", "Title"]) or "Untitled"

    # Try to find body content
    body_content = doc.get("body", {}).get("content", [])
    if not body_content and isinstance(doc, str):
        return {"summary": _truncate(doc), "title": title}

    paragraphs = []
    for element in body_content:
        paragraph = element.get("paragraph", {})
        elems = paragraph.get("elements", [])
        text_parts = []
        for e in elems:
            text_run = e.get("textRun", {})
            content = text_run.get("content", "")
            if content.strip():
                text_parts.append(content.strip())
        if text_parts:
            paragraphs.append(" ".join(text_parts))

    if not paragraphs:
        # Fallback: try plain text
        text = _extract_field(doc, ["text", "content", "body"])
        if text:
            paragraphs = [text]

    body = "\n\n".join(paragraphs) if paragraphs else "(no content)"
    return {
        "summary": _truncate(body),
        "title": title,
        "paragraph_count": len(paragraphs),
    }


# ── Google Sheets Extractor ──────────────────────────────────────────

def extract_sheet(params: dict) -> dict:
    """Convert Sheets data into a compact table representation.

    Expected params:
        raw: str | dict — raw MCP output (spreadsheet data)
        max_rows: int — max rows to include (default 50)
    """
    raw = params.get("raw", "")
    max_rows = params.get("max_rows", 50)
    data = _parse_raw(raw)

    if not data:
        return {"summary": "(empty spreadsheet)", "sheets": []}

    sheets = data.get("sheets", [])
    if not sheets and isinstance(data, list):
        # Direct cell data
        return _format_cell_grid(data, max_rows)

    results = []
    for sheet in sheets[:5]:  # max 5 sheets
        props = sheet.get("properties", {})
        title = props.get("title", "Sheet")
        grid_data = sheet.get("data", [])

        rows = []
        for grid in grid_data:
            for row in grid.get("rowData", [])[:max_rows]:
                cells = []
                for cell in row.get("values", []):
                    val = cell.get("formattedValue", cell.get("effectiveValue", {}).get("stringValue", ""))
                    cells.append(str(val) if val else "")
                rows.append(cells)

        # Format as simple table
        if rows:
            # Use first row as header if it looks like one
            header = " | ".join(rows[0]) if rows else ""
            data_rows = [" | ".join(r) for r in rows[1:]]
            table_text = f"**{title}**\n{header}\n" + "-" * 40 + "\n" + "\n".join(data_rows)
        else:
            table_text = f"**{title}** (empty)"

        results.append({"sheet": title, "rows": len(rows), "preview": _truncate(table_text, 800)})

    combined = "\n\n".join(r["preview"] for r in results)
    return {
        "summary": _truncate(combined),
        "sheets": results,
    }


def _format_cell_grid(data: list, max_rows: int) -> dict:
    rows = data[:max_rows]
    lines = [" | ".join(str(c) for c in row) if isinstance(row, list) else str(row) for row in rows]
    return {
        "summary": _truncate("\n".join(lines)),
        "row_count": len(data),
        "truncated": len(data) > max_rows,
    }


# ── Google Slides Extractor ──────────────────────────────────────────

def extract_slides(params: dict) -> dict:
    """Convert Slides presentation into an outline.

    Expected params:
        raw: str | dict — raw MCP output
    """
    raw = params.get("raw", "")
    pres = _parse_raw(raw)

    if not pres:
        return {"summary": "(empty presentation)", "slide_count": 0}

    title = _extract_field(pres, ["title", "name"]) or "Untitled Presentation"
    slides = pres.get("slides", [])

    outline = [f"# {title}\n"]
    for i, slide in enumerate(slides):
        slide_texts = []
        for element in slide.get("pageElements", []):
            shape = element.get("shape", {})
            text_el = shape.get("text", {})
            for text_elem in text_el.get("textElements", []):
                text_run = text_elem.get("textRun", {})
                content = text_run.get("content", "").strip()
                if content:
                    slide_texts.append(content)

        text = " | ".join(slide_texts) if slide_texts else "(blank slide)"
        outline.append(f"Slide {i+1}: {text}")

    body = "\n".join(outline)
    return {
        "summary": _truncate(body),
        "title": title,
        "slide_count": len(slides),
    }


# ── Drive Folder Summarizer ──────────────────────────────────────────

def summarize_drive_folder(params: dict) -> dict:
    """Organize a Drive listing into a structured summary.

    Expected params:
        raw: str | dict | list — raw MCP output (file listing)
    """
    raw = params.get("raw", "")
    data = _parse_raw(raw)

    if not data:
        return {"summary": "(empty folder)", "file_count": 0}

    files = data.get("files", data) if isinstance(data, dict) else data
    if not isinstance(files, list):
        return {"summary": str(data)[:500], "file_count": 0}

    # Group by MIME type
    by_type: dict[str, list] = {}
    for f in files:
        mime = f.get("mimeType", "unknown") if isinstance(f, dict) else "unknown"
        category = _mime_category(mime)
        by_type.setdefault(category, []).append(f)

    lines = []
    for cat, items in sorted(by_type.items()):
        lines.append(f"\n**{cat}** ({len(items)} items)")
        for item in items[:10]:
            if isinstance(item, dict):
                name = item.get("name", "?")
                modified = item.get("modifiedTime", "")[:10]
                owner = ""
                owners = item.get("owners", [])
                if owners and isinstance(owners[0], dict):
                    owner = f" by {owners[0].get('displayName', '?')}"
                lines.append(f"  - {name} ({modified}{owner})")
            else:
                lines.append(f"  - {item}")
        if len(items) > 10:
            lines.append(f"  … and {len(items) - 10} more")

    summary = "\n".join(lines)
    return {
        "summary": _truncate(summary),
        "file_count": len(files),
        "categories": {k: len(v) for k, v in by_type.items()},
    }


# ── Helpers ───────────────────────────────────────────────────────────

def _parse_raw(raw: Any) -> Any:
    """Parse raw input — could be JSON string, dict, or list."""
    if isinstance(raw, (dict, list)):
        return raw
    if isinstance(raw, str):
        raw = raw.strip()
        if not raw:
            return None
        try:
            return json.loads(raw)
        except json.JSONDecodeError:
            return raw  # plain text
    return None


def _extract_field(obj: Any, keys: list[str]) -> str:
    """Try multiple field names, return first non-empty value."""
    if not isinstance(obj, dict):
        return ""
    for key in keys:
        val = obj.get(key)
        if val:
            return str(val).strip()
    return ""


def _trim_body(text: str, max_chars: int = 400) -> str:
    """Trim email/document body text."""
    if not text:
        return "(no body)"
    text = text.strip()
    if len(text) > max_chars:
        return text[:max_chars] + "…"
    return text


def _mime_category(mime: str) -> str:
    """Map MIME type to human-friendly category."""
    if "folder" in mime:
        return "Folders"
    if "document" in mime or "doc" in mime:
        return "Documents"
    if "spreadsheet" in mime or "sheet" in mime:
        return "Spreadsheets"
    if "presentation" in mime or "slide" in mime:
        return "Presentations"
    if "image" in mime:
        return "Images"
    if "pdf" in mime:
        return "PDFs"
    if "video" in mime:
        return "Videos"
    return "Other"
