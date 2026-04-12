"""
REST API Routes
===============
All endpoints are versioned under /api/v1/.

Endpoints:
  POST   /chat                 — single-turn text interaction
  POST   /voice/push           — push-to-talk: upload WAV bytes
  GET    /sessions             — list conversation sessions
  GET    /sessions/{id}/history — get turn history for a session
  DELETE /sessions/{id}        — delete a session and its memories
  GET    /tools                — list all registered tools
  GET    /tools/{name}         — describe a specific tool
  POST   /hitl/decide          — submit approval/denial for a pending RED action
  GET    /hitl/pending         — list pending approval requests
  GET    /memory/search        — full-text search across conversations
  GET    /memory/preferences   — get user preferences
  PUT    /memory/preferences/{key} — set a user preference
  GET    /health               — detailed health summary
  POST   /safety/emergency     — enable/disable emergency mode
  GET    /audit                — query recent audit log entries
"""
import logging
from typing import Annotated, Optional

from fastapi import APIRouter, Body, HTTPException, Query, UploadFile, File
from pydantic import BaseModel

from kria.infra.config import settings

logger = logging.getLogger("kria.api.routes")

router = APIRouter(prefix="/api/v1", tags=["kria"])


# ── Pydantic models ───────────────────────────────────────────────

class ChatRequest(BaseModel):
    message: str
    session_id: str = "default"


class ChatResponse(BaseModel):
    session_id: str
    response: str
    tool_calls: list = []
    iterations: int = 0
    success: bool = True


class HITLDecision(BaseModel):
    request_id: str
    approved: bool


class PreferenceUpdate(BaseModel):
    value: str
    description: str = ""


class EmergencyRequest(BaseModel):
    enabled: bool


# ── Chat ──────────────────────────────────────────────────────────

@router.post("/chat", response_model=ChatResponse)
async def chat(req: ChatRequest) -> ChatResponse:
    from kria.voice.pipeline import voice_pipeline
    from kria.memory.context_manager import context_manager

    response_text = await voice_pipeline.process_text(req.message, session_id=req.session_id)

    await context_manager.save_turn(
        session_id=req.session_id,
        user_input=req.message,
        assistant_response=response_text,
    )

    return ChatResponse(
        session_id=req.session_id,
        response=response_text,
        success=True,
    )


# ── Voice push-to-talk ────────────────────────────────────────────

@router.post("/voice/push")
async def voice_push(
    audio: UploadFile = File(...),
    session_id: str = Query(default="voice"),
) -> dict:
    from kria.voice.pipeline import voice_pipeline
    audio_bytes = await audio.read()
    if not audio_bytes:
        raise HTTPException(status_code=400, detail="Empty audio file")
    transcript, response = await voice_pipeline.push_audio(audio_bytes, session_id=session_id)
    return {"session_id": session_id, "transcript": transcript, "response": response}


# ── TTS proxy ──────────────────────────────────────────────────────

class TTSRequest(BaseModel):
    text: str
    voice: str = ""
    speed: float = 1.0


@router.post("/tts")
async def tts_synthesize(req: TTSRequest):
    """Proxy TTS requests to the Piper server and return audio."""
    from fastapi.responses import Response as FastAPIResponse
    import httpx
    try:
        async with httpx.AsyncClient(timeout=30.0) as client:
            resp = await client.post(
                f"{settings.piper_api_url}/synthesize",
                json={"text": req.text, "voice": req.voice, "speed": req.speed},
            )
            resp.raise_for_status()
            return FastAPIResponse(content=resp.content, media_type="audio/wav")
    except Exception as exc:
        logger.error("TTS proxy failed: %s", exc)
        raise HTTPException(status_code=502, detail=f"TTS service error: {exc}")


# ── Sessions ──────────────────────────────────────────────────────

@router.get("/sessions")
async def list_sessions() -> dict:
    from kria.memory.conversation import conversation_memory
    sessions = await conversation_memory.get_sessions()
    return {"sessions": sessions}


@router.get("/sessions/{session_id}/history")
async def get_history(session_id: str, limit: int = Query(default=50, le=200)) -> dict:
    from kria.memory.conversation import conversation_memory
    turns = await conversation_memory.get_recent(session_id=session_id, limit=limit)
    return {"session_id": session_id, "turns": turns, "count": len(turns)}


@router.delete("/sessions/{session_id}")
async def delete_session(session_id: str) -> dict:
    from kria.memory.conversation import conversation_memory
    from kria.memory.semantic import semantic_memory
    deleted = await conversation_memory.delete_session(session_id)
    await semantic_memory.delete_by_session(session_id)
    return {"deleted": True, "turns_removed": deleted}


# ── Tools ─────────────────────────────────────────────────────────

@router.get("/tools")
async def list_tools() -> dict:
    from kria.tools.registry import tool_registry
    return {"tools": tool_registry.describe_all(), "count": len(tool_registry)}


@router.get("/tools/{tool_name}")
async def describe_tool(tool_name: str) -> dict:
    from kria.tools.registry import tool_registry
    spec = tool_registry.describe(tool_name)
    if spec is None:
        raise HTTPException(status_code=404, detail=f"Tool '{tool_name}' not found")
    return spec


# ── HITL ──────────────────────────────────────────────────────────

@router.post("/hitl/decide")
async def hitl_decide(decision: HITLDecision) -> dict:
    from kria.safety.hitl import hitl_gateway
    resolved = await hitl_gateway.submit_decision(decision.request_id, decision.approved)
    if not resolved:
        raise HTTPException(status_code=404, detail="Request not found or already resolved")
    return {"resolved": True, "approved": decision.approved}


@router.get("/hitl/pending")
async def hitl_pending() -> dict:
    from kria.safety.hitl import hitl_gateway
    return {"pending": hitl_gateway.get_pending()}


# ── Memory ────────────────────────────────────────────────────────

@router.get("/memory/search")
async def memory_search(q: str = Query(..., min_length=1), limit: int = Query(default=10, le=50)) -> dict:
    from kria.memory.conversation import conversation_memory
    results = await conversation_memory.search(q, limit=limit)
    return {"query": q, "results": results}


@router.get("/memory/preferences")
async def get_preferences() -> dict:
    from kria.memory.context_manager import context_manager
    prefs = await context_manager.get_all_preferences()
    return {"preferences": prefs}


@router.put("/memory/preferences/{key}")
async def set_preference(key: str, body: PreferenceUpdate) -> dict:
    from kria.memory.context_manager import context_manager
    await context_manager.set_preference(key, body.value, body.description)
    return {"key": key, "value": body.value, "set": True}


# ── UI Config (public, no auth) ──────────────────────────────────

@router.get("/ui-config")
async def ui_config() -> dict:
    """Read-only config consumed by the web dashboard (mic label, etc.)."""
    return {
        "mic_device_label": settings.mic_device_label,
    }


# ── Health ────────────────────────────────────────────────────────

@router.get("/health")
async def health_detail() -> dict:
    from kria.infra.health import health_registry
    return health_registry.summary()


# ── Safety / Emergency ────────────────────────────────────────────

@router.post("/safety/emergency")
async def set_emergency(req: EmergencyRequest) -> dict:
    from kria.safety.policy_engine import policy_engine
    policy_engine.set_emergency_mode(req.enabled)
    return {"emergency_mode": req.enabled}


# ── Audit log ─────────────────────────────────────────────────────

@router.get("/audit")
async def audit_log(
    limit: int = Query(default=50, le=500),
    risk_level: Optional[str] = Query(default=None),
    session_id: Optional[str] = Query(default=None),
) -> dict:
    from kria.safety.audit import audit_logger
    records = await audit_logger.query_recent(
        limit=limit, risk_level=risk_level, session_id=session_id
    )
    return {"records": records, "count": len(records)}
