"""
LLM Client (llama.cpp OpenAI-compatible API)
=============================================
Wraps the llama.cpp server with:
  - Non-streaming chat completion (tool-calling)
  - Streaming token generation (for TTS sentence pipelining)
  - Circuit breaker (stops hammering a restarting server)
  - Health check (called on startup and periodically)
  - Graceful fallback text when the circuit is OPEN

The client speaks the OpenAI chat-completions API that llama.cpp exposes
at /v1/chat/completions, so swapping to any other OpenAI-compatible server
requires only changing ``settings.llama_api_url``.
"""
import asyncio
import json
import logging
from typing import Any, AsyncIterator, Optional

import httpx

from kria.infra.circuit_breaker import CircuitBreaker, CircuitState
from kria.infra.config import settings
from kria.infra.health import ServiceStatus, health_registry

logger = logging.getLogger("kria.llm")

_FALLBACK = "I'm having trouble reaching my reasoning engine right now. Please try again in a moment."


class LLMClient:
    def __init__(self) -> None:
        self._base = settings.llama_api_url.rstrip("/")
        self._http = httpx.AsyncClient(
            timeout=httpx.Timeout(connect=10.0, read=120.0, write=30.0, pool=5.0),
            limits=httpx.Limits(max_connections=10),
        )
        self._circuit = CircuitBreaker(
            name="llm",
            failure_threshold=5,
            recovery_timeout=30.0,
            fallback=None,
        )
        health_registry.register("llm")
        self._probe_task: Optional[asyncio.Task] = None

    # ── Health ────────────────────────────────────────────────────

    async def health_check(self) -> bool:
        try:
            resp = await self._http.get(f"{self._base}/health", timeout=5.0)
            ok = resp.status_code == 200
            health_registry.update(
                "llm",
                ServiceStatus.HEALTHY if ok else ServiceStatus.DEGRADED,
            )
            return ok
        except Exception as exc:
            health_registry.update("llm", ServiceStatus.DOWN, str(exc))
            logger.warning("LLM health check failed: %s", exc)
            return False

    async def wait_for_ready(self, max_retries: int = 15, delay: float = 2.0) -> bool:
        """Wait for the LLM backend to become reachable. Called during startup."""
        for attempt in range(1, max_retries + 1):
            if await self.health_check():
                logger.info("LLM backend ready (attempt %d/%d)", attempt, max_retries)
                # Start periodic health probe in the background
                self._probe_task = asyncio.ensure_future(self._periodic_health_probe())
                return True
            logger.info("LLM backend not ready — retrying in %.0fs (%d/%d)",
                        delay, attempt, max_retries)
            await asyncio.sleep(delay)
        logger.warning("LLM backend not reachable after %d attempts", max_retries)
        # Start probe anyway so it can recover later
        self._probe_task = asyncio.ensure_future(self._periodic_health_probe())
        return False

    async def _periodic_health_probe(self, interval: float = 15.0) -> None:
        """Background task: probes the LLM backend and resets the circuit breaker on recovery."""
        while True:
            try:
                await asyncio.sleep(interval)
                ok = await self.health_check()
                if ok and self._circuit.state.value != "closed":
                    # Brain is back — force-reset the circuit breaker
                    async with self._circuit._lock:
                        self._circuit._failure_count = 0
                        self._circuit._state = CircuitState.CLOSED
                    logger.info("LLM health probe: brain recovered — circuit breaker reset")
            except asyncio.CancelledError:
                break
            except Exception as exc:
                logger.debug("LLM health probe error: %s", exc)

    # ── Non-streaming completion ───────────────────────────────────

    async def chat(
        self,
        messages: list[dict],
        tools: Optional[list[dict]] = None,
        temperature: float = 0.6,
        max_tokens: int = 2048,
    ) -> Optional[dict]:
        """
        Send a chat-completions request.
        Returns the full response dict or None if the circuit is open / request fails.
        """
        payload: dict[str, Any] = {
            "model": "qwen3-8b",
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
        }
        if tools:
            payload["tools"] = tools
            payload["tool_choice"] = "auto"

        async def _do_request() -> dict:
            resp = await self._http.post(
                f"{self._base}/v1/chat/completions", json=payload
            )
            resp.raise_for_status()
            return resp.json()

        result = await self._circuit.call(_do_request)
        if result is None:
            logger.warning("LLM chat returned None (circuit state: %s, failures: %d/%d)",
                           self._circuit.state.value, self._circuit._failure_count,
                           self._circuit.failure_threshold)
            health_registry.update("llm", ServiceStatus.DOWN, "circuit open or request failed")
        else:
            health_registry.update("llm", ServiceStatus.HEALTHY)
        return result

    async def direct_chat(
        self,
        messages: list[dict],
        temperature: float = 0.6,
        max_tokens: int = 256,
    ) -> Optional[dict]:
        """
        Single direct HTTP request bypassing the circuit breaker.
        Used as a last-resort fallback when the circuit is OPEN but brain might be alive.
        """
        payload: dict[str, Any] = {
            "model": "qwen3-8b",
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
        }
        try:
            resp = await self._http.post(
                f"{self._base}/v1/chat/completions",
                json=payload,
                timeout=30.0,
            )
            resp.raise_for_status()
            data = resp.json()
            # Success — reset circuit breaker since brain is actually alive
            async with self._circuit._lock:
                self._circuit._failure_count = 0
                self._circuit._state = CircuitState.CLOSED
            health_registry.update("llm", ServiceStatus.HEALTHY)
            logger.info("direct_chat succeeded — circuit breaker reset")
            return data
        except Exception as exc:
            logger.warning("direct_chat fallback failed: %s", exc)
            return None

    # ── Streaming completion ───────────────────────────────────────

    async def chat_stream(
        self,
        messages: list[dict],
        tools: Optional[list[dict]] = None,
        temperature: float = 0.6,
        max_tokens: int = 2048,
    ) -> AsyncIterator[str]:
        """
        Stream text tokens from the LLM.
        On any error, yields the fallback string and stops.
        """
        payload: dict[str, Any] = {
            "model": "qwen3-8b",
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": True,
        }
        if tools:
            payload["tools"] = tools

        try:
            async with self._http.stream(
                "POST", f"{self._base}/v1/chat/completions", json=payload
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
                        content = chunk["choices"][0].get("delta", {}).get("content")
                        if content:
                            yield content
                    except (json.JSONDecodeError, KeyError):
                        continue
        except Exception as exc:
            logger.error("LLM stream error: %s", exc)
            yield _FALLBACK

    async def close(self) -> None:
        if self._probe_task:
            self._probe_task.cancel()
        await self._http.aclose()


# Singleton
llm_client = LLMClient()
