"""
Semantic Memory (Vector Store)
================================
Stores and retrieves text chunks via ChromaDB embeddings.  Used for
long-term memory: summarised conversations, user facts, learned preferences.

ChromaDB runs as a server on port 8083 (docker/data/) OR embedded mode
for single-machine deployments.  Both modes are tried in order; the fallback
is a no-op in-process store that just returns empty results.

Collection schema:
  id          — unique UUID per chunk
  document    — text content
  metadata    — {"session_id", "timestamp", "type"}
"""
import logging
import uuid
from datetime import datetime, timezone
from typing import Optional

from kria.infra.config import settings
from kria.infra.health import ServiceStatus, health_registry

logger = logging.getLogger("kria.memory.semantic")

_COLLECTION_NAME = "kria_memory"
_EMBEDDING_MODEL = "all-MiniLM-L6-v2"  # sentence-transformers fallback


class SemanticMemory:
    def __init__(self) -> None:
        self._client = None
        self._collection = None
        self._available = False

    async def connect(self) -> bool:
        """Try HTTP then embedded ChromaDB. Returns True if memory is usable."""
        try:
            import chromadb  # type: ignore
            try:
                # Try server mode first
                self._client = chromadb.HttpClient(
                    host=settings.chromadb_host,
                    port=settings.chromadb_port,
                )
                self._client.heartbeat()  # raises if not reachable
                logger.info("ChromaDB: connected to server at %s:%d",
                            settings.chromadb_host, settings.chromadb_port)
            except Exception:
                # Fall back to embedded (PersistentClient)
                embed_path = str(settings.chromadb_path)
                self._client = chromadb.PersistentClient(path=embed_path)
                logger.info("ChromaDB: using embedded mode at %s", embed_path)

            self._collection = self._client.get_or_create_collection(
                name=_COLLECTION_NAME,
                metadata={"hnsw:space": "cosine"},
            )
            self._available = True
            health_registry.update("chromadb", ServiceStatus.HEALTHY)
            return True
        except Exception as exc:
            logger.warning("ChromaDB unavailable — semantic memory disabled: %s", exc)
            health_registry.update("chromadb", ServiceStatus.UNHEALTHY, str(exc))
            self._available = False
            return False

    # ── Write ─────────────────────────────────────────────────────

    async def store(
        self,
        text: str,
        session_id: str = "",
        memory_type: str = "conversation",
        extra_metadata: Optional[dict] = None,
    ) -> Optional[str]:
        """Store *text* in the vector collection. Returns the chunk ID or None."""
        if not self._available or not self._collection:
            return None
        chunk_id = str(uuid.uuid4())
        metadata = {
            "session_id": session_id,
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "type": memory_type,
            **(extra_metadata or {}),
        }
        try:
            self._collection.add(
                documents=[text],
                ids=[chunk_id],
                metadatas=[metadata],
            )
            return chunk_id
        except Exception as exc:
            logger.warning("SemanticMemory.store failed: %s", exc)
            return None

    # ── Query ─────────────────────────────────────────────────────

    async def query(
        self,
        query_text: str,
        n_results: int = 5,
        session_id: Optional[str] = None,
        memory_type: Optional[str] = None,
    ) -> list[dict]:
        """Return up to *n_results* semantically similar chunks."""
        if not self._available or not self._collection:
            return []
        where: Optional[dict] = None
        if session_id and memory_type:
            where = {"$and": [{"session_id": session_id}, {"type": memory_type}]}
        elif session_id:
            where = {"session_id": session_id}
        elif memory_type:
            where = {"type": memory_type}
        try:
            results = self._collection.query(
                query_texts=[query_text],
                n_results=min(n_results, self._collection.count() or 1),
                where=where,
            )
            docs = results.get("documents", [[]])[0]
            metas = results.get("metadatas", [[]])[0]
            dists = results.get("distances", [[]])[0]
            return [
                {"text": d, "metadata": m, "distance": s}
                for d, m, s in zip(docs, metas, dists)
            ]
        except Exception as exc:
            logger.warning("SemanticMemory.query failed: %s", exc)
            return []

    async def delete_by_session(self, session_id: str) -> None:
        if not self._available or not self._collection:
            return
        try:
            self._collection.delete(where={"session_id": session_id})
        except Exception as exc:
            logger.warning("SemanticMemory.delete failed: %s", exc)

    @property
    def available(self) -> bool:
        return self._available


semantic_memory = SemanticMemory()
