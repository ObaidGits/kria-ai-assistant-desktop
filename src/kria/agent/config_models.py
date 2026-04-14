"""
Model Configuration Schema
===========================
Pydantic models that parse and validate config/models.yaml.
All model-specific variables are externalized — the core router
contains zero hardcoded model names.
"""
import os
import re
from pathlib import Path
from typing import Optional

import yaml
from pydantic import BaseModel, Field, field_validator


# ── Env-var interpolation ─────────────────────────────────────────

_ENV_RE = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*)(?::-(.*?))?\}")


def _interpolate_env(value: str) -> str:
    """Replace ${VAR:-default} patterns with environment variable values."""
    def _replace(m: re.Match) -> str:
        var_name = m.group(1)
        default = m.group(2) if m.group(2) is not None else ""
        return os.environ.get(var_name, default)
    return _ENV_RE.sub(_replace, value)


def _interpolate_recursive(obj):
    """Walk a nested dict/list and interpolate all string values."""
    if isinstance(obj, str):
        return _interpolate_env(obj)
    if isinstance(obj, dict):
        return {k: _interpolate_recursive(v) for k, v in obj.items()}
    if isinstance(obj, list):
        return [_interpolate_recursive(item) for item in obj]
    return obj


# ── Pydantic models ───────────────────────────────────────────────

class ModelProfile(BaseModel):
    """Configuration for a single inference backend."""
    endpoint: str = ""
    api_key: str = "no-key-required"
    model_id: str = ""
    display_name: str = ""
    context_window: int = 4096
    max_tokens: int = 2048
    capabilities: list[str] = Field(default_factory=lambda: ["text"])
    health_key: str = ""
    tool_calling: str = "prompt_based"  # "prompt_based" | "native_api"
    max_iterations: int = 10
    rate_limit_rpm: Optional[int] = None

    @field_validator("tool_calling")
    @classmethod
    def _validate_tool_calling(cls, v: str) -> str:
        if v not in ("prompt_based", "native_api"):
            raise ValueError(f"tool_calling must be 'prompt_based' or 'native_api', got '{v}'")
        return v

    @property
    def supports_vision(self) -> bool:
        return "vision" in self.capabilities

    @property
    def is_local(self) -> bool:
        return "localhost" in self.endpoint or "127.0.0.1" in self.endpoint


class RoutingConfig(BaseModel):
    """Controls which model handles each type of request."""
    default_mode: str = "auto"
    text_model: str = "primary"
    vision_model: str = "secondary"
    planning_model: str = "primary"
    classification_model: str = "primary"
    auto_rules: dict[str, str] = Field(default_factory=lambda: {
        "agent_loop": "secondary",
        "direct_tool": "primary",
        "conversation": "primary",
    })


class VisionDetectionConfig(BaseModel):
    """Patterns used by PayloadInspector to detect vision content."""
    image_extensions: list[str] = Field(default_factory=lambda: [
        ".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".tiff",
    ])
    base64_prefix: str = "data:image/"
    path_pattern: str = r"\.(png|jpe?g|gif|webp|bmp|tiff)$"


class ValidationConfig(BaseModel):
    """Controls for the LLM response validation layer."""
    max_correction_attempts: int = 2
    strict_schema_check: bool = True


class ModelsConfig(BaseModel):
    """Root configuration loaded from config/models.yaml."""
    models: dict[str, ModelProfile] = Field(default_factory=dict)
    routing: RoutingConfig = Field(default_factory=RoutingConfig)
    vision_detection: VisionDetectionConfig = Field(default_factory=VisionDetectionConfig)
    validation: ValidationConfig = Field(default_factory=ValidationConfig)


# ── Loader ────────────────────────────────────────────────────────

def load_models_config(path: str | Path = "config/models.yaml") -> ModelsConfig:
    """
    Load and validate the models configuration from YAML.

    Environment variables are interpolated before Pydantic validation:
      ${KRIA_LLAMA_API_URL:-http://localhost:8080}  →  <env value or default>
    """
    path = Path(path)
    if not path.exists():
        # Return sane defaults so the system can start without a config file
        return ModelsConfig()

    raw = yaml.safe_load(path.read_text(encoding="utf-8"))
    if not raw:
        return ModelsConfig()

    interpolated = _interpolate_recursive(raw)
    return ModelsConfig.model_validate(interpolated)
