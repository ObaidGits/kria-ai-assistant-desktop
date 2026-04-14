"""
Web & DOM Preprocessing Module
================================
Intercept URLs, strip all DOM bloat (navigation, scripts, ads), and return
only core article/data content as clean markdown — token-budgeted.

Primary: Trafilatura (already a KRIA dependency).
Optional fallback: Crawl4AI for JavaScript-heavy SPAs.
"""
from __future__ import annotations

import asyncio
import logging
import re
from typing import Optional

import httpx

logger = logging.getLogger("kria.preprocessing.web")

_HTTP_TIMEOUT = 15.0
_BROWSER_UA = (
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36"
)


async def preprocess_url(
    url: str,
    *,
    max_tokens: int = 3500,
    include_tables: bool = True,
    include_links: bool = False,
) -> "PreprocessedPayload":
    """Fetch a URL and extract clean markdown content within token budget.

    1. Fetch HTML (async httpx).
    2. Trafilatura extraction (tables yes, images/links stripped).
    3. Fallback: Crawl4AI or regex tag stripping.
    4. Smart-crop to *max_tokens*.
    """
    from kria.preprocessing.dispatcher import PreprocessedPayload
    from kria.preprocessing.token_budget import estimate_tokens, smart_crop

    if not url.startswith(("http://", "https://")):
        return PreprocessedPayload(
            text="",
            source_type="web",
            metadata={"url": url, "error": "Invalid URL scheme"},
        )

    html, status, error = await _fetch_html(url)
    if error:
        return PreprocessedPayload(
            text="",
            source_type="web",
            metadata={"url": url, "status_code": status, "error": error},
        )

    # Primary: Trafilatura extraction
    text = await _extract_trafilatura(
        html, include_tables=include_tables, include_links=include_links
    )

    # Fallback: Crawl4AI
    if not text:
        text = await _extract_crawl4ai(url)

    # Last resort: regex strip
    if not text:
        text = _strip_html_tags(html)

    parser = "trafilatura" if text else "fallback"
    text = text or ""

    # Token budget enforcement
    text, truncated = smart_crop(text, max_tokens)
    tokens = estimate_tokens(text)

    return PreprocessedPayload(
        text=text,
        token_estimate=tokens,
        source_type="web",
        metadata={
            "url": url,
            "status_code": status,
            "parser": parser,
            "char_count": len(text),
        },
        truncated=truncated,
    )


# ---------------------------------------------------------------------------
# HTML fetch
# ---------------------------------------------------------------------------

async def _fetch_html(url: str) -> tuple[str, int, Optional[str]]:
    """Fetch raw HTML. Returns (html, status_code, error_or_None)."""
    try:
        async with httpx.AsyncClient(
            timeout=_HTTP_TIMEOUT, follow_redirects=True
        ) as client:
            resp = await client.get(url, headers={"User-Agent": _BROWSER_UA})
            resp.raise_for_status()
            return resp.text, resp.status_code, None
    except httpx.HTTPStatusError as exc:
        return "", exc.response.status_code, f"HTTP {exc.response.status_code}"
    except Exception as exc:
        return "", 0, str(exc)


# ---------------------------------------------------------------------------
# Extractors
# ---------------------------------------------------------------------------

async def _extract_trafilatura(
    html: str,
    include_tables: bool = True,
    include_links: bool = False,
) -> str:
    """Run trafilatura in a thread (it's CPU-bound)."""
    try:
        import trafilatura
    except ImportError:
        return ""

    def _run():
        return trafilatura.extract(
            html,
            include_tables=include_tables,
            include_links=include_links,
            include_images=False,
            output_format="txt",
        ) or ""

    return await asyncio.to_thread(_run)


async def _extract_crawl4ai(url: str) -> str:
    """Optional Crawl4AI extraction for JS-heavy pages."""
    try:
        from crawl4ai import AsyncWebCrawler
    except ImportError:
        return ""

    try:
        async with AsyncWebCrawler() as crawler:
            result = await crawler.arun(url=url)
            return result.markdown or ""
    except Exception as exc:
        logger.debug("Crawl4AI fallback failed for %s: %s", url, exc)
        return ""


def _strip_html_tags(html: str) -> str:
    """Last-resort: strip all HTML tags via regex."""
    # Remove script/style blocks
    text = re.sub(r"<(script|style)[^>]*>.*?</\1>", "", html, flags=re.DOTALL | re.IGNORECASE)
    # Remove tags
    text = re.sub(r"<[^>]+>", " ", text)
    # Collapse whitespace
    text = re.sub(r"\s+", " ", text).strip()
    return text
