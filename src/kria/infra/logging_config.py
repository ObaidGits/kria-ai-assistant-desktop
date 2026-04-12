"""
Structured Logging Configuration
=================================
Call ``setup_logging()`` once at process startup (inside FastAPI lifespan).

- Console handler: human-readable with colour-like prefixes.
- File handler: newline-delimited JSON with automatic rotation (10 MB × 5 files).

All K.R.I.A. modules use ``logging.getLogger("kria.<module>")`` so the
root "kria" logger controls everything from one place.
"""
import json
import logging
import logging.handlers
from datetime import datetime, timezone


class _JSONFormatter(logging.Formatter):
    """Emit each log record as a single JSON line."""

    def format(self, record: logging.LogRecord) -> str:
        entry: dict = {
            "ts": datetime.now(timezone.utc).isoformat(timespec="milliseconds"),
            "level": record.levelname,
            "logger": record.name,
            "msg": record.getMessage(),
        }
        if record.exc_info:
            entry["exc"] = self.formatException(record.exc_info)
        return json.dumps(entry, ensure_ascii=False)


def setup_logging(log_level: str = "INFO", log_file: str = "kria.log") -> None:
    root = logging.getLogger("kria")
    root.setLevel(getattr(logging, log_level.upper(), logging.INFO))

    # Avoid adding duplicate handlers on hot-reload
    if root.handlers:
        return

    # ── Console — human readable ──────────────────────────────────
    ch = logging.StreamHandler()
    ch.setFormatter(
        logging.Formatter("[%(asctime)s] %(levelname)-8s %(name)-28s %(message)s")
    )
    root.addHandler(ch)

    # ── File — JSON, rotating ─────────────────────────────────────
    fh = logging.handlers.RotatingFileHandler(
        log_file, maxBytes=10_000_000, backupCount=5, encoding="utf-8"
    )
    fh.setFormatter(_JSONFormatter())
    root.addHandler(fh)
