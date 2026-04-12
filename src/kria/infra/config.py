"""
K.R.I.A. Centralized Configuration
====================================
All settings are loaded from environment variables (prefixed KRIA_).
A .env file is automatically read if present.
"""
from pathlib import Path

from pydantic import model_validator
from pydantic_settings import BaseSettings


class KriaSettings(BaseSettings):
    model_config = {"env_prefix": "KRIA_", "env_file": ".env", "extra": "ignore"}

    # ── Service URLs ──────────────────────────────────────────────
    llama_api_url: str = "http://localhost:8080"
    whisper_api_url: str = "http://localhost:8081"
    piper_api_url: str = "http://localhost:8082"
    redis_url: str = "redis://localhost:6379/0"
    chroma_url: str = "http://localhost:8083"
    bridge_url: str = "http://localhost:9000"

    # ── Bridge authentication ─────────────────────────────────────
    # bridge_secret_file is preferred in Docker (mounted secret)
    # bridge_secret can be set directly from env / .env
    bridge_secret: str = ""
    bridge_secret_file: str = ""  # e.g. /run/secrets/bridge_secret

    @model_validator(mode="after")
    def _load_bridge_secret_from_file(self) -> "KriaSettings":
        """Read the bridge secret from a file when KRIA_BRIDGE_SECRET_FILE is set."""
        if self.bridge_secret_file and not self.bridge_secret:
            try:
                secret = Path(self.bridge_secret_file).read_text().strip()
                object.__setattr__(self, "bridge_secret", secret)
            except OSError:
                pass
        return self

    # ── Per-service aliases used by individual modules ────────────
    @property
    def whisper_url(self) -> str:
        return self.whisper_api_url

    @property
    def piper_url(self) -> str:
        return self.piper_api_url

    # ── ChromaDB connection details ───────────────────────────────
    chromadb_host: str = "localhost"
    chromadb_port: int = 8083
    chromadb_path: str = "./data/chroma"

    # ── Paths ─────────────────────────────────────────────────────
    sqlite_path: str = "./data/kria.db"
    rollback_dir: str = "~/.kria/rollback"
    audit_log_path: str = "./data/audit.db"
    log_dir: str = "./data/logs"

    # ── Safety ───────────────────────────────────────────────────
    default_risk_level: str = "RED"
    emergency_mode: bool = False

    # ── Limits / Tuning ───────────────────────────────────────────
    max_context_turns: int = 20
    tool_timeout_seconds: float = 30.0
    hitl_timeout_seconds: float = 30.0
    rollback_retention_hours: int = 72
    rollback_max_size_gb: float = 5.0
    redis_cache_ttl_seconds: int = 60
    max_concurrent_tools: int = 3

    # ── Voice / Wake word ─────────────────────────────────────────
    voice_enabled: bool = True
    wake_word: str = "hey kria"
    wake_energy_threshold: float = 500.0
    vad_silence_ms: int = 1000
    porcupine_access_key: str = ""
    # ── Web UI ────────────────────────────────────────────────────
    # Partial label of the microphone to prefer in the web UI.
    # Leave blank to use the browser default device.
    # Example: KRIA_MIC_DEVICE_LABEL="Yeti" matches any mic whose label
    # contains "Yeti" (case-insensitive).
    mic_device_label: str = ""

settings = KriaSettings()
