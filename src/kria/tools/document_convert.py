"""
Document Converter (YELLOW tier — writes files)
=================================================
Convert documents between formats using pandoc or built-in converters.
"""
import asyncio
import logging
from pathlib import Path

from kria.infra.isolation import ToolResult, isolated
from kria.infra.platform_detect import has_command
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.document_convert")


@isolated
async def convert_document(input_path: str, output_format: str, output_path: str = "") -> dict:
    """Convert a document between formats (e.g., MD→PDF, DOCX→PDF, XLSX→CSV)."""
    src = Path(input_path)
    if not src.exists():
        return {"error": f"File not found: {input_path}"}

    if not output_path:
        output_path = str(src.with_suffix(f".{output_format}"))

    # Use pandoc if available
    if has_command("pandoc"):
        proc = await asyncio.create_subprocess_exec(
            "pandoc", str(src), "-o", output_path,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        _, stderr = await proc.communicate()
        if proc.returncode == 0:
            return {"input": input_path, "output": output_path, "format": output_format}
        return {"error": f"Pandoc failed: {stderr.decode(errors='replace')}"}

    # Fallback: handle specific conversions
    if src.suffix == ".xlsx" and output_format == "csv":
        import pandas as pd
        df = pd.read_excel(input_path)
        df.to_csv(output_path, index=False)
        return {"input": input_path, "output": output_path, "format": "csv"}

    if src.suffix in (".md", ".txt") and output_format == "html":
        text = src.read_text(encoding="utf-8", errors="replace")
        html = f"<html><body><pre>{text}</pre></body></html>"
        Path(output_path).write_text(html, encoding="utf-8")
        return {"input": input_path, "output": output_path, "format": "html"}

    return {
        "error": f"Cannot convert {src.suffix} → .{output_format}. Install pandoc for more format support."
    }


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("convert_document", convert_document,
    description="Convert a document between formats (e.g., MD→PDF, DOCX→PDF, XLSX→CSV).",
    parameters_schema={
        "input_path": {"type": "string", "description": "Source file path"},
        "output_format": {"type": "string", "description": "Target format: pdf, docx, txt, html, csv, md"},
        "output_path": {"type": "string", "description": "Output file path", "default": ""},
    })
