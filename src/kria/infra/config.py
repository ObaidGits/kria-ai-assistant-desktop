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
    llama_api_url: str = "http://localhost:8080"          # primary brain (Phi-4-mini)
    llama_secondary_api_url: str = "http://localhost:8085" # secondary brain (Qwen2.5-VL-7B)
    whisper_api_url: str = "http://localhost:8081"
    piper_api_url: str = "http://localhost:8082"
    redis_url: str = "redis://localhost:6379/0"
    chroma_url: str = "http://localhost:8083"
    bridge_url: str = "http://localhost:9000"

    # ── LLM Routing ───────────────────────────────────────────────
    # Controls which backend handles inference requests.
    #   auto      — AGENT_LOOP intent → secondary; all others → primary
    #   primary   — always Phi-4-mini (fast, low VRAM)
    #   secondary — always Qwen2.5-VL-7B (smart, needs more VRAM)
    #   gemini    — bypass local models; use Google Gemini API
    #   external  — any OpenAI-compatible hosted API (Groq, OpenRouter, etc.)
    llm_mode: str = "auto"
    gemini_api_key: str = ""
    gemini_model: str = "gemini-2.0-flash"

    # ── External (OpenAI-compatible) API ──────────────────────────
    # Works with Groq, OpenRouter, Together AI, Mistral, Perplexity, LM Studio, Ollama, etc.
    # Example: KRIA_EXTERNAL_API_URL=https://api.groq.com/openai/v1
    external_api_url: str = ""
    external_api_key: str = ""
    external_api_model: str = ""

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

    # ── Qdrant (Mem0 vector store) ────────────────────────────────
    qdrant_url: str = "http://localhost:6333"

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
    interaction_timeout_seconds: float = 20.0
    rollback_retention_hours: int = 72
    rollback_max_size_gb: float = 5.0
    redis_cache_ttl_seconds: int = 60
    max_concurrent_tools: int = 3

    # ── Internet ──────────────────────────────────────────────────
    internet_enabled: bool = True
    internet_https_only: bool = True
    internet_max_response_mb: int = 50
    internet_rate_limit_per_min: int = 60
    internet_search_cache_ttl: int = 3600
    internet_page_cache_ttl: int = 86400
    max_download_size_mb: int = 500

    # ── Voice / Wake word ─────────────────────────────────────────
    voice_enabled: bool = True
    wake_word: str = "hey kria"
    wake_energy_threshold: float = 500.0
    vad_silence_ms: int = 1000
    porcupine_access_key: str = ""

    # ── Language ──────────────────────────────────────────────────
    language: str = "en"
    language_auto_detect: bool = False

    # ── Paths (expanded) ──────────────────────────────────────────
    plugins_dir: str = "~/.kria/plugins"
    workflows_dir: str = "~/.kria/workflows"
    snippets_dir: str = "~/.kria/snippets"
    downloads_dir: str = "~/Downloads/kria"
    knowledge_dir: str = "~/.kria/knowledge"

    # ── Automation ────────────────────────────────────────────────
    automation_enabled: bool = True
    max_scheduled_tasks: int = 50
    max_workflows: int = 20

    # ── Plugins ───────────────────────────────────────────────────
    plugins_enabled: bool = True

    # ── Notifications ─────────────────────────────────────────────
    notifications_enabled: bool = True

    # ── Telegram ──────────────────────────────────────────────────
    telegram_enabled: bool = False
    telegram_bot_token: str = ""
    telegram_allowed_chat_ids: list[int] = []

    # ── Web UI ────────────────────────────────────────────────────
    mic_device_label: str = ""

    # ── Preprocessing Pipeline ─────────────────────────────────────
    preprocessing_enabled: bool = True
    preprocessing_max_tokens: int = 3500
    preprocessing_image_max_edge: int = 1280
    preprocessing_image_grayscale: bool = False
    preprocessing_keyframe_max: int = 5
    preprocessing_scene_threshold: float = 0.3

    # ── Safety Demo ───────────────────────────────────────────────
    safety_demo_mode: bool = False

    # ── Model Configuration (YAML) ────────────────────────────────
    models_config_path: str = "./config/models.yaml"

    # ── MCP (Model Context Protocol) ──────────────────────────────
    mcp_enabled: bool = False
    mcp_config_path: str = "~/.kria/mcp_servers.json"
    mcp_default_risk_level: str = "RED"
    mcp_connection_timeout: float = 10.0
    mcp_tool_timeout: float = 30.0

    # ── HITL Terminal Mode ────────────────────────────────────────
    hitl_terminal_mode: bool = False

settings = KriaSettings()
