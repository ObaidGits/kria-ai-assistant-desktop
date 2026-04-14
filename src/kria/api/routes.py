"""
REST API Routes
===============
All endpoints are versioned under /api/v1/.

Endpoints:
  POST   /chat                 — single-turn text interaction
  POST   /chat/upload          — chat with file attachments
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
import asyncio
import logging
import tempfile
from pathlib import Path
from typing import Annotated, Optional

from fastapi import APIRouter, Body, HTTPException, Query, UploadFile, File, Form
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


class InteractionChoice(BaseModel):
    request_id: str
    choice_index: int


class PreferenceUpdate(BaseModel):
    value: str
    description: str = ""


class EmergencyRequest(BaseModel):
    enabled: bool


class SettingsUpdate(BaseModel):
    key: str
    value: str


class ModelSwitchRequest(BaseModel):
    model_size: str  # "primary" or "secondary"
    force: bool = False  # bypass resource checks


class LLMModeRequest(BaseModel):
    mode: str  # "auto" | "primary" | "secondary" | "gemini"


class GeminiKeyRequest(BaseModel):
    api_key: str
    model: str = ""  # optional override, e.g. "gemini-1.5-pro"


class ExternalAPIRequest(BaseModel):
    url: str = ""     # e.g. https://api.groq.com/openai/v1
    api_key: str = "" # Bearer token (leave blank for unauthenticated endpoints)
    model: str = ""   # e.g. llama-3.1-8b-instant


# ── Model-switch async state ─────────────────────────────────────

_switch_state: dict = {
    "active": False,
    "phase": "idle",
    "message": "",
    "model_size": "",
    "previous_model": "",
    "switched": False,
    "resources": {},
    "error": None,
}


class SessionRenameRequest(BaseModel):
    title: str


# ── Background turn persistence ───────────────────────────────────

async def _save_turn_bg(ctx_mgr, session_id: str, user_input: str, response: str) -> None:
    """Save conversation turn in the background (fire-and-forget from chat handler)."""
    try:
        await ctx_mgr.save_turn(
            session_id=session_id,
            user_input=user_input,
            assistant_response=response,
        )
    except Exception as exc:
        logger.warning("[chat] background save_turn failed: %s", exc)


# ── Chat ──────────────────────────────────────────────────────────

@router.post("/chat", response_model=ChatResponse)
async def chat(req: ChatRequest) -> ChatResponse:
    from kria.voice.pipeline import voice_pipeline
    from kria.memory.context_manager import context_manager

    logger.info("[chat] session=%s message=%r", req.session_id, req.message[:120])

    result = await voice_pipeline.process_text_full(req.message, session_id=req.session_id)

    # Fire-and-forget: save in background so the HTTP response isn't delayed
    # by ChromaDB embedding or SQLite writes.
    asyncio.create_task(_save_turn_bg(
        context_manager, req.session_id, req.message, result["response"],
    ))

    logger.info(
        "[chat] session=%s tools_called=%d iterations=%d success=%s response=%r",
        req.session_id, len(result["tool_calls"]), result["iterations"],
        result["success"], result["response"][:120],
    )

    return ChatResponse(
        session_id=req.session_id,
        response=result["response"],
        tool_calls=result["tool_calls"],
        iterations=result["iterations"],
        success=result["success"],
    )


# ── Chat with file attachments ────────────────────────────────────

_UPLOAD_DIR = Path(tempfile.gettempdir()) / "kria_uploads"
_UPLOAD_DIR.mkdir(parents=True, exist_ok=True)

_IMAGE_EXTS = {".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"}
_TEXT_EXTS = {
    ".txt", ".md", ".py", ".js", ".ts", ".json", ".yaml", ".yml",
    ".xml", ".html", ".css", ".csv", ".log", ".sh", ".bash", ".cfg",
    ".ini", ".toml", ".rs", ".go", ".java", ".c", ".cpp", ".h",
    ".rb", ".php", ".sql", ".r", ".lua", ".vim",
}
_MAX_FILE_SIZE = 10 * 1024 * 1024  # 10 MB
_VISION_MAX_DIMENSION = 1280


async def _analyze_image(image_bytes: bytes, filename: str, user_message: str) -> str:
    """Send an image to the best available vision model for analysis.

    Uses the preprocessing pipeline for image optimization before sending
    to the vision model.

    Priority: cloud API (Gemini/external) if active → secondary local (Qwen2.5-VL).
    """
    import base64

    from kria.agent.model_router import model_router
    from kria.infra.health import health_registry
    from kria.preprocessing.image import preprocess_image

    # Preprocess image: EXIF rotation, adaptive resize, JPEG compression
    payload = await preprocess_image(
        filename,
        content=image_bytes,
        max_edge=_VISION_MAX_DIMENSION,
    )
    processed_bytes = payload.images[0] if payload.images else image_bytes

    # Determine MIME for data URI
    mime_sub = "jpeg"  # preprocessing always outputs JPEG

    if payload.metadata.get("processed_size"):
        logger.info(
            "Vision image preprocessed for %s: %s -> %s",
            filename,
            payload.metadata.get("original_size"),
            payload.metadata.get("processed_size"),
        )

    b64 = base64.b64encode(processed_bytes).decode("ascii")
    data_uri = f"data:image/{mime_sub};base64,{b64}"

    # Use the user's message as context, or a default prompt
    analysis_prompt = user_message.strip() if user_message.strip() else "Describe this image in detail."

    messages = [
        {"role": "user", "content": [
            {"type": "image_url", "image_url": {"url": data_uri}},
            {"type": "text", "text": analysis_prompt},
        ]},
    ]

    def _extract_content(result: Optional[dict]) -> str:
        if not result:
            return ""
        message = ((result.get("choices") or [{}])[0].get("message") or {})
        content = message.get("content", "")
        if isinstance(content, str):
            return content.strip()
        if isinstance(content, list):
            text_parts = [
                part.get("text", "").strip()
                for part in content
                if isinstance(part, dict) and part.get("type") == "text"
            ]
            return "\n".join(part for part in text_parts if part)
        return ""

    # Build ordered list of vision-capable clients from the config-driven router.
    # Priority: current mode's client → secondary (local vision) → any cloud fallback.
    mode = model_router.mode
    ordered_clients: list[tuple[str, object]] = []

    # If explicit mode is set and that client has vision, prefer it
    if mode != "auto":
        mode_client = model_router.get_client_by_name(mode)
        if mode_client and mode_client.is_configured and "vision" in mode_client.capabilities:
            ordered_clients.append((mode_client.model_label, mode_client))

    # Add all vision-capable clients not already in the list
    for name, client in model_router.clients.items():
        if "vision" in client.capabilities and client not in [c for _, c in ordered_clients]:
            # For local clients, verify health first
            cfg = model_router.config.models.get(name)
            if cfg and cfg.is_local:
                if health_registry.is_healthy(client.health_key) or await client.health_check():
                    ordered_clients.append((client.model_label, client))
            elif client.is_configured:
                ordered_clients.append((client.model_label, client))

    if not ordered_clients:
        logger.info("Vision analysis skipped for %s: no healthy/configured vision backend", filename)
        return ""

    for client_label, client in ordered_clients:
        try:
            result = await client.chat(messages=messages, max_tokens=1024, temperature=0.4)
            content = _extract_content(result)
            if content:
                return content
            logger.warning("Vision analysis returned no content for %s via %s", filename, client_label)
        except Exception as exc:
            logger.warning("Vision analysis failed for %s via %s: %s", filename, client_label, exc)
    return ""


@router.post("/chat/upload", response_model=ChatResponse)
async def chat_with_upload(
    message: str = Form(...),
    session_id: str = Form(default="default"),
    files: list[UploadFile] = File(default=[]),
) -> ChatResponse:
    """Chat with optional file attachments (images, text files, documents)."""
    from kria.voice.pipeline import voice_pipeline
    from kria.memory.context_manager import context_manager

    file_context_parts: list[str] = []
    saved_paths: list[str] = []

    for f in files:
        if not f.filename:
            continue
        data = await f.read()
        if len(data) > _MAX_FILE_SIZE:
            file_context_parts.append(f"[File '{f.filename}' skipped — exceeds 10 MB limit]")
            continue

        # Save to temp dir
        safe_name = Path(f.filename).name  # prevent path traversal
        save_path = _UPLOAD_DIR / f"{session_id}_{safe_name}"
        save_path.write_bytes(data)
        saved_paths.append(str(save_path))

        ext = Path(f.filename).suffix.lower()

        if ext in _IMAGE_EXTS:
            # Analyze image using the vision model (Qwen2.5-VL)
            analysis = await _analyze_image(data, f.filename, message)
            if analysis:
                file_context_parts.append(
                    f"--- Image Analysis of {f.filename} ---\n{analysis}\n--- End of Image Analysis ---"
                )
            else:
                file_context_parts.append(
                    f"[Image attached: {f.filename} ({len(data):,} bytes) — saved at {save_path}. "
                    f"Vision analysis unavailable — no vision backend is ready. "
                    f"Start `kria-brain-secondary` with `docker compose --profile secondary up -d` "
                    f"or configure Gemini/External API vision.]"
                )
        else:
            # Use the preprocessing pipeline for all non-image files
            try:
                from kria.preprocessing import preprocess
                from kria.infra.config import settings

                payload = await preprocess(
                    str(save_path),
                    content=data,
                    max_tokens=settings.preprocessing_max_tokens,
                    image_max_edge=settings.preprocessing_image_max_edge,
                    image_grayscale=settings.preprocessing_image_grayscale,
                    keyframe_max=settings.preprocessing_keyframe_max,
                    scene_threshold=settings.preprocessing_scene_threshold,
                )
                if payload.text:
                    trunc_note = " (token-budgeted)" if payload.truncated else ""
                    file_context_parts.append(
                        f"--- Content of {f.filename} [{payload.source_type}]{trunc_note} ---\n"
                        f"{payload.text}\n"
                        f"--- End of {f.filename} (~{payload.token_estimate} tokens) ---"
                    )
                else:
                    file_context_parts.append(
                        f"[File attached: {f.filename} ({len(data):,} bytes, type: {ext}) "
                        f"— saved at {save_path}. Could not extract text content.]"
                    )
            except Exception as exc:
                logger.warning("Preprocessing failed for %s: %s", f.filename, exc)
                file_context_parts.append(
                    f"[File attached: {f.filename} ({len(data):,} bytes, type: {ext}) — saved at {save_path}]"
                )

    # Build the augmented message
    if file_context_parts:
        augmented_message = message + "\n\n" + "\n\n".join(file_context_parts)
    else:
        augmented_message = message

    logger.info("[chat/upload] session=%s files=%d message=%r",
                session_id, len(files), message[:120])

    result = await voice_pipeline.process_text_full(augmented_message, session_id=session_id)

    asyncio.create_task(_save_turn_bg(
        context_manager, session_id, message, result["response"],
    ))

    return ChatResponse(
        session_id=session_id,
        response=result["response"],
        tool_calls=result["tool_calls"],
        iterations=result["iterations"],
        success=result["success"],
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


@router.delete("/sessions")
async def delete_all_sessions() -> dict:
    from kria.memory.conversation import conversation_memory
    from kria.memory.semantic import semantic_memory
    deleted = await conversation_memory.delete_all_sessions()
    # Best-effort: clear semantic memory (may not support bulk delete — ignore errors)
    try:
        sessions = await conversation_memory.get_sessions()
        for sid in sessions:
            await semantic_memory.delete_by_session(sid)
    except Exception:
        pass
    return {"deleted": True, "turns_removed": deleted}


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


# ── Interaction (ask_user) ────────────────────────────────────────

@router.post("/interaction/decide")
async def interaction_decide(choice: InteractionChoice) -> dict:
    from kria.agent.interaction import interaction_gateway
    resolved = await interaction_gateway.submit_choice(choice.request_id, choice.choice_index)
    if not resolved:
        raise HTTPException(status_code=404, detail="Request not found or already resolved")
    return {"resolved": True, "choice_index": choice.choice_index}


@router.get("/interaction/pending")
async def interaction_pending() -> dict:
    from kria.agent.interaction import interaction_gateway
    return {"pending": interaction_gateway.get_pending()}


# ── Agent control ─────────────────────────────────────────────────

@router.post("/agent/terminate")
async def agent_terminate() -> dict:
    from kria.agent.loop import signal_terminate
    from kria.agent.interaction import interaction_gateway
    signal_terminate()
    interaction_gateway.cancel_all()
    return {"terminated": True}


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


# ── Mem0 Fact Memory ─────────────────────────────────────────────

@router.get("/memory/facts")
async def list_facts() -> dict:
    """Return all Mem0-managed facts for the current user."""
    from kria.memory.mem0_memory import mem0_memory
    facts = await mem0_memory.get_all()
    return {"facts": facts, "count": len(facts)}


@router.get("/memory/facts/search")
async def search_facts(
    q: str = Query(..., min_length=1),
    limit: int = Query(default=10, le=50),
) -> dict:
    """Semantic search across stored facts."""
    from kria.memory.mem0_memory import mem0_memory
    results = await mem0_memory.search(query=q, limit=limit)
    return {"query": q, "results": results, "count": len(results)}


@router.delete("/memory/facts/{memory_id}")
async def delete_fact(memory_id: str) -> dict:
    """Delete a specific fact by its Mem0 ID."""
    from kria.memory.mem0_memory import mem0_memory
    ok = await mem0_memory.delete(memory_id)
    if not ok:
        raise HTTPException(status_code=404, detail="Fact not found or Mem0 unavailable")
    return {"deleted": True, "memory_id": memory_id}


@router.delete("/memory/facts")
async def delete_all_facts() -> dict:
    """Delete ALL stored facts.  Irreversible."""
    from kria.memory.mem0_memory import mem0_memory
    ok = await mem0_memory.delete_all()
    return {"deleted": ok}


@router.get("/memory/facts/{memory_id}/history")
async def fact_history(memory_id: str) -> dict:
    """Get the change history for a specific fact."""
    from kria.memory.mem0_memory import mem0_memory
    history = await mem0_memory.history(memory_id)
    return {"memory_id": memory_id, "history": history}


# ── UI Config (public, no auth) ──────────────────────────────────

@router.get("/ui-config")
async def ui_config() -> dict:
    """Read-only config consumed by the web dashboard (mic label, etc.)."""
    return {
        "mic_device_label": settings.mic_device_label,
    }


# ── Settings ──────────────────────────────────────────────────────

@router.get("/settings")
async def get_settings() -> dict:
    """Return all user-facing settings with their current values."""
    from kria.agent.model_router import model_router
    from kria.memory.mem0_memory import mem0_memory
    status = model_router.status_dict()
    return {
        "model": {
            "current_size": _get_current_model_size(),
            "available": sorted(model_router.clients.keys()),
            "names": status.get("labels", {}),
            "llm_mode": status["mode"],
            "llm_mode_options": status["available_modes"],
            "gemini_configured": status.get("gemini_configured", False),
            "gemini_model": status.get("gemini_model", ""),
            "external_configured": status.get("external_configured", False),
            "external_url": status.get("external_url", ""),
            "external_model": status.get("external_model", ""),
        },
        "memory": {
            "mem0_available": mem0_memory.available,
        },
        "safety": {
            "emergency_mode": settings.emergency_mode,
            "default_risk_level": settings.default_risk_level,
        },
        "internet": {
            "enabled": settings.internet_enabled,
            "https_only": settings.internet_https_only,
            "max_download_size_mb": settings.max_download_size_mb,
        },
        "voice": {
            "enabled": settings.voice_enabled,
            "wake_word": settings.wake_word,
            "language": settings.language,
        },
        "general": {
            "max_context_turns": settings.max_context_turns,
            "tool_timeout_seconds": settings.tool_timeout_seconds,
            "hitl_timeout_seconds": settings.hitl_timeout_seconds,
            "notifications_enabled": settings.notifications_enabled,
            "automation_enabled": settings.automation_enabled,
        },
    }


def _get_current_model_size() -> str:
    """Detect which local model the primary brain is running."""
    import os
    return os.environ.get("KRIA_ACTIVE_MODEL", "primary")


# ── Model-switch helpers ──────────────────────────────────────────

_MODEL_REQUIREMENTS = {
    "primary":   {"ram_gb": 4.0, "vram_gb": 3.0, "label": "Phi-4-mini-instruct 3.8B"},
    "secondary": {"ram_gb": 8.0, "vram_gb": 6.0, "label": "Qwen2.5-VL-7B-Instruct"},
}

async def _check_system_resources() -> dict:
    """Check available RAM and GPU VRAM on the host."""
    import subprocess
    resources: dict = {}

    # RAM via /proc/meminfo (works in pid:host mode without nsenter too)
    try:
        result = subprocess.run(
            ["nsenter", "-t", "1", "-m", "-u", "-i", "-n", "-p", "--", "free", "-b"],
            capture_output=True, text=True, timeout=5,
        )
        if result.returncode == 0:
            for line in result.stdout.splitlines():
                if line.startswith("Mem:"):
                    parts = line.split()
                    resources["ram_total_gb"] = round(int(parts[1]) / (1024**3), 1)
                    resources["ram_available_gb"] = round(int(parts[6]) / (1024**3), 1) if len(parts) > 6 else round(int(parts[3]) / (1024**3), 1)
    except Exception as e:
        logger.debug("RAM check failed: %s", e)

    # GPU VRAM
    try:
        result = subprocess.run(
            ["nsenter", "-t", "1", "-m", "-u", "-i", "-n", "-p", "--",
             "nvidia-smi", "--query-gpu=memory.total,memory.free,name", "--format=csv,noheader,nounits"],
            capture_output=True, text=True, timeout=5,
        )
        if result.returncode == 0 and result.stdout.strip():
            parts = [p.strip() for p in result.stdout.strip().split("\n")[0].split(",")]
            resources["vram_total_gb"] = round(int(parts[0]) / 1024, 1)
            resources["vram_free_gb"] = round(int(parts[1]) / 1024, 1)
            resources["gpu_name"] = parts[2] if len(parts) > 2 else "Unknown"
    except Exception as e:
        logger.debug("GPU check failed: %s", e)

    return resources


async def _restart_docker_container(name: str, timeout: int = 30) -> tuple[bool, str]:
    """Restart a Docker container via the Docker Engine API (unix socket)."""
    import httpx as _httpx
    try:
        transport = _httpx.AsyncHTTPTransport(uds="/var/run/docker.sock")
        async with _httpx.AsyncClient(transport=transport, base_url="http://docker") as client:
            resp = await client.post(
                f"/v1.43/containers/{name}/restart",
                params={"t": timeout},
                timeout=float(timeout + 15),
            )
            if resp.status_code == 204:
                return True, "Container restarting"
            elif resp.status_code == 404:
                return False, f"Container '{name}' not found"
            else:
                body = resp.text[:200]
                return False, f"Docker API returned {resp.status_code}: {body}"
    except Exception as e:
        return False, f"Docker socket error: {e}"


async def _wait_for_brain_health(max_wait: int = 90, poll_interval: float = 3.0) -> tuple[bool, str]:
    """Poll the LLM health probe until brain is healthy or timeout."""
    import asyncio, httpx as _httpx
    brain_url = settings.llama_api_url.rstrip("/") + "/health"
    waited = 0.0
    while waited < max_wait:
        try:
            async with _httpx.AsyncClient(timeout=5.0) as client:
                resp = await client.get(brain_url)
                if resp.status_code == 200:
                    return True, "Brain is healthy"
        except Exception:
            pass
        await asyncio.sleep(poll_interval)
        waited += poll_interval
    return False, f"Brain did not become healthy within {max_wait}s"


@router.post("/settings/model")
async def switch_model(req: ModelSwitchRequest) -> dict:
    """
    Start an async model switch.  Returns immediately; poll
    GET /settings/model/status for progress.
    """
    import asyncio

    if _switch_state["active"]:
        return {"started": False, "phase": "already_switching",
                "message": "A model switch is already in progress."}

    if req.model_size not in _MODEL_REQUIREMENTS:
        raise HTTPException(status_code=400, detail="model_size must be 'primary' or 'secondary'")

    current = _get_current_model_size()
    reqs = _MODEL_REQUIREMENTS[req.model_size]

    if current == req.model_size:
        return {"started": False, "phase": "check",
                "message": f"Already running {reqs['label']}", "model_size": current}

    # ── Phase 1: Resource check (fast, stays synchronous) ──
    resources = await _check_system_resources()
    issues = []
    has_gpu = "vram_free_gb" in resources

    if has_gpu:
        if resources.get("vram_free_gb", 0) < reqs["vram_gb"]:
            issues.append(f"Need {reqs['vram_gb']}GB VRAM, only {resources.get('vram_free_gb', '?')}GB free")
    else:
        if resources.get("ram_available_gb", 0) < reqs["ram_gb"]:
            issues.append(f"Need {reqs['ram_gb']}GB RAM, only {resources.get('ram_available_gb', '?')}GB available")

    if issues and not req.force:
        logger.warning("Model switch blocked — resource issues: %s", issues)
        return {
            "started": False, "phase": "resource_check",
            "message": "Insufficient resources for " + reqs["label"],
            "issues": issues, "resources": resources, "can_force": True,
        }

    # ── Kick off background task ──
    _switch_state.update({
        "active": True, "phase": "starting", "switched": False,
        "message": "Starting model switch…",
        "model_size": req.model_size, "previous_model": current,
        "resources": resources, "error": None,
    })
    asyncio.create_task(_do_model_switch(req.model_size, current, resources))
    return {"started": True, "phase": "starting",
            "message": f"Switching to {reqs['label']}…"}


async def _do_model_switch(target: str, previous: str, resources: dict):
    """Background task that performs the actual model switch."""
    import os, pathlib
    from kria.infra.health import health_registry, ServiceStatus

    reqs = _MODEL_REQUIREMENTS[target]
    try:
        # ── Phase 2: Write model config ──
        _switch_state.update(phase="write_config", message="Writing model config…")
        pathlib.Path("/data/model_size").write_text(target)

        logger.info("Model switch: %s -> %s", previous, target)

        # ── Phase 3: Restart brain ──
        _switch_state.update(phase="restarting", message="Restarting brain container…")
        health_registry.update("llm", ServiceStatus.DEGRADED, "Model switch in progress")

        ok, msg = await _restart_docker_container("kria-brain", timeout=30)
        if not ok:
            logger.error("Brain restart failed: %s", msg)
            health_registry.update("llm", ServiceStatus.DOWN, msg)
            _switch_state.update(active=False, phase="restart_failed",
                                 message=f"Failed to restart brain: {msg}", error=msg)
            return

        # ── Phase 4: Wait for healthy ──
        _switch_state.update(phase="waiting_healthy", message="Waiting for brain to load model…")
        healthy, health_msg = await _wait_for_brain_health(max_wait=120)

        if healthy:
            os.environ["KRIA_ACTIVE_MODEL"] = target
            health_registry.update("llm", ServiceStatus.HEALTHY)
            try:
                from kria.agent.model_router import model_router as _mr
                _primary = _mr.get_client_by_name("primary")
                if _primary and hasattr(_primary, '_circuit') and _primary._circuit:
                    from kria.infra.circuit_breaker import CircuitState
                    _primary._circuit._failure_count = 0
                    _primary._circuit._state = CircuitState.CLOSED
            except Exception:
                pass
            logger.info("Model switch complete: now running %s", target)
            _switch_state.update(active=False, phase="complete", switched=True,
                                 message=f"Successfully switched to {reqs['label']}",
                                 model_size=target)
            return

        # ── Phase 5: Auto-rollback ──
        logger.error("Brain not healthy after switch to %s — rolling back to %s", target, previous)
        _switch_state.update(phase="rolling_back", message=f"Model failed to load, rolling back to {previous}…")

        try:
            pathlib.Path("/data/model_size").write_text(previous)
            rb_ok, rb_msg = await _restart_docker_container("kria-brain", timeout=30)
            if rb_ok:
                rb_healthy, _ = await _wait_for_brain_health(max_wait=90)
                if rb_healthy:
                    os.environ["KRIA_ACTIVE_MODEL"] = previous
                    health_registry.update("llm", ServiceStatus.HEALTHY)
                    logger.info("Rollback to %s successful", previous)
                    _switch_state.update(
                        active=False, phase="rollback_ok", switched=False,
                        message=f"Model {reqs['label']} failed to load (likely OOM). Rolled back to {_MODEL_REQUIREMENTS[previous]['label']}.",
                        model_size=previous)
                    return
        except Exception as rb_err:
            logger.error("Rollback failed: %s", rb_err)

        health_registry.update("llm", ServiceStatus.DOWN, health_msg)
        _switch_state.update(
            active=False, phase="rollback_failed", switched=False,
            message=f"Model switch failed and rollback to {previous} also failed. Use the Restart button.",
            model_size=previous, error=health_msg)

    except Exception as exc:
        logger.exception("Unexpected error during model switch")
        _switch_state.update(active=False, phase="error",
                             message=f"Unexpected error: {exc}", error=str(exc))


@router.get("/settings/model/status")
async def model_status() -> dict:
    """Return switch progress (if active) or current brain status."""
    from kria.infra.health import health_registry
    brain = health_registry.get_all().get("llm")

    # If a switch is active, return the live state
    if _switch_state["active"]:
        return {
            "switching": True,
            "phase": _switch_state["phase"],
            "message": _switch_state["message"],
            "model_size": _switch_state["model_size"],
            "previous_model": _switch_state["previous_model"],
        }

    # If a switch just finished, return final result and reset
    phase = _switch_state["phase"]
    if phase not in ("idle",):
        result = {
            "switching": False,
            "phase": phase,
            "message": _switch_state["message"],
            "model_size": _switch_state.get("model_size", _get_current_model_size()),
            "switched": _switch_state.get("switched", False),
            "error": _switch_state.get("error"),
        }
        # Reset state for next switch
        _switch_state.update(active=False, phase="idle", message="",
                             switched=False, error=None)
        return result

    # No switch in progress or recently finished
    return {
        "switching": False,
        "phase": "idle",
        "model_size": _get_current_model_size(),
        "brain_status": brain.status.value if brain else "unknown",
        "brain_error": brain.error if brain else None,
    }


@router.get("/settings/model/resources")
async def model_resources() -> dict:
    """Pre-flight resource check for model switching."""
    resources = await _check_system_resources()
    return {"resources": resources, "requirements": _MODEL_REQUIREMENTS}


# ── LLM Routing Mode ──────────────────────────────────────────────

@router.get("/settings/llm-mode")
async def get_llm_mode() -> dict:
    """Return current LLM routing mode and available options."""
    from kria.agent.model_router import model_router
    return model_router.status_dict()


@router.post("/settings/llm-mode")
async def set_llm_mode(req: LLMModeRequest) -> dict:
    """
    Switch the LLM routing mode at runtime — no container restart required.

    Modes:
      auto      — smart per-request routing (AGENT_LOOP → secondary, rest → primary)
      primary   — always Phi-4-mini (fast, low VRAM)
      secondary — always Qwen2.5-VL-7B (smart, more VRAM)
      gemini    — bypass local models; use Google Gemini API
    """
    from kria.agent.model_router import model_router
    try:
        model_router.set_mode(req.mode)
    except ValueError as exc:
        raise HTTPException(status_code=400, detail=str(exc))
    logger.info("[settings] llm_mode changed → %s", req.mode)
    return {"mode": req.mode, "changed": True, **model_router.status_dict()}


@router.post("/settings/gemini-key")
async def set_gemini_key(req: GeminiKeyRequest) -> dict:
    """
    Save a Google Gemini API key at runtime.
    The key is stored in memory only — it is never written to disk by this endpoint.
    Optionally update the Gemini model name (e.g. 'gemini-1.5-pro').
    """
    from kria.agent.model_router import model_router
    if not req.api_key and not req.model:
        raise HTTPException(status_code=400, detail="api_key must not be empty")
    overrides: dict = {}
    if req.api_key:
        overrides["api_key"] = req.api_key
    if req.model:
        overrides["model"] = req.model
    model_router.reconfigure("gemini", **overrides)
    gemini = model_router.get_client_by_name("gemini")
    logger.info("[settings] Gemini API key updated (length=%d)", len(req.api_key))
    return {
        "gemini_configured": gemini.is_configured if gemini else False,
        "gemini_model": gemini._model_id if gemini else "",
        "changed": True,
    }


@router.post("/settings/external-api")
async def set_external_api(req: ExternalAPIRequest) -> dict:
    """
    Configure the External (OpenAI-compatible) API at runtime.
    Supported providers: Groq, OpenRouter, Together AI, Mistral, LM Studio, Ollama, etc.
    The key is stored in memory only — it is never written to disk.
    """
    from kria.agent.model_router import model_router
    if not req.url and not req.model:
        raise HTTPException(
            status_code=400,
            detail="Provide at least url and model to configure an external API.",
        )
    overrides: dict = {}
    if req.url:
        overrides["url"] = req.url
    if req.api_key:
        overrides["api_key"] = req.api_key
    if req.model:
        overrides["model"] = req.model
    model_router.reconfigure("external", **overrides)
    ext = model_router.get_client_by_name("external")
    logger.info("[settings] External API configured: url=%s model=%s",
                ext._base_url if ext else "", ext._model_id if ext else "")
    return {
        "external_configured": ext.is_configured if ext else False,
        "external_url": ext._base_url if ext else "",
        "external_model": ext._model_id if ext else "",
        "changed": True,
    }


@router.post("/services/restart/{service}")
async def restart_service(service: str) -> dict:
    """Restart a KRIA service container via Docker API."""
    allowed = {"kria-brain", "kria-brain-secondary", "kria-voice", "kria-data", "kria-qdrant"}
    if service not in allowed:
        raise HTTPException(status_code=400, detail=f"Cannot restart '{service}'. Allowed: {sorted(allowed)}")
    ok, msg = await _restart_docker_container(service, timeout=30)
    if not ok:
        raise HTTPException(status_code=500, detail=msg)
    logger.info("Service %s restarted via API", service)
    return {"restarted": True, "service": service, "message": msg}


async def _stop_docker_container(name: str, timeout: int = 15) -> tuple[bool, str]:
    """Stop a running Docker container via the Docker Engine API."""
    import httpx as _httpx
    try:
        transport = _httpx.AsyncHTTPTransport(uds="/var/run/docker.sock")
        async with _httpx.AsyncClient(transport=transport, base_url="http://docker") as client:
            resp = await client.post(
                f"/v1.43/containers/{name}/stop",
                params={"t": timeout},
                timeout=float(timeout + 10),
            )
            if resp.status_code in (204, 304):  # 304 = already stopped
                return True, "Container stopped"
            elif resp.status_code == 404:
                return False, f"Container '{name}' not found"
            else:
                return False, f"Docker API returned {resp.status_code}: {resp.text[:200]}"
    except Exception as e:
        return False, f"Docker socket error: {e}"


async def _start_or_create_secondary() -> tuple[bool, str]:
    """Start kria-brain-secondary — creates the container if it doesn't exist yet."""
    import httpx as _httpx
    name = "kria-brain-secondary"
    image = "docker-kria-brain-secondary:latest"

    try:
        transport = _httpx.AsyncHTTPTransport(uds="/var/run/docker.sock")
        async with _httpx.AsyncClient(transport=transport, base_url="http://docker") as client:
            # ── Check if container already exists ────────────────
            info_resp = await client.get(f"/v1.43/containers/{name}/json", timeout=10.0)
            if info_resp.status_code == 200:
                state = info_resp.json().get("State", {})
                if state.get("Running"):
                    return True, "Container is already running"
                # Exists but stopped — just start it
                start_resp = await client.post(f"/v1.43/containers/{name}/start", timeout=30.0)
                if start_resp.status_code in (204, 304):
                    return True, "Container started"
                return False, f"Start failed: {start_resp.status_code} {start_resp.text[:200]}"

            elif info_resp.status_code == 404:
                # ── Container doesn't exist — read primary brain config ──
                brain_resp = await client.get("/v1.43/containers/kria-brain/json", timeout=10.0)
                if brain_resp.status_code != 200:
                    return False, "Cannot read primary brain config to derive secondary settings"
                brain = brain_resp.json()
                binds = brain["HostConfig"].get("Binds") or []
                mem = brain["HostConfig"].get("Memory", 8589934592)

                # ── Create the secondary container ────────────────
                create_body = {
                    "Image": image,
                    "Env": ["LLAMA_BRAIN_ROLE=secondary", "LLAMA_PORT=8085", "LLAMA_GPU_LAYERS=0"],
                    "ExposedPorts": {"8085/tcp": {}},
                    "HostConfig": {
                        "Binds": binds,
                        "PortBindings": {"8085/tcp": [{"HostIp": "127.0.0.1", "HostPort": "8085"}]},
                        "Memory": mem,
                        "RestartPolicy": {"Name": "unless-stopped"},
                    },
                    "NetworkingConfig": {
                        "EndpointsConfig": {"kria-net": {}}
                    },
                }
                create_resp = await client.post(
                    "/v1.43/containers/create",
                    params={"name": name},
                    json=create_body,
                    timeout=30.0,
                )
                if create_resp.status_code not in (201, 200):
                    return False, f"Create failed: {create_resp.status_code} {create_resp.text[:300]}"

                # ── Start the new container ───────────────────────
                start_resp = await client.post(f"/v1.43/containers/{name}/start", timeout=30.0)
                if start_resp.status_code in (204, 304):
                    return True, "Container created and started"
                return False, f"Container created but start failed: {start_resp.status_code}"

            else:
                return False, f"Docker inspect returned {info_resp.status_code}"

    except Exception as e:
        return False, f"Docker socket error: {e}"


@router.post("/services/start/{service}")
async def start_service(service: str) -> dict:
    """Start (or create) a KRIA service container — supports profile-gated services."""
    allowed = {"kria-brain-secondary"}
    if service not in allowed:
        raise HTTPException(status_code=400, detail=f"Use /services/restart for already-running services. Start allowed for: {sorted(allowed)}")
    ok, msg = await _start_or_create_secondary()
    if not ok:
        raise HTTPException(status_code=500, detail=msg)
    logger.info("Service %s started via API: %s", service, msg)
    return {"started": True, "service": service, "message": msg}


@router.post("/services/stop/{service}")
async def stop_service(service: str) -> dict:
    """Stop a KRIA service container (non-destructive — container is preserved)."""
    allowed = {"kria-brain-secondary"}
    if service not in allowed:
        raise HTTPException(status_code=400, detail=f"Stop allowed for: {sorted(allowed)}")
    ok, msg = await _stop_docker_container(service, timeout=15)
    if not ok:
        raise HTTPException(status_code=500, detail=msg)
    logger.info("Service %s stopped via API", service)
    return {"stopped": True, "service": service, "message": msg}


@router.post("/settings/update")
async def update_setting(body: SettingsUpdate) -> dict:
    """Update a runtime setting (stored as user preference)."""
    from kria.memory.context_manager import context_manager
    allowed_keys = {
        "max_context_turns", "tool_timeout_seconds", "hitl_timeout_seconds",
        "internet_enabled", "voice_enabled", "wake_word", "language",
        "notifications_enabled", "automation_enabled",
    }
    if body.key not in allowed_keys:
        raise HTTPException(status_code=400, detail=f"Setting '{body.key}' cannot be changed at runtime. Allowed: {sorted(allowed_keys)}")

    # Update the live settings object
    try:
        current = getattr(settings, body.key)
        target_type = type(current)
        if target_type == bool:
            coerced = body.value.lower() in ("true", "1", "yes")
        elif target_type == int:
            coerced = int(body.value)
        elif target_type == float:
            coerced = float(body.value)
        else:
            coerced = body.value
        object.__setattr__(settings, body.key, coerced)
    except Exception as exc:
        raise HTTPException(status_code=400, detail=f"Invalid value: {exc}")

    # Persist as user preference so it survives restarts
    await context_manager.set_preference(f"setting:{body.key}", body.value, f"Runtime setting override")
    logger.info("Setting updated: %s = %s", body.key, body.value)
    return {"key": body.key, "value": body.value, "applied": True}


# ── Enhanced Sessions ─────────────────────────────────────────────

@router.get("/sessions/detail")
async def list_sessions_detail() -> dict:
    """Return sessions with metadata (title, turn count, last activity)."""
    from kria.memory.conversation import conversation_memory
    from kria.memory.persistent import sqlite_manager

    rows = await sqlite_manager.execute(
        """SELECT session_id,
                  COUNT(*) as turn_count,
                  MIN(timestamp) as first_turn,
                  MAX(timestamp) as last_turn,
                  (SELECT content FROM conversations c2
                   WHERE c2.session_id = c.session_id AND c2.role = 'user'
                   ORDER BY c2.id ASC LIMIT 1) as first_message
           FROM conversations c
           GROUP BY session_id
           ORDER BY MAX(id) DESC""",
        (),
    )
    sessions = []
    for r in rows:
        first_msg = r.get("first_message", "") or ""
        title = first_msg[:60] + ("…" if len(first_msg) > 60 else "") if first_msg else r["session_id"][:12]
        sessions.append({
            "session_id": r["session_id"],
            "title": title,
            "turn_count": r["turn_count"],
            "first_turn": r.get("first_turn"),
            "last_turn": r.get("last_turn"),
        })
    return {"sessions": sessions}


@router.put("/sessions/{session_id}/rename")
async def rename_session(session_id: str, body: SessionRenameRequest) -> dict:
    """Store a custom title for a session (as a preference)."""
    from kria.memory.context_manager import context_manager
    await context_manager.set_preference(f"session_title:{session_id}", body.title, "Custom session title")
    return {"session_id": session_id, "title": body.title, "renamed": True}


# ── Health ────────────────────────────────────────────────────────

@router.get("/health")
async def health_detail() -> dict:
    from kria.infra.health import health_registry
    summary = health_registry.summary()
    # Add error details for each service that isn't healthy
    details = {}
    for name, svc in health_registry.get_all().items():
        details[name] = {"status": svc.status.value, "error": svc.error}
    summary["_details"] = details
    return summary


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
