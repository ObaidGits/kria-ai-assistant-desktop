from types import SimpleNamespace

import pytest

from kria.api import routes
from kria.infra.health import ServiceStatus, health_registry


class FakeVisionClient:
    def __init__(
        self,
        *,
        configured: bool = False,
        content: str = "",
        health_ok: bool = False,
        error: Exception | None = None,
    ) -> None:
        self.is_configured = configured
        self._content = content
        self._health_ok = health_ok
        self._error = error
        self.chat_calls = 0
        self.health_checks = 0

    async def chat(self, **kwargs):
        self.chat_calls += 1
        if self._error is not None:
            raise self._error
        if not self._content:
            return None
        return {"choices": [{"message": {"content": self._content}}]}

    async def health_check(self) -> bool:
        self.health_checks += 1
        return self._health_ok


@pytest.mark.asyncio
async def test_analyze_image_skips_unhealthy_secondary(monkeypatch):
    import kria.agent.llm_client as llm_client
    import kria.agent.model_router as model_router_mod

    secondary = FakeVisionClient(health_ok=False)
    gemini = FakeVisionClient(configured=False)
    external = FakeVisionClient(configured=False)

    monkeypatch.setattr(llm_client, "secondary_llm_client", secondary)
    monkeypatch.setattr(model_router_mod, "model_router", SimpleNamespace(mode="auto"))
    monkeypatch.setattr(model_router_mod, "gemini_client", gemini)
    monkeypatch.setattr(model_router_mod, "external_client", external)

    health_registry.update("llm_secondary", ServiceStatus.DOWN, "offline")

    result = await routes._analyze_image(b"fake-image", "test.png", "Describe this image")

    assert result == ""
    assert secondary.health_checks == 1
    assert secondary.chat_calls == 0


@pytest.mark.asyncio
async def test_analyze_image_uses_configured_cloud_fallback_when_secondary_is_down(monkeypatch):
    import kria.agent.llm_client as llm_client
    import kria.agent.model_router as model_router_mod

    secondary = FakeVisionClient(health_ok=False)
    gemini = FakeVisionClient(configured=False)
    external = FakeVisionClient(configured=True, content="Cloud vision result")

    monkeypatch.setattr(llm_client, "secondary_llm_client", secondary)
    monkeypatch.setattr(model_router_mod, "model_router", SimpleNamespace(mode="auto"))
    monkeypatch.setattr(model_router_mod, "gemini_client", gemini)
    monkeypatch.setattr(model_router_mod, "external_client", external)

    health_registry.update("llm_secondary", ServiceStatus.DOWN, "offline")

    result = await routes._analyze_image(b"fake-image", "test.png", "Describe this image")

    assert result == "Cloud vision result"
    assert secondary.chat_calls == 0
    assert external.chat_calls == 1


@pytest.mark.asyncio
async def test_analyze_image_falls_back_to_secondary_after_cloud_failure(monkeypatch):
    import kria.agent.llm_client as llm_client
    import kria.agent.model_router as model_router_mod

    secondary = FakeVisionClient(content="Local vision result", health_ok=True)
    gemini = FakeVisionClient(
        configured=True,
        error=RuntimeError("Gemini unavailable"),
    )
    external = FakeVisionClient(configured=False)

    monkeypatch.setattr(llm_client, "secondary_llm_client", secondary)
    monkeypatch.setattr(model_router_mod, "model_router", SimpleNamespace(mode="gemini"))
    monkeypatch.setattr(model_router_mod, "gemini_client", gemini)
    monkeypatch.setattr(model_router_mod, "external_client", external)

    health_registry.update("llm_secondary", ServiceStatus.DOWN, "stale")

    result = await routes._analyze_image(b"fake-image", "test.png", "Describe this image")

    assert result == "Local vision result"
    assert gemini.chat_calls == 1
    assert secondary.health_checks == 1
    assert secondary.chat_calls == 1
