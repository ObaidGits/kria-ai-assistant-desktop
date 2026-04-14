"""
Mem0 Fact Memory
================
Automatic fact extraction and lifecycle management via Mem0 OSS.

Mem0 analyses conversation turns and automatically:
 - ADDs new facts ("user prefers Python over JS")
 - UPDATEs facts when they change ("user switched to Arch Linux")
 - DELETEs facts that are contradicted or retracted

Backend: Qdrant (vector store) + local LLM (llama.cpp, OpenAI-compat)
         + sentence-transformers for embeddings (no external API needed).

Collection lifecycle is fully managed by Mem0 — we just call add/search/delete.
"""
import asyncio
import logging
from typing import Optional

from kria.infra.config import settings
from kria.infra.health import ServiceStatus, health_registry

logger = logging.getLogger("kria.memory.mem0")

_USER_ID = "kria_user"  # single-user assistant


class Mem0Memory:
    def __init__(self) -> None:
        self._m = None
        self._available = False
        self._qdrant_host: str = "localhost"
        self._qdrant_port: int = 6333

    async def connect(self) -> bool:
        """Initialise Mem0 with Qdrant + local LLM.  Returns True on success."""
        try:
            from mem0 import Memory

            qdrant_url = settings.qdrant_url
            llm_url = settings.llama_api_url

            # Parse host/port from qdrant_url (e.g. http://kria-qdrant:6333)
            from urllib.parse import urlparse
            parsed = urlparse(qdrant_url)
            self._qdrant_host = parsed.hostname or "localhost"
            self._qdrant_port = parsed.port or 6333

            config = {
                "vector_store": {
                    "provider": "qdrant",
                    "config": {
                        "host": self._qdrant_host,
                        "port": self._qdrant_port,
                        "collection_name": "kria_mem0",
                        "embedding_model_dims": 384,
                    },
                },
                "llm": {
                    "provider": "openai",
                    "config": {
                        "model": "local-model",
                        "openai_base_url": llm_url + "/v1",
                        "api_key": "not-needed",
                        "temperature": 0.1,
                        "max_tokens": 1500,
                    },
                },
                "embedder": {
                    "provider": "huggingface",
                    "config": {
                        "model": "all-MiniLM-L6-v2",
                        "embedding_dims": 384,
                    },
                },
                "version": "v1.1",
            }

            def _init():
                return Memory.from_config(config)

            try:
                self._m = await asyncio.to_thread(_init)
            except Exception as init_exc:
                # Auto-recover from vector dimension mismatch by dropping stale collections
                if "dimension error" in str(init_exc).lower() or "vector dimension" in str(init_exc).lower() or "Wrong input" in str(init_exc):
                    logger.warning("Mem0: dimension mismatch detected — resetting Qdrant collections and retrying")
                    await self._reset_qdrant_collections()
                    self._m = await asyncio.to_thread(_init)
                else:
                    raise

            self._available = True
            health_registry.update("mem0", ServiceStatus.HEALTHY)
            logger.info("Mem0: connected (Qdrant %s:%d)", self._qdrant_host, self._qdrant_port)
            return True

        except Exception as exc:
            logger.warning("Mem0 unavailable — fact memory disabled: %s", exc)
            health_registry.update("mem0", ServiceStatus.DOWN, str(exc))
            self._available = False
            return False

    async def _reset_qdrant_collections(self) -> None:
        """Delete stale Mem0 Qdrant collections so they are recreated with correct dims."""
        import httpx
        base = f"http://{self._qdrant_host}:{self._qdrant_port}"
        try:
            async with httpx.AsyncClient(timeout=10.0) as client:
                for col in ("kria_mem0", "mem0migrations"):
                    resp = await client.delete(f"{base}/collections/{col}")
                    if resp.status_code in (200, 404):
                        logger.info("Mem0: deleted Qdrant collection '%s'", col)
                    else:
                        logger.warning("Mem0: could not delete '%s': %s", col, resp.status_code)
        except Exception as exc:
            logger.warning("Mem0: failed to reset collections: %s", exc)

    # ── Write ─────────────────────────────────────────────────────

    async def add(
        self,
        messages: list[dict],
        user_id: str = _USER_ID,
        metadata: Optional[dict] = None,
    ) -> Optional[dict]:
        """Feed conversation messages to Mem0 for automatic fact extraction.

        Args:
            messages: OpenAI-format messages [{"role": ..., "content": ...}]
            user_id:  User identifier (single-user by default)
            metadata: Extra metadata tags
        Returns:
            Mem0 result dict or None on failure.
        """
        if not self._available or not self._m:
            return None
        try:
            m = self._m
            result = await asyncio.to_thread(
                m.add, messages, user_id=user_id, metadata=metadata or {},
            )
            return result
        except Exception as exc:
            err = str(exc)
            if "dimension error" in err.lower() or "vector dimension" in err.lower() or "Wrong input" in err:
                logger.warning("Mem0.add: dimension mismatch — resetting and reconnecting")
                self._available = False
                self._m = None
                await self._reset_qdrant_collections()
                await self.connect()
                return None
            logger.warning("Mem0.add failed: %s", exc)
            return None

    # ── Search ────────────────────────────────────────────────────

    async def search(
        self,
        query: str,
        user_id: str = _USER_ID,
        limit: int = 5,
    ) -> list[dict]:
        """Search stored facts by semantic similarity.

        Returns list of dicts: [{id, memory, score, ...}, ...]
        """
        if not self._available or not self._m:
            return []
        try:
            m = self._m
            result = await asyncio.to_thread(
                m.search, query, user_id=user_id, limit=limit,
            )
            # Mem0 returns {"results": [...]} dict
            if isinstance(result, dict):
                return result.get("results", [])
            return result if isinstance(result, list) else []
        except Exception as exc:
            err = str(exc)
            # Auto-recover on dimension mismatch at query time
            if "dimension error" in err.lower() or "vector dimension" in err.lower() or "Wrong input" in err:
                logger.warning("Mem0.search: dimension mismatch — resetting and reconnecting")
                self._available = False
                self._m = None
                await self._reset_qdrant_collections()
                await self.connect()
                return []
            logger.warning("Mem0.search failed: %s", exc)
            return []

    # ── Read all ──────────────────────────────────────────────────

    async def get_all(
        self,
        user_id: str = _USER_ID,
    ) -> list[dict]:
        """Return all stored facts for a user."""
        if not self._available or not self._m:
            return []
        try:
            m = self._m
            result = await asyncio.to_thread(m.get_all, user_id=user_id)
            if isinstance(result, dict):
                return result.get("results", result.get("memories", []))
            return result if isinstance(result, list) else []
        except Exception as exc:
            logger.warning("Mem0.get_all failed: %s", exc)
            return []

    # ── Delete ────────────────────────────────────────────────────

    async def delete(self, memory_id: str) -> bool:
        """Delete a specific fact by ID."""
        if not self._available or not self._m:
            return False
        try:
            m = self._m
            await asyncio.to_thread(m.delete, memory_id)
            return True
        except Exception as exc:
            logger.warning("Mem0.delete failed: %s", exc)
            return False

    async def delete_all(self, user_id: str = _USER_ID) -> bool:
        """Delete all facts for a user."""
        if not self._available or not self._m:
            return False
        try:
            m = self._m
            await asyncio.to_thread(m.delete_all, user_id=user_id)
            return True
        except Exception as exc:
            logger.warning("Mem0.delete_all failed: %s", exc)
            return False

    # ── History ───────────────────────────────────────────────────

    async def history(self, memory_id: str) -> list[dict]:
        """Get change history for a specific fact."""
        if not self._available or not self._m:
            return []
        try:
            m = self._m
            result = await asyncio.to_thread(m.history, memory_id)
            return result if isinstance(result, list) else []
        except Exception as exc:
            logger.warning("Mem0.history failed: %s", exc)
            return []

    @property
    def available(self) -> bool:
        return self._available


# Singleton
mem0_memory = Mem0Memory()
