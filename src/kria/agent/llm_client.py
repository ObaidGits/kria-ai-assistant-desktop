"""
LLM Client — Backward-Compatibility Shim
=========================================
This module now delegates to the config-driven ModelRouter / OpenAIInferenceClient.

The old singletons are preserved as lazy proxies so that any file doing::

    from kria.agent.llm_client import llm_client          # primary
    from kria.agent.llm_client import primary_llm_client   # primary
    from kria.agent.llm_client import secondary_llm_client # secondary

continues to work without changes.

The actual ``LLMClient`` class is kept for type-checking only — new code
should import ``OpenAIInferenceClient`` from ``kria.agent.inference_client``.
"""
from __future__ import annotations

import logging
from typing import Any

logger = logging.getLogger("kria.llm")


def _get_primary():
    from kria.agent.model_router import model_router
    return model_router.get_client_by_name("primary")


def _get_secondary():
    from kria.agent.model_router import model_router
    return model_router.get_client_by_name("secondary")


class _ClientProxy:
    """Lazy proxy that resolves the real client on first attribute access."""

    def __init__(self, resolver):
        object.__setattr__(self, "_resolver", resolver)
        object.__setattr__(self, "_client", None)

    def _resolve(self):
        client = object.__getattribute__(self, "_client")
        if client is None:
            client = object.__getattribute__(self, "_resolver")()
            object.__setattr__(self, "_client", client)
        return client

    def __getattr__(self, name: str) -> Any:
        return getattr(self._resolve(), name)

    def __repr__(self):
        try:
            return repr(self._resolve())
        except Exception:
            return "<_ClientProxy (unresolved)>"


# ── Backward-compat singletons ───────────────────────────────────
primary_llm_client = _ClientProxy(_get_primary)
secondary_llm_client = _ClientProxy(_get_secondary)
llm_client = primary_llm_client  # alias

# Re-export the concrete class under the old name for isinstance checks
from kria.agent.inference_client import OpenAIInferenceClient as LLMClient  # noqa: E402, F401
