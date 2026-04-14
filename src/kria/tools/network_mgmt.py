"""
Network Management Tools (GREEN tier — read-only)
===================================================
Ping, DNS lookup, public IP, URL status check.
"""
import asyncio
import logging
import socket

import httpx

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.network_mgmt")


@isolated
async def ping_host(host: str, count: int = 4) -> dict:
    """Ping a host and return round-trip time stats."""
    proc = await asyncio.create_subprocess_exec(
        "ping", "-c", str(count), host,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=30.0)
    return {
        "host": host,
        "output": stdout.decode(errors="replace"),
        "reachable": proc.returncode == 0,
    }


@isolated
async def dns_lookup(hostname: str) -> dict:
    """Perform DNS lookup for a hostname."""
    try:
        results = socket.getaddrinfo(hostname, None)
        ips = list(set(r[4][0] for r in results))
        return {"hostname": hostname, "ips": ips, "count": len(ips)}
    except socket.gaierror as e:
        return {"hostname": hostname, "error": str(e)}


@isolated
async def get_public_ip() -> dict:
    """Get public IP address and basic geo info."""
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await client.get("https://ipinfo.io/json")
        return resp.json()


@isolated
async def check_url_status(url: str) -> dict:
    """Check if a URL is reachable (HTTP HEAD request)."""
    try:
        async with httpx.AsyncClient(timeout=10.0, follow_redirects=True) as client:
            resp = await client.head(url)
            return {
                "url": url,
                "status_code": resp.status_code,
                "reachable": resp.status_code < 400,
                "content_type": resp.headers.get("content-type", ""),
            }
    except Exception as e:
        return {"url": url, "reachable": False, "error": str(e)}


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("ping_host", ping_host,
    description="Ping a host and return round-trip time stats.",
    parameters_schema={
        "host": {"type": "string", "description": "Hostname or IP"},
        "count": {"type": "integer", "default": 4},
    })

tool_registry.register("dns_lookup", dns_lookup,
    description="Perform DNS lookup for a hostname.",
    parameters_schema={
        "hostname": {"type": "string", "description": "Domain to look up"},
    })

tool_registry.register("get_public_ip", get_public_ip,
    description="Get public IP address and basic geo info.")

tool_registry.register("check_url_status", check_url_status,
    description="Check if a URL is reachable (HTTP HEAD request).",
    parameters_schema={
        "url": {"type": "string", "description": "URL to check"},
    })
