"""
K.R.I.A. — FastAPI Application Entry Point
============================================
Startup sequence (inside lifespan):

  Phase 1 — Infrastructure (logging, Redis, SQLite, ChromaDB)
  Phase 2 — Safety system (policy engine, rollback dir, audit logger)
  Phase 3 — Memory (conversation buffer, semantic memory connection)
  Phase 4 — Agent (LLM client health-check, tool registry import)
  Phase 5 — Voice pipeline (start supervised wake-word task)

Each phase is wrapped in its own try/except.  A failure in one phase
logs a warning but never aborts the startup of subsequent phases.
This satisfies the "break-resistant" requirement from the design doc:
the system always starts, always answers via text, even if voice is down.
"""
import asyncio
import logging
import os
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI
from fastapi.middleware.cors import CORSMiddleware

from kria.infra.config import settings
from kria.infra.health import ServiceStatus, health_registry
from kria.infra.logging_config import setup_logging

logger = logging.getLogger("kria.main")


# ── Ensure data directory exists ──────────────────────────────────
Path(settings.sqlite_path).parent.mkdir(parents=True, exist_ok=True)


@asynccontextmanager
async def lifespan(app: FastAPI):
    """Managed startup / shutdown for all K.R.I.A. services."""
    setup_logging()
    logger.info("=" * 56)
    logger.info("  K.R.I.A. v1.0.0 — Starting up")
    logger.info("=" * 56)

    # ── Phase 1: Infrastructure ───────────────────────────────────
    try:
        from kria.infra.redis_bus import redis_bus
        await redis_bus.connect()
        logger.info("[P1] Redis bus connected")
    except Exception as exc:
        logger.warning("[P1] Redis unavailable — in-memory fallback active: %s", exc)

    try:
        from kria.memory.persistent import sqlite_manager
        await sqlite_manager.connect()
        logger.info("[P1] SQLite initialized")
    except Exception as exc:
        logger.warning("[P1] SQLite unavailable: %s", exc)

    # ── Phase 2: Safety system ────────────────────────────────────
    try:
        from kria.safety.policy_engine import policy_engine   # noqa: F401
        from kria.safety.rollback import rollback_manager
        from kria.safety.hitl import hitl_gateway
        from kria.api.websocket import broadcast
        hitl_gateway.set_broadcast_handler(broadcast)
        await rollback_manager.cleanup_expired()
        logger.info("[P2] Safety system ready")
    except Exception as exc:
        logger.warning("[P2] Safety init partial failure: %s", exc)

    # ── Phase 3: Memory ───────────────────────────────────────────
    try:
        from kria.memory.semantic import semantic_memory
        await semantic_memory.connect()
        logger.info("[P3] Semantic memory connected")
    except Exception as exc:
        logger.warning("[P3] ChromaDB unavailable — RAG disabled: %s", exc)

    # ── Phase 4: Agent (tools auto-register on import) ────────────
    try:
        from kria.tools.registry import tool_registry
        tool_registry.load_all()
        from kria.agent.llm_client import llm_client
        await llm_client.wait_for_ready(max_retries=15, delay=2.0)
        logger.info("[P4] LLM client ready — %d tools registered", len(tool_registry))
    except Exception as exc:
        logger.warning("[P4] LLM/tool init partial failure: %s", exc)

    # ── Phase 5: Voice (optional — gracefully absent without audio HW) ──
    try:
        from kria.voice.pipeline import build_voice_pipeline
        pipeline = build_voice_pipeline()
        await pipeline.start()
        logger.info("[P5] Voice pipeline started")
    except ImportError:
        logger.info("[P5] sounddevice not installed — text-only mode")
    except Exception as exc:
        logger.warning("[P5] Voice pipeline unavailable: %s", exc)

    # ── Phase 6: Probe external services for health dashboard ─────
    try:
        import httpx
        async with httpx.AsyncClient(timeout=5.0) as client:
            try:
                resp = await client.get(f"{settings.piper_api_url}/health")
                if resp.status_code == 200:
                    health_registry.update("piper_tts", ServiceStatus.HEALTHY)
                else:
                    health_registry.update("piper_tts", ServiceStatus.DOWN,
                                           f"HTTP {resp.status_code}")
            except Exception as exc:
                health_registry.update("piper_tts", ServiceStatus.DOWN, str(exc))
            try:
                resp = await client.get(f"{settings.whisper_api_url}/health")
                if resp.status_code == 200:
                    health_registry.update("stt", ServiceStatus.HEALTHY)
                else:
                    health_registry.update("stt", ServiceStatus.DOWN,
                                           f"HTTP {resp.status_code}")
            except Exception as exc:
                health_registry.update("stt", ServiceStatus.DOWN, str(exc))
        logger.info("[P6] Service health probes done")
    except Exception as exc:
        logger.warning("[P6] Health probes failed: %s", exc)

    logger.info("K.R.I.A. ready — http://localhost:8000")
    yield

    # ── Shutdown ──────────────────────────────────────────────────
    logger.info("K.R.I.A. shutting down...")

    try:
        from kria.voice.pipeline import voice_pipeline
        await voice_pipeline.stop()
    except Exception:
        pass

    try:
        from kria.infra.redis_bus import redis_bus
        await redis_bus.close()
    except Exception:
        pass

    try:
        from kria.memory.persistent import sqlite_manager
        await sqlite_manager.close()
    except Exception:
        pass

    try:
        from kria.agent.llm_client import llm_client
        await llm_client.close()
    except Exception:
        pass

    logger.info("K.R.I.A. shut down cleanly.")


# ── Application factory ───────────────────────────────────────────
app = FastAPI(
    title="K.R.I.A.",
    description="Kernel-Responsive Intelligent Agent — Local OS control via voice & text",
    version="1.0.0",
    lifespan=lifespan,
)

app.add_middleware(
    CORSMiddleware,
    allow_origins=["http://localhost:3000", "http://127.0.0.1:3000"],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)

# ── Mounted routers (imported here to avoid circular imports) ─────
from kria.api.routes import router as api_router         # noqa: E402
from kria.api.websocket import ws_router                 # noqa: E402

app.include_router(api_router)
app.include_router(ws_router)


@app.get("/health", tags=["infra"])
async def health_check() -> dict:
    return {
        "status": "running",
        "services": health_registry.summary(),
    }
