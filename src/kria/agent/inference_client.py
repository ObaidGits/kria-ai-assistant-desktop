"""
Unified Inference Client
========================
A single client class that handles all backends (local llama.cpp,
Gemini, Groq, OpenRouter, etc.) through the OpenAI-compatible API.

This replaces the old separate LLMClient / CloudAPIClient / GeminiClient /
ExternalAPIClient classes.  All behavior differences are driven by the
``ModelProfile`` config — no isinstance checks required.

Features:
  - Circuit breaker for local models (reuses kria.infra.circuit_breaker)
  - Sliding-window rate limiter for cloud APIs
  - Background health probes with automatic circuit reset
  - Runtime reconfiguration (API key, URL, model_id, etc.)
"""
from __future__ import annotations

import asyncio
import json
import logging
import time
from collections import deque
from typing import Any, AsyncIterator, Optional, Protocol, runtime_checkable

import httpx

from kria.agent.config_models import ModelProfile
from kria.infra.circuit_breaker import CircuitBreaker, CircuitState
from kria.infra.health import ServiceStatus, health_registry

logger = logging.getLogger("kria.inference")

_FALLBACK = (
    "I'm having trouble reaching my reasoning engine right now. "
    "Please try again in a moment."
)


# ── Protocol (the contract) ───────────────────────────────────────

@runtime_checkable
class InferenceClient(Protocol):
    """Interface every inference backend must satisfy."""

    @property
    def model_label(self) -> str: ...
    @property
    def capabilities(self) -> set[str]: ...
    @property
    def tool_calling_mode(self) -> str: ...
    @property
    def max_iterations(self) -> int: ...
    @property
    def health_key(self) -> str: ...
    @property
    def is_configured(self) -> bool: ...

    async def chat(
        self,
        messages: list[dict],
        tools: Optional[list[dict]] = None,
        temperature: float = 0.6,
        max_tokens: int = 2048,
        tool_choice: str = "auto",
    ) -> Optional[dict]: ...

    async def chat_stream(
        self,
        messages: list[dict],
        tools: Optional[list[dict]] = None,
        temperature: float = 0.6,
        max_tokens: int = 2048,
    ) -> AsyncIterator[str]: ...

    async def health_check(self) -> bool: ...
    async def wait_for_ready(self, max_retries: int, delay: float) -> bool: ...
    async def close(self) -> None: ...
    def reconfigure(self, **overrides: Any) -> None: ...


# ── Rate limiter ──────────────────────────────────────────────────

class RateLimiter:
    """Sliding-window rate limiter.  Not thread-safe but fine for asyncio."""

    def __init__(self, rpm: int) -> None:
        self._rpm = rpm
        self._window: deque[float] = deque()

    async def acquire(self) -> None:
        now = time.monotonic()
        # Purge entries older than 60 s
        while self._window and self._window[0] < now - 60:
            self._window.popleft()
        if len(self._window) >= self._rpm:
            wait = 60 - (now - self._window[0]) + 0.1
            logger.debug("Rate limiter: sleeping %.1fs", wait)
            await asyncio.sleep(wait)
        self._window.append(time.monotonic())


# ── Concrete implementation ───────────────────────────────────────

class OpenAIInferenceClient:
    """
    Universal OpenAI-compatible inference client.

    Backed by httpx (matching the existing codebase style).
    All behavior — circuit breaking, rate limiting, health probing —
    is conditional on properties read from ``ModelProfile``.
    """

    def __init__(self, profile: ModelProfile) -> None:
        self._profile = profile
        self._base_url = profile.endpoint.rstrip("/")
        self._api_key = profile.api_key
        self._model_id = profile.model_id
        self._display_name = profile.display_name
        self._capabilities = set(profile.capabilities)
        self._tool_calling_mode = profile.tool_calling
        self._max_iterations = profile.max_iterations
        self._health_key = profile.health_key
        self._context_window = profile.context_window
        self._max_tokens = profile.max_tokens

        self._http = httpx.AsyncClient(
            timeout=httpx.Timeout(connect=10.0, read=120.0, write=30.0, pool=5.0),
            limits=httpx.Limits(max_connections=10),
        )

        # Circuit breaker: only for local models
        self._circuit: Optional[CircuitBreaker] = None
        if profile.is_local:
            self._circuit = CircuitBreaker(
                name=profile.health_key,
                failure_threshold=5,
                recovery_timeout=30.0,
                fallback=None,
            )

        # Rate limiter: only for cloud models with rpm configured
        self._rate_limiter: Optional[RateLimiter] = None
        if profile.rate_limit_rpm:
            self._rate_limiter = RateLimiter(profile.rate_limit_rpm)

        # Register with health system
        if profile.health_key:
            health_registry.register(profile.health_key)

        self._probe_task: Optional[asyncio.Task] = None

    # ── Properties ────────────────────────────────────────────────

    @property
    def model_label(self) -> str:
        return self._display_name or self._model_id

    @property
    def capabilities(self) -> set[str]:
        return self._capabilities

    @property
    def tool_calling_mode(self) -> str:
        return self._tool_calling_mode

    @property
    def max_iterations(self) -> int:
        return self._max_iterations

    @property
    def health_key(self) -> str:
        return self._health_key

    @property
    def is_configured(self) -> bool:
        """True if this client has a non-empty endpoint and model_id."""
        return bool(self._base_url) and bool(self._model_id)

    # ── Health ────────────────────────────────────────────────────

    async def health_check(self) -> bool:
        if self._profile.is_local:
            return await self._local_health_check()
        # Cloud: configured == healthy (can't ping most cloud APIs for free)
        ok = self.is_configured
        if self._health_key:
            health_registry.update(
                self._health_key,
                ServiceStatus.HEALTHY if ok else ServiceStatus.DOWN,
            )
        return ok

    async def _local_health_check(self) -> bool:
        # Strip /v1 suffix for health endpoint (llama-server serves /health at root)
        base = self._base_url
        if base.endswith("/v1"):
            base = base[:-3]
        try:
            resp = await self._http.get(f"{base}/health", timeout=5.0)
            ok = resp.status_code == 200
            if self._health_key:
                health_registry.update(
                    self._health_key,
                    ServiceStatus.HEALTHY if ok else ServiceStatus.DEGRADED,
                )
            return ok
        except Exception as exc:
            if self._health_key:
                health_registry.update(self._health_key, ServiceStatus.DOWN, str(exc))
            logger.warning("Health check failed (%s): %s", self._health_key, exc)
            return False

    async def wait_for_ready(self, max_retries: int = 15, delay: float = 2.0) -> bool:
        for attempt in range(1, max_retries + 1):
            if await self.health_check():
                logger.info(
                    "Backend ready (%s, attempt %d/%d)",
                    self._health_key, attempt, max_retries,
                )
                if self._profile.is_local:
                    self._probe_task = asyncio.ensure_future(
                        self._periodic_health_probe()
                    )
                return True
            logger.info(
                "Backend not ready (%s) — retrying in %.0fs (%d/%d)",
                self._health_key, delay, attempt, max_retries,
            )
            await asyncio.sleep(delay)
        logger.warning(
            "Backend not reachable after %d attempts (%s)", max_retries, self._health_key
        )
        if self._profile.is_local:
            self._probe_task = asyncio.ensure_future(self._periodic_health_probe())
        return False

    async def _periodic_health_probe(self, interval: float = 15.0) -> None:
        while True:
            try:
                await asyncio.sleep(interval)
                ok = await self.health_check()
                if ok and self._circuit and self._circuit.state.value != "closed":
                    async with self._circuit._lock:
                        self._circuit._failure_count = 0
                        self._circuit._state = CircuitState.CLOSED
                    logger.info(
                        "Health probe: %s recovered — circuit breaker reset",
                        self._health_key,
                    )
            except asyncio.CancelledError:
                break
            except Exception as exc:
                logger.debug("Health probe error (%s): %s", self._health_key, exc)

    # ── Chat (non-streaming) ──────────────────────────────────────

    async def chat(
        self,
        messages: list[dict],
        tools: Optional[list[dict]] = None,
        temperature: float = 0.6,
        max_tokens: int = 2048,
        tool_choice: str = "auto",
    ) -> Optional[dict]:
        if self._circuit:
            return await self._chat_local(
                messages, tools, temperature, max_tokens, tool_choice
            )
        return await self._chat_cloud(
            messages, tools, temperature, max_tokens, tool_choice
        )

    async def _chat_local(
        self,
        messages: list[dict],
        tools: Optional[list[dict]],
        temperature: float,
        max_tokens: int,
        tool_choice: str,
    ) -> Optional[dict]:
        payload = self._build_payload(messages, tools, temperature, max_tokens, tool_choice)

        async def _do_request() -> dict:
            resp = await self._http.post(
                f"{self._base_url}/chat/completions", json=payload
            )
            resp.raise_for_status()
            return resp.json()

        result = await self._circuit.call(_do_request)
        if result is None:
            logger.warning(
                "Chat returned None (%s, circuit=%s, failures=%d/%d)",
                self._health_key,
                self._circuit.state.value,
                self._circuit._failure_count,
                self._circuit.failure_threshold,
            )
            if self._health_key:
                health_registry.update(
                    self._health_key, ServiceStatus.DOWN, "circuit open or request failed"
                )
        else:
            if self._health_key:
                health_registry.update(self._health_key, ServiceStatus.HEALTHY)
        return result

    async def _chat_cloud(
        self,
        messages: list[dict],
        tools: Optional[list[dict]],
        temperature: float,
        max_tokens: int,
        tool_choice: str,
    ) -> Optional[dict]:
        if self._rate_limiter:
            await self._rate_limiter.acquire()

        payload = self._build_payload(messages, tools, temperature, max_tokens, tool_choice)
        headers = {}
        if self._api_key and self._api_key != "no-key-required":
            headers["Authorization"] = f"Bearer {self._api_key}"

        max_attempts = 3
        for attempt in range(1, max_attempts + 1):
            try:
                resp = await self._http.post(
                    f"{self._base_url}/chat/completions",
                    json=payload,
                    headers=headers,
                )
                resp.raise_for_status()
                data = resp.json()
                if self._health_key:
                    health_registry.update(self._health_key, ServiceStatus.HEALTHY)
                return data
            except httpx.HTTPStatusError as exc:
                if exc.response.status_code == 429 and attempt < max_attempts:
                    wait = 2 ** attempt
                    logger.warning(
                        "Rate limited (%s), retrying in %ds (attempt %d/%d)",
                        self._health_key, wait, attempt, max_attempts,
                    )
                    await asyncio.sleep(wait)
                    continue
                logger.error("Cloud chat failed (%s): %s", self._health_key, exc)
                if self._health_key:
                    health_registry.update(
                        self._health_key, ServiceStatus.DOWN, str(exc)
                    )
                return None
            except Exception as exc:
                logger.error("Cloud chat error (%s): %s", self._health_key, exc)
                if self._health_key:
                    health_registry.update(self._health_key, ServiceStatus.DOWN, str(exc))
                return None
        return None

    def _build_payload(
        self,
        messages: list[dict],
        tools: Optional[list[dict]],
        temperature: float,
        max_tokens: int,
        tool_choice: str,
    ) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "model": self._model_id,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
        }
        if tools:
            payload["tools"] = tools
            payload["tool_choice"] = tool_choice
        return payload

    # ── Direct chat (bypass circuit breaker) ──────────────────────

    async def direct_chat(
        self,
        messages: list[dict],
        temperature: float = 0.6,
        max_tokens: int = 256,
    ) -> Optional[dict]:
        """Bypass circuit breaker — used as last-resort fallback."""
        payload = self._build_payload(messages, None, temperature, max_tokens, "auto")
        headers = {}
        if self._api_key and self._api_key != "no-key-required":
            headers["Authorization"] = f"Bearer {self._api_key}"
        try:
            resp = await self._http.post(
                f"{self._base_url}/chat/completions",
                json=payload,
                headers=headers,
                timeout=30.0,
            )
            resp.raise_for_status()
            data = resp.json()
            # Success — reset circuit if applicable
            if self._circuit:
                async with self._circuit._lock:
                    self._circuit._failure_count = 0
                    self._circuit._state = CircuitState.CLOSED
            if self._health_key:
                health_registry.update(self._health_key, ServiceStatus.HEALTHY)
            logger.info("direct_chat succeeded (%s)", self._health_key)
            return data
        except Exception as exc:
            logger.warning("direct_chat fallback failed (%s): %s", self._health_key, exc)
            return None

    # ── Streaming chat ────────────────────────────────────────────

    async def chat_stream(
        self,
        messages: list[dict],
        tools: Optional[list[dict]] = None,
        temperature: float = 0.6,
        max_tokens: int = 2048,
    ) -> AsyncIterator[str]:
        if self._rate_limiter:
            await self._rate_limiter.acquire()

        payload = self._build_payload(messages, tools, temperature, max_tokens, "auto")
        payload["stream"] = True

        headers = {}
        if self._api_key and self._api_key != "no-key-required":
            headers["Authorization"] = f"Bearer {self._api_key}"

        try:
            async with self._http.stream(
                "POST",
                f"{self._base_url}/chat/completions",
                json=payload,
                headers=headers,
            ) as resp:
                resp.raise_for_status()
                async for line in resp.aiter_lines():
                    if not line.startswith("data:"):
                        continue
                    raw = line[5:].strip()
                    if raw == "[DONE]":
                        break
                    try:
                        chunk = json.loads(raw)
                        content = (
                            chunk["choices"][0].get("delta", {}).get("content")
                        )
                        if content:
                            yield content
                    except (json.JSONDecodeError, KeyError):
                        continue
        except Exception as exc:
            logger.error("Stream error (%s): %s", self._health_key, exc)
            yield _FALLBACK

    # ── Runtime reconfiguration ───────────────────────────────────

    def reconfigure(self, **overrides: Any) -> None:
        """
        Update client configuration at runtime (e.g. new API key).

        Accepted keyword overrides:
          api_key, url/endpoint, model/model_id, display_name
        """
        if "api_key" in overrides:
            self._api_key = overrides["api_key"]
        if "url" in overrides:
            self._base_url = overrides["url"].rstrip("/")
        if "endpoint" in overrides:
            self._base_url = overrides["endpoint"].rstrip("/")
        if "model" in overrides:
            self._model_id = overrides["model"]
        if "model_id" in overrides:
            self._model_id = overrides["model_id"]
        if "display_name" in overrides:
            self._display_name = overrides["display_name"]

    # ── Lifecycle ─────────────────────────────────────────────────

    async def close(self) -> None:
        if self._probe_task:
            self._probe_task.cancel()
        await self._http.aclose()

    def __repr__(self) -> str:
        return (
            f"<OpenAIInferenceClient name={self._display_name!r} "
            f"model={self._model_id!r} configured={self.is_configured}>"
        )


# ── Factory ───────────────────────────────────────────────────────

def create_client(profile: ModelProfile) -> OpenAIInferenceClient:
    """Create an inference client from a model profile."""
    return OpenAIInferenceClient(profile)
