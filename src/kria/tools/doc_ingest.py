"""
Document Ingestion (GREEN tier)
================================
Chunk and embed documents into ChromaDB for RAG.
"""
import logging
import uuid
from pathlib import Path

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.doc_ingest")


@isolated
async def ingest_document(path: str, chunk_size: int = 512) -> dict:
    """Ingest a document into the knowledge base for future Q&A."""
    from kria.memory.semantic import semantic_memory

    ext = Path(path).suffix.lower()
    text = ""

    if ext == ".pdf":
        from kria.tools.document_parser import parse_pdf
        result = await parse_pdf(path)
        if isinstance(result, ToolResult) and result.success:
            text = "\n".join(p["text"] for p in result.data["content"])
    elif ext == ".docx":
        from kria.tools.document_parser import parse_docx
        result = await parse_docx(path)
        if isinstance(result, ToolResult) and result.success:
            text = "\n".join(result.data["paragraphs"])
    else:
        text = Path(path).read_text(encoding="utf-8", errors="replace")

    if not text:
        return {"error": "Could not extract text from document"}

    # Chunk text with overlap
    overlap = min(50, chunk_size // 4)
    chunks = [text[i:i + chunk_size] for i in range(0, len(text), chunk_size - overlap)]

    # Store in ChromaDB
    for i, chunk in enumerate(chunks):
        if chunk.strip():
            await semantic_memory.add(
                text=chunk,
                metadata={"source": path, "chunk": i, "type": "document"},
                doc_id=f"doc_{uuid.uuid4().hex[:8]}",
            )

    return {"path": path, "chunks_ingested": len(chunks), "total_chars": len(text)}


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("ingest_document", ingest_document,
    description="Ingest a document into the knowledge base for future Q&A.",
    parameters_schema={
        "path": {"type": "string", "description": "Path to document"},
        "chunk_size": {"type": "integer", "default": 512},
    })
