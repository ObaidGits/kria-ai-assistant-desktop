"""
API Tools (GREEN tier — read-only HTTP requests)
==================================================
Generic REST API consumer for making HTTP requests.
"""
import json
import logging

import httpx

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.api_tools")

_MAX_RESPONSE = 50 * 1024  # 50 KB cap on response body


@isolated
async def http_request(
    url: str,
    method: str = "GET",
    headers: dict | None = None,
    body: str = "",
    timeout: float = 15.0,
) -> dict:
    """Make an HTTP request and return the response."""
    method = method.upper()
    if method not in ("GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"):
        return {"error": f"Unsupported method: {method}"}

    kwargs: dict = {
        "method": method,
        "url": url,
        "headers": headers or {},
        "timeout": timeout,
        "follow_redirects": True,
    }

    if body and method in ("POST", "PUT", "PATCH"):
        try:
            json_body = json.loads(body)
            kwargs["json"] = json_body
        except (json.JSONDecodeError, TypeError):
            kwargs["content"] = body

    async with httpx.AsyncClient() as client:
        resp = await client.request(**kwargs)  # type: ignore[arg-type]

        content_type = resp.headers.get("content-type", "")
        raw = resp.text[:_MAX_RESPONSE]

        return {
            "status_code": resp.status_code,
            "content_type": content_type,
            "body": raw,
            "headers": dict(resp.headers),
        }


@isolated
async def get_stock_price(symbol: str) -> dict:
    """Get current stock price info (via free Yahoo Finance endpoint)."""
    url = f"https://query1.finance.yahoo.com/v8/finance/chart/{symbol}?interval=1d&range=1d"
    headers = {"User-Agent": "Mozilla/5.0 (compatible; KRIA/2.0)"}
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await client.get(url, headers=headers)
        if resp.status_code != 200:
            return {"error": f"HTTP {resp.status_code}", "symbol": symbol}
        data = resp.json()

    try:
        meta = data["chart"]["result"][0]["meta"]
        return {
            "symbol": symbol.upper(),
            "price": meta.get("regularMarketPrice"),
            "previous_close": meta.get("previousClose"),
            "currency": meta.get("currency"),
            "exchange": meta.get("exchangeName"),
        }
    except (KeyError, IndexError):
        return {"error": "Could not parse stock data", "symbol": symbol}


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("http_request", http_request,
    description="Make an HTTP request (GET, POST, etc.) and return the response.",
    parameters_schema={
        "url": {"type": "string", "description": "Target URL"},
        "method": {"type": "string", "description": "HTTP method", "default": "GET"},
        "headers": {"type": "object", "description": "Request headers", "default": {}},
        "body": {"type": "string", "description": "Request body (JSON or text)", "default": ""},
    })

tool_registry.register("get_stock_price", get_stock_price,
    description="Get current stock price for a ticker symbol.",
    parameters_schema={
        "symbol": {"type": "string", "description": "Stock ticker symbol (e.g. AAPL, MSFT)"},
    })
