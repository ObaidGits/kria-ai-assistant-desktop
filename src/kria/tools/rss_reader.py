"""
RSS Feed Reader (GREEN tier — read-only network)
==================================================
Parse RSS/Atom feeds and return structured entries.
"""
import logging

import httpx

from kria.infra.isolation import ToolResult, isolated
from kria.tools.registry import tool_registry

logger = logging.getLogger("kria.tools.rss_reader")


@isolated
async def rss_feed_read(url: str, max_entries: int = 10) -> dict:
    """Read an RSS/Atom feed and return recent entries."""
    import feedparser

    async with httpx.AsyncClient(timeout=15.0) as client:
        resp = await client.get(url)
        resp.raise_for_status()

    feed = feedparser.parse(resp.text)
    entries = []
    for entry in feed.entries[:max_entries]:
        entries.append({
            "title": entry.get("title", ""),
            "link": entry.get("link", ""),
            "published": entry.get("published", ""),
            "summary": entry.get("summary", "")[:500],
        })

    return {
        "feed_title": feed.feed.get("title", ""),
        "entries": entries,
        "count": len(entries),
    }


@isolated
async def get_news(category: str = "general", max_items: int = 5) -> dict:
    """Get latest news headlines from well-known RSS feeds."""
    import feedparser

    feeds = {
        "general": "https://feeds.bbci.co.uk/news/rss.xml",
        "tech": "https://feeds.arstechnica.com/arstechnica/index",
        "science": "https://rss.nytimes.com/services/xml/rss/nyt/Science.xml",
        "business": "https://feeds.bbci.co.uk/news/business/rss.xml",
        "world": "https://feeds.bbci.co.uk/news/world/rss.xml",
        "sports": "https://feeds.bbci.co.uk/sport/rss.xml",
        "health": "https://rss.nytimes.com/services/xml/rss/nyt/Health.xml",
        "entertainment": "https://feeds.bbci.co.uk/news/entertainment_and_arts/rss.xml",
        "ai": "https://feeds.arstechnica.com/arstechnica/technology-lab",
        "linux": "https://www.phoronix.com/rss.php",
    }
    feed_url = feeds.get(category, feeds["general"])

    async with httpx.AsyncClient(timeout=15.0) as client:
        resp = await client.get(feed_url)
        resp.raise_for_status()

    feed = feedparser.parse(resp.text)
    entries = []
    for entry in feed.entries[:max_items]:
        entries.append({
            "title": entry.get("title", ""),
            "link": entry.get("link", ""),
            "published": entry.get("published", ""),
            "summary": entry.get("summary", "")[:300],
        })

    return {
        "category": category,
        "feed_title": feed.feed.get("title", ""),
        "entries": entries,
        "count": len(entries),
    }


# ── Register ──────────────────────────────────────────────────────

tool_registry.register("rss_feed_read", rss_feed_read,
    description="Read an RSS/Atom feed and return recent entries.",
    parameters_schema={
        "url": {"type": "string", "description": "Feed URL"},
        "max_entries": {"type": "integer", "description": "Max entries", "default": 10},
    })

tool_registry.register("get_news", get_news,
    description="Get latest news headlines from RSS feeds.",
    parameters_schema={
        "category": {"type": "string", "description": "News category: general, tech, science, business, world, sports, health, entertainment, ai, linux", "default": "general"},
        "max_items": {"type": "integer", "default": 5},
    })
