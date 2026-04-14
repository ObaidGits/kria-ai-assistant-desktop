"""
Web Tools (GREEN tier — read-only network requests)
====================================================
web_search    → DuckDuckGo HTML scraping (no API key required)
fetch_webpage → Extract main text via trafilatura
get_weather   → wttr.in JSON API (no API key required)
deep_search   → Combined search: DuckDuckGo discovery + Trafilatura extraction
"""
import logging
import urllib.parse
from typing import Optional

import httpx

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.web_tools")

_HTTP_TIMEOUT = 15.0
_MAX_CONTENT = 20 * 1024  # 20 KB cap on response content

_BROWSER_UA = (
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36"
)


@isolated
async def web_search(query: str, max_results: int = 5) -> dict:
    """Search the web via DuckDuckGo (no API key required)."""
    import asyncio
    try:
        from duckduckgo_search import DDGS

        def _do_search():
            with DDGS() as ddgs:
                return list(ddgs.text(query, max_results=max_results))

        raw = await asyncio.to_thread(_do_search)
        results = []
        for i, r in enumerate(raw):
            results.append({
                "rank": i + 1,
                "title": r.get("title", ""),
                "snippet": r.get("body", ""),
                "url": r.get("href", ""),
            })
        return {"query": query, "results": results, "count": len(results)}
    except ImportError:
        return {"error": "duckduckgo-search package not installed", "results": []}
    except Exception as exc:
        logger.warning("web_search failed: %s", exc)
        return {"error": str(exc), "results": []}


@isolated
async def fetch_webpage(
    url: str,
    include_headers: bool = False,
    include_links: bool = False,
) -> dict:
    """Fetch and extract the main text content of a webpage using the preprocessing pipeline."""
    if not url.startswith(("http://", "https://")):
        return {"url": url, "content": "", "error": "Invalid URL scheme — only http:// and https:// allowed"}
    try:
        from kria.preprocessing.web import preprocess_url
        payload = await preprocess_url(url, include_links=include_links)
        return {
            "url": url,
            "content": payload.text,
            "truncated": payload.truncated,
            "token_estimate": payload.token_estimate,
            "parser": payload.metadata.get("parser", "unknown"),
            "status_code": payload.metadata.get("status_code", 0),
        }
    except Exception as exc:
        return {"url": url, "content": "", "error": str(exc)}


@isolated
async def get_weather(location: str, units: str = "metric") -> dict:
    """
    Fetch weather from wttr.in (no API key required).
    units: 'metric' (°C) | 'imperial' (°F) | 'si'
    """
    unit_flag = "m" if units == "metric" else ("u" if units == "imperial" else "")
    encoded = urllib.parse.quote_plus(location)
    url = f"https://wttr.in/{encoded}?format=j1&{unit_flag}"
    try:
        async with httpx.AsyncClient(timeout=_HTTP_TIMEOUT) as client:
            resp = await client.get(url, headers={"User-Agent": _BROWSER_UA})
            resp.raise_for_status()
            data = resp.json()
        current = data["current_condition"][0]
        return {
            "location": location,
            "temp_c": current.get("temp_C"),
            "temp_f": current.get("temp_F"),
            "feels_like_c": current.get("FeelsLikeC"),
            "description": current.get("weatherDesc", [{}])[0].get("value", ""),
            "humidity": current.get("humidity"),
            "visibility_km": current.get("visibility"),
            "wind_kmph": current.get("windspeedKmph"),
        }
    except Exception as exc:
        return {"error": str(exc), "location": location}


@isolated
async def deep_search(query: str, max_pages: int = 3) -> dict:
    """
    Deep web search: DuckDuckGo discovery + Trafilatura extraction.

    1. Runs a DuckDuckGo search to find relevant URLs.
    2. Fetches the top results and extracts clean text via Trafilatura.
    3. Returns structured results with full article content as markdown.

    Use this when a user asks for detailed/current information from the web.
    """
    import asyncio
    import re

    # Step 1: DuckDuckGo discovery via duckduckgo-search library
    try:
        from duckduckgo_search import DDGS

        def _do_search():
            with DDGS() as ddgs:
                return list(ddgs.text(query, max_results=max_pages + 2))

        raw = await asyncio.to_thread(_do_search)
    except ImportError:
        return {"error": "duckduckgo-search package not installed", "results": []}
    except Exception as exc:
        return {"error": f"Search failed: {exc}", "results": []}

    # Collect valid URLs
    discovered: list[dict] = []
    for r in raw:
        url = r.get("href", "")
        if url and url.startswith("http"):
            discovered.append({
                "title": r.get("title", ""),
                "url": url,
                "snippet": r.get("body", ""),
            })

    if not discovered:
        return {"query": query, "results": [], "error": "No results found", "count": 0}

    # Step 2: Extract content via preprocessing pipeline from top results
    results: list[dict] = []
    from kria.preprocessing.web import preprocess_url

    for item in discovered[:max_pages]:
        try:
            payload = await preprocess_url(item["url"], max_tokens=3500)
            results.append({
                "title": item["title"],
                "url": item["url"],
                "snippet": item["snippet"],
                "content": payload.text[:_MAX_CONTENT],
                "extracted": bool(payload.text),
                "token_estimate": payload.token_estimate,
            })
        except Exception as exc:
            logger.debug("deep_search: failed to extract %s: %s", item["url"], exc)
            results.append({
                "title": item["title"],
                "url": item["url"],
                "snippet": item["snippet"],
                "content": "",
                "error": str(exc),
            })

    return {
        "query": query,
        "results": results,
        "count": len(results),
        "extraction_method": "preprocessing_pipeline",
    }


# ── Register ─────────────────────────────────────────────────────

tool_registry.register("web_search", web_search,
    description="Search the web for a query using DuckDuckGo. Returns titles, snippets, and URLs.",
    parameters_schema={"query": {"type": "string"}, "max_results": {"type": "integer", "default": 5}})
tool_registry.register("fetch_webpage", fetch_webpage,
    description="Fetch and extract the main text content of a web page.",
    parameters_schema={"url": {"type": "string"}, "include_links": {"type": "boolean", "default": False}})
tool_registry.register("get_weather", get_weather,
    description="Get current weather for a location. No API key required.",
    parameters_schema={"location": {"type": "string"}, "units": {"type": "string", "default": "metric"}})
tool_registry.register("deep_search", deep_search,
    description="Deep web search: searches DuckDuckGo, then extracts full article text from top results via Trafilatura. Use for detailed/current information.",
    parameters_schema={"query": {"type": "string", "description": "Search query"}, "max_pages": {"type": "integer", "default": 3, "description": "Number of pages to extract (1-5)"}})
