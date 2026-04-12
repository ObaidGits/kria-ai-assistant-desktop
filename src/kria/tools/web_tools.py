"""
Web Tools (GREEN tier — read-only network requests)
====================================================
web_search   → DuckDuckGo HTML scraping (no API key required)
fetch_webpage → Extract main text via trafilatura
get_weather  → wttr.in JSON API (no API key required)
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


@isolated
async def web_search(query: str, max_results: int = 5) -> dict:
    """Search the web via DuckDuckGo HTML endpoint (no API key required)."""
    encoded = urllib.parse.quote_plus(query)
    url = f"https://html.duckduckgo.com/html/?q={encoded}"
    headers = {"User-Agent": "Mozilla/5.0 (compatible; KRIA/1.0)"}
    try:
        async with httpx.AsyncClient(timeout=_HTTP_TIMEOUT, follow_redirects=True) as client:
            resp = await client.get(url, headers=headers)
            resp.raise_for_status()
    except Exception as exc:
        return {"error": str(exc), "results": []}

    # Parse result snippets from raw HTML (no external HTML parser required)
    import re
    html = resp.text
    results = []
    # DuckDuckGo result structure: class="result__a" and class="result__snippet"
    titles = re.findall(r'class="result__a"[^>]*>(.*?)</a>', html, re.DOTALL)
    snippets = re.findall(r'class="result__snippet"[^>]*>(.*?)</div>', html, re.DOTALL)
    urls_raw = re.findall(r'class="result__url"[^>]*>(.*?)</a>', html, re.DOTALL)

    def _strip_tags(text: str) -> str:
        return re.sub(r"<[^>]+>", "", text).strip()

    for i in range(min(max_results, len(titles))):
        results.append({
            "rank": i + 1,
            "title": _strip_tags(titles[i]),
            "snippet": _strip_tags(snippets[i]) if i < len(snippets) else "",
            "url": _strip_tags(urls_raw[i]) if i < len(urls_raw) else "",
        })

    return {"query": query, "results": results, "count": len(results)}


@isolated
async def fetch_webpage(
    url: str,
    include_headers: bool = False,
    include_links: bool = False,
) -> dict:
    """Fetch and extract the main text content of a webpage using trafilatura."""
    if not url.startswith(("http://", "https://")):
        return {"url": url, "content": "", "error": "Invalid URL scheme — only http:// and https:// allowed"}
    try:
        import trafilatura
        async with httpx.AsyncClient(timeout=_HTTP_TIMEOUT, follow_redirects=True) as client:
            resp = await client.get(url, headers={"User-Agent": "Mozilla/5.0 (compatible; KRIA/1.0)"})
            resp.raise_for_status()
        html = resp.text
        text = trafilatura.extract(html, include_links=include_links, include_tables=True)
        return {
            "url": url,
            "content": (text or "")[:_MAX_CONTENT],
            "truncated": len(text or "") > _MAX_CONTENT,
            "status_code": resp.status_code,
        }
    except ImportError:
        # Fallback: strip tags manually
        async with httpx.AsyncClient(timeout=_HTTP_TIMEOUT, follow_redirects=True) as client:
            resp = await client.get(url, headers={"User-Agent": "Mozilla/5.0 (compatible; KRIA/1.0)"})
        import re
        text = re.sub(r"<[^>]+>", " ", resp.text)
        text = " ".join(text.split())
        return {"url": url, "content": text[:_MAX_CONTENT], "truncated": True, "status_code": resp.status_code}
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
            resp = await client.get(url, headers={"User-Agent": "Mozilla/5.0 (compatible; KRIA/1.0)"})
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
