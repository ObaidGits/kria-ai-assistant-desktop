"""
News processor — background polling + on-demand search.

Architecture:
  - Polls RSS feeds + GDELT every 15 min in a background thread
  - Deduplicates stories by embedding similarity (sentence-transformers)
  - Source-tiered trust scoring (tier-1 = wire services, tier-3 = blogs)
  - Stores in SQLite at ~/.kria/news.db
  - Exposes: search, fetch_article, list_sources, get_status
"""

import os
import time
import json
import sqlite3
import logging
import hashlib
import threading
import ipaddress
import urllib.request
import urllib.parse
import urllib.error
from datetime import datetime, timezone, timedelta
from typing import Any

logger = logging.getLogger("kria.processors.news")

METHODS = ["search", "fetch_article", "list_sources", "get_status"]

# ── Source registry ────────────────────────────────────────────────────────────

# Tier 1 = major public broadcasters / verified live feeds
# Tier 2 = major newspapers / regional specialists
# (Reuters/AP/AFP RSS feeds are dead as of 2025 — removed)

SOURCES: list[dict] = [
    # Major broadcast networks — tier 1
    {
        "name": "BBC",
        "tier": 1,
        "rss": "http://feeds.bbci.co.uk/news/rss.xml",
        "country": "GB",
        "region": "europe",
        "language": "en",
        "authenticity": "established",
    },
    {
        "name": "CNN",
        "tier": 1,
        "rss": "http://rss.cnn.com/rss/edition.rss",
        "country": "US",
        "region": "north-america",
        "language": "en",
        "authenticity": "established",
    },
    {
        "name": "NPR",
        "tier": 1,
        "rss": "https://feeds.npr.org/1001/rss.xml",
        "country": "US",
        "region": "north-america",
        "language": "en",
        "authenticity": "established",
    },
    {
        "name": "Al Jazeera",
        "tier": 1,
        "rss": "https://www.aljazeera.com/xml/rss/all.xml",
        "country": "QA",
        "region": "middle-east",
        "language": "en",
        "authenticity": "established",
    },
    {
        "name": "DW",
        "tier": 1,
        "rss": "https://rss.dw.com/rdf/rss-en-all",
        "country": "DE",
        "region": "europe",
        "language": "en",
        "authenticity": "established",
    },
    {
        "name": "PBS NewsHour",
        "tier": 1,
        "rss": "https://www.pbs.org/newshour/feeds/rss/headlines",
        "country": "US",
        "region": "north-america",
        "language": "en",
        "authenticity": "established",
    },

    # Major newspapers — tier 2
    {
        "name": "Guardian",
        "tier": 2,
        "rss": "https://www.theguardian.com/world/rss",
        "country": "GB",
        "region": "europe",
        "language": "en",
        "authenticity": "established",
    },
    {
        "name": "NYT",
        "tier": 2,
        "rss": "https://rss.nytimes.com/services/xml/rss/nyt/HomePage.xml",
        "country": "US",
        "region": "north-america",
        "language": "en",
        "authenticity": "established",
    },
    {
        "name": "Washington Post",
        "tier": 2,
        "rss": "https://feeds.washingtonpost.com/rss/national",
        "country": "US",
        "region": "north-america",
        "language": "en",
        "authenticity": "established",
    },
    {
        "name": "Time",
        "tier": 2,
        "rss": "https://time.com/feed/",
        "country": "US",
        "region": "north-america",
        "language": "en",
        "authenticity": "established",
    },
    {
        "name": "Middle East Eye",
        "tier": 2,
        "rss": "https://www.middleeasteye.net/rss",
        "country": "GB",
        "region": "middle-east",
        "language": "en",
        "authenticity": "established",
    },

    # India-focused sources — tier 2
    {
        "name": "The Hindu",
        "tier": 2,
        "rss": "https://www.thehindu.com/news/national/feeder/default.rss",
        "country": "IN",
        "region": "south-asia",
        "language": "en",
        "authenticity": "established",
    },
    {
        "name": "Indian Express",
        "tier": 2,
        "rss": "https://indianexpress.com/section/india/feed/",
        "country": "IN",
        "region": "south-asia",
        "language": "en",
        "authenticity": "established",
    },
    {
        "name": "Hindustan Times",
        "tier": 2,
        "rss": "https://www.hindustantimes.com/feeds/rss/india-news/rssfeed.xml",
        "country": "IN",
        "region": "south-asia",
        "language": "en",
        "authenticity": "established",
    },

    # Science/Tech — tier 2
    {
        "name": "Ars Technica",
        "tier": 2,
        "rss": "https://feeds.arstechnica.com/arstechnica/index",
        "country": "US",
        "region": "north-america",
        "language": "en",
        "authenticity": "established",
    },
    {
        "name": "The Verge",
        "tier": 2,
        "rss": "https://www.theverge.com/rss/index.xml",
        "country": "US",
        "region": "north-america",
        "language": "en",
        "authenticity": "established",
    },
]

# Sources to use for quick DB seeding on first search (reliable + fast)
_SEED_SOURCES = [s for s in SOURCES if s["name"] in ("BBC", "CNN", "Al Jazeera", "DW")]

_SOURCE_META = {
    s["name"]: {
        "country": s.get("country", ""),
        "region": s.get("region", "global"),
        "language": s.get("language", "en"),
        "authenticity": s.get("authenticity", "established"),
    }
    for s in SOURCES
}

_FRESHNESS_DEFAULT_HOURS = {
    "live": 6,
    "recent": 24,
    "archive": 168,
}


def _as_bool(value: Any, default: bool = True) -> bool:
    if isinstance(value, bool):
        return value
    if value is None:
        return default
    if isinstance(value, (int, float)):
        return bool(value)
    text = str(value).strip().lower()
    if text in {"1", "true", "yes", "on"}:
        return True
    if text in {"0", "false", "no", "off"}:
        return False
    return default


def _safe_parse_iso(ts: str) -> datetime | None:
    if not ts:
        return None
    try:
        return datetime.fromisoformat(ts.replace("Z", "+00:00"))
    except Exception:
        return None


def _freshness_score(published: str, now: datetime, mode: str) -> float:
    dt = _safe_parse_iso(published)
    if dt is None:
        return 0.0
    age_hours = max(0.0, (now - dt).total_seconds() / 3600.0)
    half_life = {
        "live": 4.0,
        "recent": 18.0,
        "archive": 72.0,
    }.get(mode, 18.0)
    score = 1.0 / (1.0 + (age_hours / half_life))
    return round(score, 4)


def _region_match_score(meta: dict[str, str], country: str, region: str, language: str) -> int:
    score = 0
    if country and meta.get("country", "").upper() == country:
        score += 3
    if region and meta.get("region", "").lower() == region:
        score += 2
    if language and meta.get("language", "").lower() == language:
        score += 1
    return score


def _validate_safe_url(url: str) -> tuple[bool, str]:
    """Reject non-http(s), local/internal, and private-address URLs."""
    try:
        parsed = urllib.parse.urlparse(url)
    except Exception:
        return False, "invalid URL"

    if parsed.scheme not in {"http", "https"}:
        return False, "unsupported URL scheme"

    if parsed.username or parsed.password:
        return False, "embedded credentials are not allowed"

    host = (parsed.hostname or "").strip().lower()
    if not host:
        return False, "missing host"

    if host in {"localhost", "127.0.0.1", "::1"}:
        return False, "localhost is blocked"
    if host.endswith(".local") or host.endswith(".internal") or host.endswith(".localhost"):
        return False, "local/internal host is blocked"

    try:
        ip = ipaddress.ip_address(host)
        if (
            ip.is_private
            or ip.is_loopback
            or ip.is_link_local
            or ip.is_multicast
            or ip.is_reserved
            or ip.is_unspecified
        ):
            return False, "private/internal IP ranges are blocked"
    except ValueError:
        # Hostname (not a literal IP) is allowed.
        pass

    return True, ""

# GDELT endpoint — returns CSV of top news events
GDELT_URL = "https://api.gdeltproject.org/api/v2/doc/doc?query={query}&mode=ArtList&maxrecords=25&format=json&timespan={timespan}"

# GDELT response cache — keyed by (query, timespan), stores (fetched_at, articles)
_gdelt_cache: dict[tuple, tuple] = {}
_GDELT_CACHE_TTL = 300  # seconds — avoid 429 from repeated identical queries

# ── Database setup ─────────────────────────────────────────────────────────────

_DB_PATH = os.path.expanduser("~/.kria/news.db")
_db_lock = threading.Lock()


def _get_db() -> sqlite3.Connection:
    os.makedirs(os.path.dirname(_DB_PATH), exist_ok=True)
    logger.debug("[db] opening SQLite at %s", _DB_PATH)
    conn = sqlite3.connect(_DB_PATH, check_same_thread=False)
    conn.row_factory = sqlite3.Row
    return conn


def _init_db(conn: sqlite3.Connection) -> None:
    logger.debug("[db] ensuring schema (articles / clusters / poll_log)")
    conn.executescript("""
        CREATE TABLE IF NOT EXISTS articles (
            id          TEXT PRIMARY KEY,
            url         TEXT UNIQUE NOT NULL,
            title       TEXT NOT NULL,
            summary     TEXT,
            body        TEXT,
            source      TEXT,
            source_tier INTEGER DEFAULT 3,
            cluster_id  TEXT,
            published   TEXT,
            fetched_at  TEXT,
            embedding   BLOB
        );

        CREATE INDEX IF NOT EXISTS idx_articles_published ON articles(published DESC);
        CREATE INDEX IF NOT EXISTS idx_articles_cluster   ON articles(cluster_id);
        CREATE INDEX IF NOT EXISTS idx_articles_source    ON articles(source);

        CREATE TABLE IF NOT EXISTS clusters (
            cluster_id      TEXT PRIMARY KEY,
            title           TEXT,
            source_count    INTEGER DEFAULT 1,
            tier1_confirmed INTEGER DEFAULT 0,
            first_seen      TEXT,
            last_seen       TEXT
        );

        CREATE TABLE IF NOT EXISTS poll_log (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            source      TEXT,
            polled_at   TEXT,
            articles_new INTEGER DEFAULT 0
        );
    """)
    conn.commit()
    logger.debug("[db] schema ready")


# ── Embedding helpers ──────────────────────────────────────────────────────────

_embed_model = None
_embed_lock = threading.Lock()


def _get_embedder():
    global _embed_model
    with _embed_lock:
        if _embed_model is None:
            logger.info("[embed] loading sentence-transformers model all-MiniLM-L6-v2 ...")
            try:
                from sentence_transformers import SentenceTransformer
                _embed_model = SentenceTransformer("all-MiniLM-L6-v2")
                logger.info("[embed] model loaded (384-dim)")
            except Exception as e:
                logger.warning("[embed] sentence-transformers unavailable, dedup will use title-hash fallback: %s", e)
                _embed_model = "hash"
    return _embed_model


def _embed_text(text: str) -> list[float] | None:
    """Return a 384-dim embedding or None on failure."""
    model = _get_embedder()
    if model == "hash" or model is None:
        return None
    try:
        vec = model.encode(text, convert_to_numpy=True)
        logger.debug("[embed] encoded %d chars → 384-dim vector", len(text))
        return vec.tolist()
    except Exception as e:
        logger.warning("[embed] encode failed: %s", e)
        return None


def _cosine_similarity(a: list[float], b: list[float]) -> float:
    import math
    dot = sum(x * y for x, y in zip(a, b))
    mag_a = math.sqrt(sum(x * x for x in a))
    mag_b = math.sqrt(sum(x * x for x in b))
    if mag_a == 0 or mag_b == 0:
        return 0.0
    return dot / (mag_a * mag_b)


# ── RSS ingestion ──────────────────────────────────────────────────────────────

def _parse_rss(url: str, source_name: str, source_tier: int) -> list[dict]:
    """Fetch + parse an RSS feed, return list of article dicts."""
    logger.debug("[rss] fetching %s (tier %d): %s", source_name, source_tier, url)
    try:
        import feedparser
        feed = feedparser.parse(url)
        logger.debug("[rss] %s: feed status=%s, %d entries",
                     source_name, getattr(feed, 'status', '?'), len(feed.entries))
        articles = []
        for entry in feed.entries[:30]:
            title = entry.get("title", "").strip()
            link = entry.get("link", "").strip()
            summary = entry.get("summary", "").strip()
            # Parse published date
            published = ""
            if hasattr(entry, "published_parsed") and entry.published_parsed:
                published = datetime(*entry.published_parsed[:6], tzinfo=timezone.utc).isoformat()
            elif hasattr(entry, "updated_parsed") and entry.updated_parsed:
                published = datetime(*entry.updated_parsed[:6], tzinfo=timezone.utc).isoformat()

            if title and link:
                articles.append({
                    "url": link,
                    "title": title,
                    "summary": summary[:500] if summary else "",
                    "source": source_name,
                    "source_tier": source_tier,
                    "published": published,
                })
        logger.debug("[rss] %s: parsed %d valid articles", source_name, len(articles))
        return articles
    except Exception as e:
        logger.warning("[rss] parse failed for %s: %s", source_name, e)
        return []


# ── GDELT ingestion ────────────────────────────────────────────────────────────

def _fetch_gdelt(query: str, timespan: str = "1d") -> list[dict]:
    """Query GDELT for a topic, return article dicts."""
    cache_key = (query.lower().strip(), timespan)
    now_ts = time.monotonic()
    if cache_key in _gdelt_cache:
        cached_at, cached_articles = _gdelt_cache[cache_key]
        age = now_ts - cached_at
        if age < _GDELT_CACHE_TTL:
            logger.info("[gdelt] cache hit for %r (age=%.0fs, %d articles)",
                        query, age, len(cached_articles))
            return cached_articles

    logger.info("[gdelt] querying: query=%r timespan=%s", query, timespan)
    q_enc = urllib.parse.quote(query)
    url = GDELT_URL.format(query=q_enc, timespan=timespan)
    req = urllib.request.Request(url, headers={"User-Agent": "KRIA-News/1.0"})

    data = None
    for attempt in range(3):
        try:
            logger.debug("[gdelt] GET %s (attempt %d/3)", url, attempt + 1)
            with urllib.request.urlopen(req, timeout=10) as resp:
                data = json.loads(resp.read().decode("utf-8"))
            break
        except urllib.error.HTTPError as e:
            if e.code == 429:
                logger.warning("[gdelt] rate-limited (429) — returning empty (retry in %ds)", _GDELT_CACHE_TTL)
                _gdelt_cache[cache_key] = (now_ts, [])
                return []

            # Retry only server-side failures.
            if e.code >= 500 and attempt < 2:
                delay = 0.35 * (2 ** attempt)
                logger.warning("[gdelt] HTTP %s (retrying in %.2fs): %s", e.code, delay, e)
                time.sleep(delay)
                continue
            logger.warning("[gdelt] HTTP error %s: %s", e.code, e)
            return []
        except Exception as e:
            if attempt < 2:
                delay = 0.35 * (2 ** attempt)
                logger.warning("[gdelt] transient failure (retrying in %.2fs): %s", delay, e)
                time.sleep(delay)
                continue
            logger.warning("[gdelt] fetch failed: %s", e)
            return []

    if data is None:
        return []

    articles = []
    for item in (data.get("articles") or []):
        title = item.get("title", "").strip()
        link = item.get("url", "").strip()
        source = item.get("domain", "unknown")
        seendate = item.get("seendate", "")
        # GDELT seendate format: "20260416T120000Z"
        published = ""
        try:
            published = datetime.strptime(seendate, "%Y%m%dT%H%M%SZ").replace(
                tzinfo=timezone.utc).isoformat()
        except Exception:
            pass

        if title and link:
            articles.append({
                "url": link,
                "title": title,
                "summary": "",
                "source": source,
                "source_tier": 3,
                "published": published,
            })
    logger.info("[gdelt] received %d articles for query %r", len(articles), query)
    _gdelt_cache[cache_key] = (now_ts, articles)
    return articles


# ── Deduplication / clustering ─────────────────────────────────────────────────

_CLUSTER_THRESHOLD = 0.82  # cosine similarity above this = same story


def _find_or_create_cluster(conn: sqlite3.Connection, article: dict, embedding: list[float] | None) -> str:
    """Return cluster_id for this article, merging into existing cluster if similar enough."""
    now = datetime.now(timezone.utc).isoformat()

    if embedding is not None:
        # Check recent articles (last 48h) for similarity
        cutoff = (datetime.now(timezone.utc) - timedelta(hours=48)).isoformat()
        rows = conn.execute(
            "SELECT cluster_id, embedding FROM articles WHERE embedding IS NOT NULL AND fetched_at > ? LIMIT 500",
            (cutoff,)
        ).fetchall()
        logger.debug("[cluster] comparing against %d recent embeddings", len(rows))

        for row in rows:
            if row["embedding"] is None:
                continue
            try:
                existing_emb = json.loads(row["embedding"])
                sim = _cosine_similarity(embedding, existing_emb)
                if sim >= _CLUSTER_THRESHOLD:
                    cid = row["cluster_id"]
                    logger.debug("[cluster] merged into existing cluster %s (sim=%.3f): %r",
                                 cid, sim, article["title"][:60])
                    # Update cluster stats
                    conn.execute("""
                        UPDATE clusters SET
                            source_count = source_count + 1,
                            tier1_confirmed = CASE WHEN ? <= 1 THEN 1 ELSE tier1_confirmed END,
                            last_seen = ?
                        WHERE cluster_id = ?
                    """, (article["source_tier"], now, cid))
                    conn.commit()
                    return cid
            except Exception:
                continue

    # New cluster
    cid = hashlib.sha256(article["title"].encode()).hexdigest()[:16]
    logger.debug("[cluster] new cluster %s: %r", cid, article["title"][:60])
    conn.execute("""
        INSERT OR IGNORE INTO clusters (cluster_id, title, source_count, tier1_confirmed, first_seen, last_seen)
        VALUES (?, ?, 1, ?, ?, ?)
    """, (cid, article["title"], 1 if article["source_tier"] <= 1 else 0, now, now))
    conn.commit()
    return cid


def _store_article(conn: sqlite3.Connection, article: dict) -> bool:
    """Store an article, return True if it was new."""
    url = article["url"]
    art_id = hashlib.sha256(url.encode()).hexdigest()[:20]

    # Already stored?
    if conn.execute("SELECT 1 FROM articles WHERE id = ?", (art_id,)).fetchone():
        logger.debug("[store] skip duplicate: %r", article["title"][:60])
        return False

    logger.debug("[store] embedding + clustering: %r", article["title"][:60])
    embedding = _embed_text(article["title"] + " " + article.get("summary", ""))
    cluster_id = _find_or_create_cluster(conn, article, embedding)

    emb_json = json.dumps(embedding) if embedding else None

    conn.execute("""
        INSERT OR IGNORE INTO articles
            (id, url, title, summary, source, source_tier, cluster_id, published, fetched_at, embedding)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    """, (
        art_id,
        url,
        article["title"],
        article.get("summary", ""),
        article["source"],
        article["source_tier"],
        cluster_id,
        article.get("published", ""),
        datetime.now(timezone.utc).isoformat(),
        emb_json,
    ))
    conn.commit()
    logger.debug("[store] saved article %s → cluster %s (source=%s tier=%d)",
                 art_id, cluster_id, article["source"], article["source_tier"])
    return True


def _quick_seed_if_empty(conn: sqlite3.Connection) -> None:
    """If DB is empty, synchronously fetch seed sources so the first search has data."""
    count = conn.execute("SELECT COUNT(*) FROM articles").fetchone()[0]
    if count > 0:
        logger.debug("[seed] DB has %d articles, no seed needed", count)
        return
    logger.info("[seed] DB is empty — seeding from %d fast sources before search", len(_SEED_SOURCES))
    total_new = 0
    for src in _SEED_SOURCES:
        articles = _parse_rss(src["rss"], src["name"], src["tier"])
        new_count = 0
        with _db_lock:
            for art in articles:
                if _store_article(conn, art):
                    new_count += 1
        logger.info("[seed] %s → %d new articles", src["name"], new_count)
        total_new += new_count
    logger.info("[seed] done — %d articles available for search", total_new)


# ── Background poller ──────────────────────────────────────────────────────────

_poller_started = False
_poller_lock = threading.Lock()
_last_poll: dict[str, str] = {}  # source name → ISO timestamp
_POLL_INTERVAL = 900  # 15 minutes


def _poll_once() -> None:
    """Poll all RSS sources and store new articles."""
    poll_start = time.monotonic()
    logger.info("[poll] starting poll cycle (%d sources)", len(SOURCES))
    conn = _get_db()
    _init_db(conn)
    total_new = 0

    for source in SOURCES:
        src_start = time.monotonic()
        try:
            articles = _parse_rss(source["rss"], source["name"], source["tier"])
            new_count = 0
            with _db_lock:
                for art in articles:
                    if _store_article(conn, art):
                        new_count += 1
            elapsed = time.monotonic() - src_start
            _last_poll[source["name"]] = datetime.now(timezone.utc).isoformat()
            conn.execute(
                "INSERT INTO poll_log (source, polled_at, articles_new) VALUES (?, ?, ?)",
                (source["name"], _last_poll[source["name"]], new_count)
            )
            conn.commit()
            logger.info("[poll] %-20s fetched=%2d new=%2d  (%.1fs)",
                        source["name"], len(articles), new_count, elapsed)
            total_new += new_count
        except Exception as e:
            logger.warning("[poll] ERROR for %s: %s", source["name"], e)

    elapsed_total = time.monotonic() - poll_start
    logger.info("[poll] cycle complete — %d new articles, %.1fs total (next in %ds)",
                total_new, elapsed_total, _POLL_INTERVAL)


def _poller_thread() -> None:
    logger.info("[poll] poller thread started (tid=%d)", threading.get_ident())
    while True:
        try:
            _poll_once()
        except Exception as e:
            logger.error("[poll] unhandled poller error: %s", e, exc_info=True)
        logger.debug("[poll] sleeping %ds until next cycle", _POLL_INTERVAL)
        time.sleep(_POLL_INTERVAL)


def _ensure_poller() -> None:
    global _poller_started
    with _poller_lock:
        if not _poller_started:
            logger.info("[poll] launching background poller (interval=%ds)", _POLL_INTERVAL)
            t = threading.Thread(target=_poller_thread, daemon=True, name="news-poller")
            t.start()
            _poller_started = True
            logger.info("[poll] poller running as daemon thread")
        else:
            logger.debug("[poll] poller already running, skip")


# ── Search ─────────────────────────────────────────────────────────────────────

def search(params: dict) -> dict:
    """
    Search stored news articles by topic/keyword.

    Params:
        query:      str  — topic or keywords to search for
        hours:      int  — optional explicit lookback override
        freshness_mode: str — live|recent|archive (default recent)
        min_trust:  int  — minimum source tier: 1=wire services only, 2=major outlets, 3=all sources (default)
        limit:      int  — max results to return (default 10)
        use_gdelt:  bool — also query GDELT live (default True)
        country:    str  — optional ISO country code preference (e.g., IN)
        region:     str  — optional region preference (e.g., south-asia)
        language:   str  — optional language preference (e.g., en)
        source_profile: str — balanced|authentic|global_authentic|india|india_authentic
    """
    _ensure_poller()

    query = params.get("query", "").strip()
    freshness_mode = str(params.get("freshness_mode", "recent")).strip().lower()
    if freshness_mode not in _FRESHNESS_DEFAULT_HOURS:
        freshness_mode = "recent"

    hours_raw = params.get("hours", None)
    if hours_raw is None or str(hours_raw).strip() == "":
        hours = _FRESHNESS_DEFAULT_HOURS[freshness_mode]
    else:
        hours = int(hours_raw)
    hours = max(1, min(hours, 336))

    min_trust = int(params.get("min_trust", 3))  # default 3 = include all tiers incl. GDELT
    limit = max(1, min(int(params.get("limit", 10)), 30))
    use_gdelt = _as_bool(params.get("use_gdelt", True), True)

    country = str(params.get("country", "")).strip().upper()
    region = str(params.get("region", "")).strip().lower()
    language = str(params.get("language", "")).strip().lower()
    source_profile = str(params.get("source_profile", "balanced")).strip().lower()

    if source_profile in {"india", "india_authentic"}:
        if not country:
            country = "IN"
        if not region:
            region = "south-asia"
    if source_profile in {"authentic", "global_authentic", "india_authentic"}:
        min_trust = min(min_trust, 2)

    if not query:
        raise ValueError("query is required")

    logger.info(
        "[search] query=%r hours=%d mode=%s min_trust=%d limit=%d use_gdelt=%s country=%s region=%s language=%s profile=%s",
        query, hours, freshness_mode, min_trust, limit, use_gdelt, country or "-", region or "-", language or "-", source_profile,
    )

    conn = _get_db()
    _init_db(conn)

    # Guarantee data is available before querying
    _quick_seed_if_empty(conn)

    # Optionally fetch live from GDELT and ingest
    if use_gdelt:
        logger.info("[search] step 1/4 — GDELT live fetch")
        gdelt_articles = _fetch_gdelt(query, timespan=f"{hours}h" if hours <= 24 else "1d")
        gdelt_new = 0
        with _db_lock:
            for art in gdelt_articles:
                if _store_article(conn, art):
                    gdelt_new += 1
        logger.info("[search] GDELT ingested %d/%d articles (%d new)",
                    gdelt_new, len(gdelt_articles), gdelt_new)
    else:
        logger.info("[search] step 1/4 — GDELT skipped (use_gdelt=False)")

    # Full-text search across title + summary, filtered by recency and trust
    logger.info("[search] step 2/4 — SQLite keyword search (cutoff=%dh, tier<=%d)", hours, min_trust)
    cutoff = (datetime.now(timezone.utc) - timedelta(hours=hours)).isoformat()
    keywords = [kw.strip() for kw in query.lower().split() if len(kw.strip()) > 2]

    if not keywords:
        keywords = [query.lower().strip()]

    # OR logic: match articles containing ANY keyword, then score by how many match.
    # This prevents AND-filtering from silently dropping relevant articles when a query
    # has 3+ words (e.g. "Israel Iran war" → AND would require all three in one article).
    kw_clauses = " OR ".join(
        f"(lower(a.title) LIKE ? OR lower(a.summary) LIKE ?)" for _ in keywords
    )
    kw_values: list[str] = []
    for kw in keywords:
        kw_values.extend([f"%{kw}%", f"%{kw}%"])

    sql = f"""
        SELECT
            a.id, a.url, a.title, a.summary, a.source, a.source_tier,
            a.cluster_id, a.published,
            c.source_count, c.tier1_confirmed
        FROM articles a
        LEFT JOIN clusters c ON a.cluster_id = c.cluster_id
        WHERE a.source_tier <= ?
          AND a.published >= ?
          AND ({kw_clauses})
        ORDER BY a.source_tier ASC, c.source_count DESC, a.published DESC
        LIMIT ?
    """

    rows = conn.execute(sql, [min_trust, cutoff] + kw_values + [limit * 5]).fetchall()
    logger.info("[search] step 2/4 — DB returned %d raw rows (pre-dedup, pre-score)", len(rows))

    now = datetime.now(timezone.utc)

    # Score each row by how many query keywords appear in title + summary
    def _score(row) -> int:
        text = (row["title"] + " " + (row["summary"] or "")).lower()
        return sum(1 for kw in keywords if kw in text)

    def _rank_tuple(row):
        relevance = _score(row)
        meta = _SOURCE_META.get(row["source"], {})
        region_score = _region_match_score(meta, country, region, language)
        freshness = _freshness_score(row["published"] or "", now, freshness_mode)
        trust_rank = int(row["source_tier"] or 3)
        corroboration = int(row["source_count"] or 1)

        # Live mode prioritizes recency. Archive mode prioritizes relevance/trust.
        if freshness_mode == "live":
            return (-region_score, -freshness, trust_rank, -relevance, -corroboration)
        if freshness_mode == "archive":
            return (-region_score, -relevance, trust_rank, -corroboration, -freshness)
        return (-region_score, -freshness, -relevance, trust_rank, -corroboration)

    scored_rows = sorted(rows, key=_rank_tuple)

    # Deduplicate by cluster: one result per cluster, best article wins
    logger.info("[search] step 3/4 — cluster deduplication")
    seen_clusters: dict[str, dict] = {}
    for row in scored_rows:
        cid = row["cluster_id"] or row["id"]
        if cid not in seen_clusters:
            source_meta = _SOURCE_META.get(row["source"], {})
            freshness = _freshness_score(row["published"] or "", now, freshness_mode)
            region_score = _region_match_score(source_meta, country, region, language)
            seen_clusters[cid] = {
                "title":            row["title"],
                "url":              row["url"],
                "source":           row["source"],
                "source_tier":      row["source_tier"],
                "country":          source_meta.get("country", ""),
                "region":           source_meta.get("region", "global"),
                "language":         source_meta.get("language", "en"),
                "authenticity":     source_meta.get("authenticity", "unverified"),
                "published":        row["published"],
                "summary":          row["summary"],
                "confirmed_by":     row["source_count"] or 1,
                "tier1_confirmed":  bool(row["tier1_confirmed"]),
                "relevance":        _score(row),
                "freshness_score":  freshness,
                "region_match_score": region_score,
            }
    logger.info("[search] after dedup: %d unique stories (from %d rows)",
                len(seen_clusters), len(rows))

    results = list(seen_clusters.values())

    if source_profile in {"authentic", "global_authentic", "india_authentic"}:
        results = [r for r in results if int(r.get("source_tier", 3)) <= 2]

    if source_profile in {"india", "india_authentic"}:
        india_first = [r for r in results if r.get("country") == "IN"]
        non_india = [r for r in results if r.get("country") != "IN"]
        results = india_first + non_india

    results = results[:limit]

    # For multi-word queries, require at least 2 keyword matches to filter noise
    # (single-word queries use score >= 1 automatically since that's the SQL filter)
    min_score = 2 if len(keywords) >= 3 else 1
    results = [r for r in results if r.get("relevance", 0) >= min_score]
    if len(results) < 3 and min_score > 1:
        # Not enough results with strict threshold — relax to score >= 1
        logger.info("[search] strict threshold returned %d results, relaxing to score>=1", len(results))
        results = list(seen_clusters.values())[:limit]

    # Format published time as relative ("2h ago")
    for r in results:
        try:
            pub = datetime.fromisoformat(r["published"].replace("Z", "+00:00"))
            delta = now - pub
            hours_ago = int(delta.total_seconds() / 3600)
            if hours_ago < 1:
                r["age"] = f"{int(delta.total_seconds() / 60)}m ago"
            elif hours_ago < 24:
                r["age"] = f"{hours_ago}h ago"
            else:
                r["age"] = f"{hours_ago // 24}d ago"
        except Exception:
            r["age"] = ""

    # Trust label
    logger.info("[search] step 4/4 — annotating trust + cross-reference labels")
    tier_label = {1: "⭐ Wire service", 2: "📰 Major newspaper", 3: "🌐 Online source"}
    for r in results:
        r["trust"] = tier_label.get(r["source_tier"], "unknown")
        conf = r["confirmed_by"]
        r["cross_referenced"] = f"Confirmed by {conf} source{'s' if conf != 1 else ''}" if conf > 1 else "Single source"
        r["region_match"] = bool(r.get("region_match_score", 0) > 0)

    logger.info("[search] done — returning %d results for %r", len(results), query)
    return {
        "query":        query,
        "results":      results,
        "count":        len(results),
        "hours_searched": hours,
        "freshness_mode": freshness_mode,
        "country": country,
        "region": region,
        "language": language,
        "source_profile": source_profile,
        "min_trust_applied": min_trust,
    }


def fetch_article(params: dict) -> dict:
    """
    Fetch full article body from a URL using trafilatura.

    Params:
        url: str — article URL to extract
    """
    url = params.get("url", "").strip()
    if not url:
        raise ValueError("url is required")

    safe, reason = _validate_safe_url(url)
    if not safe:
        logger.warning("[fetch] rejected unsafe URL %s: %s", url, reason)
        return {"url": url, "error": f"unsafe url: {reason}", "text": ""}

    logger.info("[fetch] fetching article: %s", url)
    try:
        import trafilatura
        logger.debug("[fetch] step 1/3 — HTTP fetch via trafilatura")

        html = None
        for attempt in range(3):
            html = trafilatura.fetch_url(url)
            if html:
                break
            if attempt < 2:
                delay = 0.35 * (2 ** attempt)
                logger.warning("[fetch] empty HTTP response, retrying in %.2fs (attempt %d/3)", delay, attempt + 1)
                time.sleep(delay)

        if not html:
            logger.warning("[fetch] HTTP fetch returned empty — %s", url)
            return {"url": url, "error": "Could not fetch page", "text": ""}
        logger.debug("[fetch] got %d bytes of HTML", len(html))

        logger.debug("[fetch] step 2/3 — extracting metadata")
        meta_obj = None
        try:
            meta_obj = trafilatura.metadata.extract_metadata(html, url)
        except Exception as e:
            logger.debug("[fetch] metadata extraction failed: %s", e)

        logger.debug("[fetch] step 3/3 — extracting main text")
        text = trafilatura.extract(html, include_comments=False,
                                   include_tables=False, output_format="txt", url=url) or ""
        logger.info("[fetch] extracted %d chars from %s", len(text), url)

        metadata = {}
        if meta_obj:
            metadata = {k: v for k, v in {
                "title":       meta_obj.title,
                "author":      meta_obj.author,
                "date":        meta_obj.date,
                "sitename":    meta_obj.sitename,
                "description": meta_obj.description,
            }.items() if v}
            logger.debug("[fetch] metadata: %s", metadata)

        # Store body in DB if we have it
        conn = _get_db()
        _init_db(conn)
        art_id = hashlib.sha256(url.encode()).hexdigest()[:20]
        rows_updated = conn.execute("UPDATE articles SET body = ? WHERE id = ?", (text[:50_000], art_id)).rowcount
        conn.commit()
        if rows_updated:
            logger.debug("[fetch] stored body back to DB for article %s", art_id)
        else:
            logger.debug("[fetch] article %s not in DB (external URL), body not cached", art_id)

        return {
            "url":       url,
            "text":      text[:16_000],
            "metadata":  metadata,
            "char_count": len(text),
        }

    except Exception as e:
        logger.error("[fetch] unhandled error for %s: %s", url, e, exc_info=True)
        return {"url": url, "error": str(e), "text": ""}


def list_sources(params: dict) -> dict:
    """Return the list of news sources being polled."""
    _ensure_poller()
    return {
        "sources": [
            {
                "name":       s["name"],
                "tier":       s["tier"],
                "country":    s.get("country", ""),
                "region":     s.get("region", "global"),
                "language":   s.get("language", "en"),
                "authenticity": s.get("authenticity", "unverified"),
                "last_polled": _last_poll.get(s["name"], "never"),
            }
            for s in SOURCES
        ],
        "poll_interval_secs": _POLL_INTERVAL,
        "total": len(SOURCES),
    }


def get_status(params: dict) -> dict:
    """Return current poller status and DB stats."""
    _ensure_poller()
    try:
        conn = _get_db()
        _init_db(conn)
        total   = conn.execute("SELECT COUNT(*) FROM articles").fetchone()[0]
        sources = conn.execute("SELECT COUNT(DISTINCT source) FROM articles").fetchone()[0]
        fresh   = conn.execute(
            "SELECT COUNT(*) FROM articles WHERE published >= ?",
            ((datetime.now(timezone.utc) - timedelta(hours=24)).isoformat(),)
        ).fetchone()[0]
        return {
            "status":           "running" if _poller_started else "idle",
            "total_articles":   total,
            "sources_tracked":  sources,
            "articles_last_24h": fresh,
            "db_path":          _DB_PATH,
        }
    except Exception as e:
        return {"status": "error", "error": str(e)}
