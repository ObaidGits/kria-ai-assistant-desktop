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
    logger.info("  K.R.I.A. v2.0.0 — Starting up")
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
        from kria.agent.interaction import interaction_gateway
        from kria.api.websocket import broadcast, ws_clients_count
        hitl_gateway.set_broadcast_handler(broadcast, ws_clients_count)
        interaction_gateway.set_broadcast_handler(broadcast, ws_clients_count)
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

    # ── Phase 3b: Mem0 Fact Memory ────────────────────────────────
    try:
        from kria.memory.mem0_memory import mem0_memory
        await mem0_memory.connect()
        logger.info("[P3b] Mem0 fact memory connected")
    except Exception as exc:
        logger.warning("[P3b] Mem0 unavailable — fact memory disabled: %s", exc)

    # ── Phase 4: Agent (tools auto-register on import) ────────────
    try:
        from kria.tools.registry import tool_registry
        tool_registry.load_all()
        from kria.agent.model_router import model_router
        await model_router.wait_all_ready()
        logger.info("[P4] LLM clients ready — mode=%s — %d tools registered",
                    model_router.mode, len(tool_registry))
    except Exception as exc:
        logger.warning("[P4] LLM/tool init partial failure: %s", exc)

    # ── Phase 4b: MCP servers (optional external tools) ───────────
    try:
        if settings.mcp_enabled:
            from kria.mcp import mcp_manager
            await mcp_manager.start(tool_registry, health_registry)
            logger.info("[P4b] MCP client started — %d servers, %d tools",
                        mcp_manager.server_count, mcp_manager.tool_count)
        else:
            logger.info("[P4b] MCP integration disabled")
    except Exception as exc:
        logger.warning("[P4b] MCP init failed (non-fatal): %s", exc)

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

    # ── Phase 5b: Scheduler ──────────────────────────────────────
    try:
        from kria.automation.scheduler import scheduler
        scheduler.start()
        logger.info("[P5b] Scheduler started")
    except Exception as exc:
        logger.warning("[P5b] Scheduler unavailable: %s", exc)

    # ── Phase 5c: Plugins ─────────────────────────────────────────
    try:
        if settings.plugins_enabled:
            from kria.plugins.loader import plugin_loader
            for plugin_info in plugin_loader.discover():
                if plugin_info.get("enabled", True):
                    plugin_loader.load(plugin_info.get("name", ""))
            logger.info("[P5c] Plugins loaded: %d", len(plugin_loader.list_loaded()))
    except Exception as exc:
        logger.warning("[P5c] Plugin loading failed: %s", exc)

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
            try:
                resp = await client.get(f"{settings.qdrant_url}/healthz")
                if resp.status_code == 200:
                    health_registry.update("qdrant", ServiceStatus.HEALTHY)
                else:
                    health_registry.update("qdrant", ServiceStatus.DOWN,
                                           f"HTTP {resp.status_code}")
            except Exception as exc:
                health_registry.update("qdrant", ServiceStatus.DOWN, str(exc))
        logger.info("[P6] Service health probes done")
    except Exception as exc:
        logger.warning("[P6] Health probes failed: %s", exc)

    logger.info("K.R.I.A. ready — http://localhost:8088")
    yield

    # ── Shutdown ──────────────────────────────────────────────────
    logger.info("K.R.I.A. shutting down...")

    try:
        from kria.voice.pipeline import voice_pipeline
        await voice_pipeline.stop()
    except Exception:
        pass

    try:
        from kria.automation.scheduler import scheduler
        scheduler.stop()
    except Exception:
        pass

    try:
        from kria.tools.file_watcher import file_watcher
        file_watcher.stop_all()
    except Exception:
        pass

    try:
        if settings.mcp_enabled:
            from kria.mcp import mcp_manager
            await mcp_manager.stop()
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
        from kria.agent.model_router import model_router
        await model_router.close_all()
    except Exception:
        pass

    logger.info("K.R.I.A. shut down cleanly.")


# ── Application factory ───────────────────────────────────────────
app = FastAPI(
    title="K.R.I.A.",
    description="Kernel-Responsive Intelligent Agent — Complete AI Assistant",
    version="2.0.0",
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
