"""
Model Router
============
Routes inference requests to the correct backend based on the current mode,
intent, and payload content — all driven by config/models.yaml.

No model names are hardcoded.  All routing decisions derive from:
  - ModelsConfig.routing.auto_rules  (intent → model name mapping)
  - ModelsConfig.routing.vision_model (automatic vision payload routing)
  - The current mode (auto | primary | secondary | gemini | external)

Dynamic model switching:
  - set_mode("gemini") → all traffic to Gemini
  - set_mode("auto") → payload-inspected routing
  - reconfigure("gemini", api_key="...") → runtime key change, no restart

Backward compatibility:
  - get_client(intent) preserved for callers that don't pass messages
  - route(intent, messages) is the new preferred entry point
  - gemini_client / external_client aliases preserved for routes.py imports
"""
import logging
from typing import Any, Optional

from kria.agent.config_models import (
    ModelsConfig,
    ModelProfile,
    load_models_config,
)
from kria.agent.inference_client import (
    OpenAIInferenceClient,
    create_client,
)
from kria.agent.payload_inspector import PayloadInspector, PayloadType
from kria.infra.config import settings

logger = logging.getLogger("kria.model_router")


class ModelRouter:
    """
    Config-driven model router.  All model names and routing rules come
    from ModelsConfig (loaded from YAML).  Zero hardcoded model strings.
    """

    def __init__(self, config: Optional[ModelsConfig] = None) -> None:
        self._config = config or load_models_config(settings.models_config_path)
        self._clients: dict[str, OpenAIInferenceClient] = {}
        self._inspector = PayloadInspector(self._config.vision_detection)

        # Determine valid modes from config model names + fixed names
        self._valid_modes: frozenset[str] = frozenset(
            {"auto"} | set(self._config.models.keys())
        )

        # Set initial mode from config
        self._mode: str = self._config.routing.default_mode
        if self._mode not in self._valid_modes:
            self._mode = "auto"

        # Create clients for all configured models
        for name, profile in self._config.models.items():
            self._clients[name] = create_client(profile)

    # ── Public properties ─────────────────────────────────────────

    @property
    def mode(self) -> str:
        return self._mode

    @property
    def VALID_MODES(self) -> frozenset[str]:
        """Backward compat: routes.py reads this."""
        return self._valid_modes

    @property
    def config(self) -> ModelsConfig:
        return self._config

    # ── Mode management ───────────────────────────────────────────

    def set_mode(self, mode: str) -> None:
        if mode not in self._valid_modes:
            raise ValueError(
                f"Invalid mode '{mode}'. Valid: {sorted(self._valid_modes)}"
            )
        self._mode = mode
        logger.info("Routing mode changed → %s", mode)

    # ── Routing ───────────────────────────────────────────────────

    def route(
        self,
        intent: str = "agent_loop",
        messages: Optional[list[dict]] = None,
    ) -> OpenAIInferenceClient:
        """
        Select the inference backend for a request.

        In auto mode:
          1. If messages contain vision data → vision_model
          2. Otherwise → auto_rules[intent] or text_model fallback

        In explicit mode: return the named client directly.
        """
        # Explicit mode: return the named client
        if self._mode != "auto":
            client = self._clients.get(self._mode)
            if client:
                return client
            logger.warning(
                "Mode '%s' has no client — falling back to text_model",
                self._mode,
            )
            return self._get_text_client()

        # Auto mode: inspect payload for vision content
        if messages:
            payload_type = self._inspector.inspect(messages)
            if payload_type == PayloadType.VISION:
                vision_client = self._get_vision_client()
                if vision_client:
                    return vision_client

        # Auto mode: route by intent
        model_name = self._config.routing.auto_rules.get(intent)
        if model_name and model_name in self._clients:
            return self._clients[model_name]

        # Fallback to text model
        return self._get_text_client()

    def get_client(self, intent: str = "agent_loop") -> OpenAIInferenceClient:
        """Backward-compatible: route without message inspection."""
        return self.route(intent=intent, messages=None)

    def get_classification_client(self) -> OpenAIInferenceClient:
        """Return the client designated for intent classification."""
        name = self._config.routing.classification_model
        return self._clients.get(name, self._get_text_client())

    def get_planning_client(self) -> OpenAIInferenceClient:
        """Return the client designated for multi-step planning."""
        name = self._config.routing.planning_model
        return self._clients.get(name, self._get_text_client())

    def _get_text_client(self) -> OpenAIInferenceClient:
        name = self._config.routing.text_model
        client = self._clients.get(name)
        if client:
            return client
        # Last resort: return any client
        return next(iter(self._clients.values()))

    def _get_vision_client(self) -> Optional[OpenAIInferenceClient]:
        name = self._config.routing.vision_model
        return self._clients.get(name)

    # ── Active label (config-driven) ──────────────────────────────

    def active_label(
        self,
        intent: str = "agent_loop",
        messages: Optional[list[dict]] = None,
    ) -> str:
        """Human-readable label for the active model.  Reads from config display_name."""
        client = self.route(intent, messages)
        label = client.model_label
        # Add mode context
        if self._mode != "auto":
            return f"{label} ({self._mode.title()})"
        return label

    # ── Dynamic client management ─────────────────────────────────

    def register_client(
        self, name: str, profile: ModelProfile
    ) -> OpenAIInferenceClient:
        """Add a new model at runtime (e.g. user adds a new API endpoint)."""
        client = create_client(profile)
        self._clients[name] = client
        self._config.models[name] = profile
        # Refresh valid modes
        self._valid_modes = frozenset({"auto"} | set(self._config.models.keys()))
        logger.info("Registered new client: %s (%s)", name, profile.display_name)
        return client

    def reconfigure(self, name: str, **overrides: Any) -> None:
        """
        Update a model's configuration at runtime.

        Example: model_router.reconfigure("gemini", api_key="sk-...", model_id="gemini-1.5-pro")
        """
        client = self._clients.get(name)
        if client is None:
            raise ValueError(
                f"Unknown model '{name}'. Available: {sorted(self._clients.keys())}"
            )
        client.reconfigure(**overrides)
        logger.info("Reconfigured %s: %s", name, list(overrides.keys()))

    # ── Status / introspection ────────────────────────────────────

    def status_dict(self) -> dict:
        """Status dict consumed by GET /settings/llm-mode and the dashboard."""
        models_info = {}
        for name, client in self._clients.items():
            models_info[name] = {
                "display_name": client.model_label,
                "configured": client.is_configured,
                "health_key": client.health_key,
                "capabilities": sorted(client.capabilities),
                "tool_calling": client.tool_calling_mode,
            }

        # Backward-compat fields
        gemini = self._clients.get("gemini")
        external = self._clients.get("external")

        return {
            "mode": self._mode,
            "available_modes": sorted(self._valid_modes),
            "models": models_info,
            "labels": {name: info["display_name"] for name, info in models_info.items()},
            # Backward compat for dashboard
            "gemini_configured": gemini.is_configured if gemini else False,
            "gemini_model": gemini._model_id if gemini else "",
            "external_configured": external.is_configured if external else False,
            "external_url": external._base_url if external else "",
            "external_model": external._model_id if external else "",
        }

    # ── Client access by name ─────────────────────────────────────

    def get_client_by_name(self, name: str) -> Optional[OpenAIInferenceClient]:
        """Get a specific client by config name."""
        return self._clients.get(name)

    @property
    def clients(self) -> dict[str, OpenAIInferenceClient]:
        """Read-only access to all clients."""
        return dict(self._clients)

    async def wait_all_ready(self) -> None:
        """Wait for all configured clients to become ready (startup)."""
        for name, client in self._clients.items():
            profile = self._config.models.get(name)
            if profile and profile.is_local:
                # Local: 15 retries for primary, 3 for others
                retries = 15 if name == self._config.routing.text_model else 3
                await client.wait_for_ready(max_retries=retries, delay=2.0)
            elif client.is_configured:
                # Cloud: just check if configured
                await client.health_check()

    async def close_all(self) -> None:
        """Shutdown all clients (called during app shutdown)."""
        for client in self._clients.values():
            await client.close()


# ── Module initialization ─────────────────────────────────────────
# Load config and create the singleton router.
# Backward-compat aliases are provided for existing imports.

model_router = ModelRouter()

# Backward-compat aliases for routes.py and other modules that import these
gemini_client: Optional[OpenAIInferenceClient] = model_router.get_client_by_name("gemini")
external_client: Optional[OpenAIInferenceClient] = model_router.get_client_by_name("external")

# Legacy classes — kept as type aliases so isinstance() checks in
# not-yet-migrated code don't crash.  They resolve to OpenAIInferenceClient.
GeminiClient = OpenAIInferenceClient
ExternalAPIClient = OpenAIInferenceClient
CloudAPIClient = OpenAIInferenceClient

