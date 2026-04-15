"""
Web processor — Pre-Cognitive web article extraction.

Extracts clean article text, metadata, and links from web pages
using trafilatura and readability-lxml. Tier-aware depth.
"""

import logging
from typing import Any

logger = logging.getLogger("kria.processors.web")

METHODS = ["extract"]

_MAX_CHARS = {"lite": 4_000, "standard": 16_000, "performance": 64_000, "high": 256_000}


def extract(params: dict) -> dict:
    """
    Extract article content from HTML or a URL.

    Params:
        url: str — URL to fetch (optional if html provided)
        html: str — raw HTML content (optional if url provided)
        max_chars: int — override char budget (optional)
    """
    url = params.get("url", "")
    html = params.get("html", "")
    tier = params.get("_tier", "standard")
    max_chars = params.get("max_chars", _MAX_CHARS.get(tier, 16_000))

    if not url and not html:
        raise ValueError("Either 'url' or 'html' must be provided")

    result: dict[str, Any] = {}
    if url:
        result["url"] = url

    # Fetch HTML if only URL provided
    if url and not html:
        try:
            import trafilatura
            html = trafilatura.fetch_url(url)
            if not html:
                return {"url": url, "error": "Failed to fetch URL", "text": ""}
        except Exception as e:
            return {"url": url, "error": f"Fetch error: {e}", "text": ""}

    # Primary extraction: trafilatura
    text = ""
    metadata: dict[str, Any] = {}
    try:
        import trafilatura

        extracted = trafilatura.extract(
            html,
            include_comments=False,
            include_tables=(tier in ("performance", "high")),
            include_links=(tier in ("performance", "high")),
            output_format="txt",
            url=url or None,
        )
        if extracted:
            text = extracted

        # Metadata extraction
        meta_raw = trafilatura.extract(
            html,
            output_format="xml",
            url=url or None,
        )
        # Parse basic metadata from trafilatura
        meta_obj = trafilatura.metadata.extract_metadata(html, url or None)
        if meta_obj:
            metadata = {
                "title": meta_obj.title or "",
                "author": meta_obj.author or "",
                "date": meta_obj.date or "",
                "sitename": meta_obj.sitename or "",
                "description": meta_obj.description or "",
            }
            # Remove empty keys
            metadata = {k: v for k, v in metadata.items() if v}
    except Exception as e:
        logger.warning("trafilatura extraction failed: %s", e)

    # Fallback: readability-lxml
    if not text:
        try:
            from readability import Document as ReadabilityDoc
            import re

            doc = ReadabilityDoc(html)
            raw_html = doc.summary()
            # Strip HTML tags
            text = re.sub(r"<[^>]+>", " ", raw_html)
            text = re.sub(r"\s+", " ", text).strip()
            if not metadata.get("title"):
                metadata["title"] = doc.title()
        except Exception as e:
            logger.warning("readability fallback failed: %s", e)

    # Truncate
    truncated = False
    if len(text) > max_chars:
        text = text[:max_chars] + "\n[TRUNCATED]"
        truncated = True

    # Extract links if high tier
    links = []
    if tier in ("performance", "high"):
        try:
            import re
            href_pattern = re.compile(r'href=["\']([^"\']+)["\']')
            raw_links = href_pattern.findall(html[:500_000])
            seen = set()
            for link in raw_links:
                if link.startswith(("http://", "https://")) and link not in seen:
                    seen.add(link)
                    links.append(link)
                if len(links) >= 50:
                    break
        except Exception:
            pass

    result.update({
        "text": text,
        "metadata": metadata,
        "char_count": len(text),
        "truncated": truncated,
    })
    if links:
        result["links"] = links

    # Summary
    title = metadata.get("title", "Untitled")
    result["summary"] = f"{title} | {len(text)} chars"

    return result
