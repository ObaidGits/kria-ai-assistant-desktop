"""
WebSocket Manager
=================
Provides a persistent bidirectional connection to the browser dashboard
and any voice clients.

Protocol (JSON messages):
  Client → Server:
    {"type": "chat",    "message": "...", "session_id": "..."}
    {"type": "audio",   "data": "<base64 WAV>"}         ← push-to-talk
    {"type": "hitl",    "request_id": "...", "approved": true|false}
    {"type": "ping"}

  Server → Client:
    {"type": "chat_response",  "response": "...", "session_id": "..."}
    {"type": "stream_token",   "token": "..."}           ← streaming TTS
    {"type": "hitl_request",   "id": "...", "action": "...", ...}
    {"type": "status_update",  "service": "...", "status": "..."}
    {"type": "error",          "message": "..."}
    {"type": "pong"}

Connections are stored in a set.  Broadcasting iterates all live
connections and silently drops dead ones.
"""
import asyncio
import base64
import json
import logging
from typing import Any

from fastapi import APIRouter, WebSocket, WebSocketDisconnect

logger = logging.getLogger("kria.api.websocket")

ws_router = APIRouter(tags=["websocket"])

# All active WebSocket connections
_connections: set[WebSocket] = set()


# ── Broadcast helper (used by HITL gateway) ────────────────────────

async def broadcast(data: dict | str) -> None:
    """Send a message to every connected client."""
    if not _connections:
        return
    payload = json.dumps(data) if isinstance(data, dict) else data
    dead: set[WebSocket] = set()
    for ws in list(_connections):
        try:
            await ws.send_text(payload)
        except Exception:
            dead.add(ws)
    _connections.difference_update(dead)


def ws_clients_count() -> int:
    """Return the number of active WebSocket connections."""
    return len(_connections)


# ── WebSocket endpoint ────────────────────────────────────────────

@ws_router.websocket("/ws")
async def websocket_endpoint(websocket: WebSocket) -> None:
    await websocket.accept()
    _connections.add(websocket)
    logger.info("WebSocket connected. Active: %d", len(_connections))

    try:
        while True:
            raw = await websocket.receive_text()
            try:
                msg = json.loads(raw)
            except json.JSONDecodeError:
                await websocket.send_json({"type": "error", "message": "Invalid JSON"})
                continue

            await _handle_message(websocket, msg)

    except WebSocketDisconnect:
        logger.info("WebSocket disconnected")
    except Exception as exc:
        logger.error("WebSocket error: %s", exc)
    finally:
        _connections.discard(websocket)
        logger.debug("Active WebSocket connections: %d", len(_connections))


async def _handle_message(ws: WebSocket, msg: dict) -> None:
    msg_type = msg.get("type", "")

    if msg_type == "ping":
        await ws.send_json({"type": "pong"})
        return

    if msg_type == "chat":
        await _handle_chat(ws, msg)
        return

    if msg_type == "audio":
        await _handle_audio(ws, msg)
        return

    if msg_type == "hitl":
        await _handle_hitl(ws, msg)
        return

    if msg_type == "interaction_choice":
        await _handle_interaction_choice(ws, msg)
        return

    await ws.send_json({"type": "error", "message": f"Unknown message type: {msg_type!r}"})


async def _handle_chat(ws: WebSocket, msg: dict) -> None:
    user_input = msg.get("message", "").strip()
    session_id = msg.get("session_id", "ws_default")
    if not user_input:
        await ws.send_json({"type": "error", "message": "Empty message"})
        return
    try:
        from kria.voice.pipeline import voice_pipeline
        from kria.memory.context_manager import context_manager
        response = await voice_pipeline.process_text(user_input, session_id=session_id)
        await context_manager.save_turn(session_id, user_input, response)
        await ws.send_json({
            "type": "chat_response",
            "session_id": session_id,
            "response": response,
        })
    except Exception as exc:
        logger.error("WebSocket chat error: %s", exc)
        await ws.send_json({"type": "error", "message": str(exc)})


async def _handle_audio(ws: WebSocket, msg: dict) -> None:
    audio_b64 = msg.get("data", "")
    session_id = msg.get("session_id", "ws_audio")
    try:
        audio_bytes = base64.b64decode(audio_b64)
        from kria.voice.pipeline import voice_pipeline
        transcript, response = await voice_pipeline.push_audio(audio_bytes)
        await ws.send_json({
            "type": "chat_response",
            "session_id": session_id,
            "transcript": transcript,
            "response": response,
        })
    except Exception as exc:
        logger.error("WebSocket audio error: %s", exc)
        await ws.send_json({"type": "error", "message": str(exc)})


async def _handle_hitl(ws: WebSocket, msg: dict) -> None:
    request_id = msg.get("request_id", "")
    approved = bool(msg.get("approved", False))
    if not request_id:
        await ws.send_json({"type": "error", "message": "Missing request_id"})
        return
    try:
        from kria.safety.hitl import hitl_gateway
        resolved = await hitl_gateway.submit_decision(request_id, approved)
        await ws.send_json({
            "type": "hitl_response",
            "request_id": request_id,
            "resolved": resolved,
            "approved": approved,
        })
    except Exception as exc:
        await ws.send_json({"type": "error", "message": str(exc)})


async def _handle_interaction_choice(ws: WebSocket, msg: dict) -> None:
    request_id = msg.get("request_id", "")
    choice_index = msg.get("choice_index", 0)
    if not request_id:
        await ws.send_json({"type": "error", "message": "Missing request_id"})
        return
    try:
        from kria.agent.interaction import interaction_gateway
        resolved = await interaction_gateway.submit_choice(request_id, int(choice_index))
        await ws.send_json({
            "type": "interaction_response",
            "request_id": request_id,
            "resolved": resolved,
            "choice_index": choice_index,
        })
    except Exception as exc:
        await ws.send_json({"type": "error", "message": str(exc)})
