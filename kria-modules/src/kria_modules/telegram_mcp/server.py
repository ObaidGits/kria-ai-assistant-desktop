#!/usr/bin/env python3
"""
K.R.I.A. Telegram MCP Server — stdio-based MCP server for Telegram integration.

This server implements the MCP (Model Context Protocol) over stdio,
providing tools for sending/receiving Telegram messages. It also runs
a background polling loop to forward incoming Telegram messages to KRIA.

Usage:
    python -m kria_modules.telegram_mcp.server

Environment variables:
    TELEGRAM_BOT_TOKEN  — Telegram bot API token (from @BotFather)
    TELEGRAM_CHAT_IDS   — Comma-separated allowed chat IDs (security restriction)
    KRIA_API_URL        — KRIA server URL for forwarding messages (default: http://127.0.0.1:3001)
"""

import asyncio
import json
import logging
import os
import sys
import time
from typing import Any

import httpx

logging.basicConfig(
    stream=sys.stderr,
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
    datefmt="%H:%M:%S",
)
logger = logging.getLogger("kria.telegram_mcp")

TELEGRAM_API = "https://api.telegram.org/bot{token}/{method}"
BOT_TOKEN: str = ""
ALLOWED_CHAT_IDS: set[int] = set()
KRIA_API_URL: str = "http://127.0.0.1:3001"
TELEGRAM_CONFLICT_BASE_BACKOFF_SECS = 2
TELEGRAM_CONFLICT_MAX_BACKOFF_SECS = 90
TELEGRAM_CONFLICT_JITTER_MAX_MS = 1200

# Track the last processed update to avoid duplicates
_last_update_id: int = 0


# ── Telegram Bot API helpers ────────────────────────────────────

async def telegram_api(method: str, params: dict | None = None) -> dict:
    """Call a Telegram Bot API method."""
    if not BOT_TOKEN:
        return {"ok": False, "description": "Bot token not configured"}
    url = TELEGRAM_API.format(token=BOT_TOKEN, method=method)
    async with httpx.AsyncClient(timeout=30) as client:
        resp = await client.post(url, json=params or {})
        resp.raise_for_status()
        return resp.json()


async def send_telegram_message(chat_id: int, text: str, parse_mode: str = "Markdown") -> dict:
    """Send a message to a Telegram chat."""
    if not _is_allowed_chat(chat_id):
        return {"ok": False, "description": f"Chat {chat_id} not in allowed list"}
    return await telegram_api("sendMessage", {
        "chat_id": chat_id,
        "text": text,
        "parse_mode": parse_mode,
    })


async def get_telegram_updates(offset: int = 0, limit: int = 10, timeout: int = 0) -> list[dict]:
    """Get new updates from Telegram (long polling)."""
    result = await telegram_api("getUpdates", {
        "offset": offset,
        "limit": limit,
        "timeout": timeout,
    })
    if result.get("ok"):
        return result.get("result", [])
    return []


async def get_bot_info() -> dict:
    """Get bot identity info."""
    result = await telegram_api("getMe")
    return result.get("result", {})


async def check_kria_api() -> bool:
    """Check whether KRIA's local HTTP bridge is reachable."""
    try:
        async with httpx.AsyncClient(timeout=10) as client:
            resp = await client.get(f"{KRIA_API_URL}/api/health")
            return resp.status_code == 200
    except Exception:
        return False


def is_get_updates_conflict(description: str) -> bool:
    normalized = description.strip().lower()
    return (
        "conflict" in normalized
        and "getupdates" in normalized
        and "only one bot instance" in normalized
    )


def conflict_backoff_seconds(retry_count: int) -> float:
    exponent = max(0, min(retry_count - 1, 6))
    base = min(
        TELEGRAM_CONFLICT_BASE_BACKOFF_SECS * (2 ** exponent),
        TELEGRAM_CONFLICT_MAX_BACKOFF_SECS,
    )
    jitter = (int(time.time() * 1000) % TELEGRAM_CONFLICT_JITTER_MAX_MS) / 1000.0
    return float(base) + jitter


def _is_allowed_chat(chat_id: int) -> bool:
    """Check if a chat ID is in the allowed list. Empty list = allow all."""
    if not ALLOWED_CHAT_IDS:
        return True
    return chat_id in ALLOWED_CHAT_IDS


# ── MCP Tool definitions ───────────────────────────────────────

MCP_TOOLS = [
    {
        "name": "send_message",
        "description": "Send a message to a Telegram chat. Use this to reply to the user on Telegram.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "chat_id": {
                    "type": "integer",
                    "description": "Telegram chat ID to send the message to",
                },
                "text": {
                    "type": "string",
                    "description": "Message text to send (supports Markdown)",
                },
            },
            "required": ["chat_id", "text"],
        },
    },
    {
        "name": "get_updates",
        "description": "Get recent unread messages from Telegram.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of updates to retrieve (default: 10)",
                },
            },
        },
    },
    {
        "name": "get_bot_info",
        "description": "Get information about the connected Telegram bot.",
        "inputSchema": {
            "type": "object",
            "properties": {},
        },
    },
    {
        "name": "send_photo",
        "description": "Send a photo to a Telegram chat by URL or file path.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "chat_id": {
                    "type": "integer",
                    "description": "Telegram chat ID",
                },
                "photo_url": {
                    "type": "string",
                    "description": "URL of the photo to send",
                },
                "caption": {
                    "type": "string",
                    "description": "Photo caption text",
                },
            },
            "required": ["chat_id", "photo_url"],
        },
    },
    {
        "name": "list_allowed_chats",
        "description": "List the currently allowed Telegram chat IDs.",
        "inputSchema": {
            "type": "object",
            "properties": {},
        },
    },
]


# ── MCP Tool handlers ──────────────────────────────────────────

async def handle_tool_call(name: str, arguments: dict | None) -> dict:
    """Execute an MCP tool call and return the result."""
    args = arguments or {}

    try:
        if name == "send_message":
            chat_id = args.get("chat_id")
            text = args.get("text", "")
            if not chat_id:
                return _tool_error("chat_id is required")
            if not text:
                return _tool_error("text is required")
            result = await send_telegram_message(int(chat_id), text)
            if result.get("ok"):
                return _tool_result(f"Message sent to chat {chat_id}")
            else:
                return _tool_error(result.get("description", "Failed to send"))

        elif name == "get_updates":
            limit = args.get("limit", 10)
            updates = await get_telegram_updates(offset=_last_update_id + 1, limit=limit)
            messages = []
            for u in updates:
                msg = u.get("message", {})
                if msg:
                    chat = msg.get("chat", {})
                    messages.append({
                        "update_id": u["update_id"],
                        "chat_id": chat.get("id"),
                        "from": msg.get("from", {}).get("first_name", "Unknown"),
                        "text": msg.get("text", ""),
                        "date": msg.get("date"),
                    })
            return _tool_result(json.dumps(messages, indent=2))

        elif name == "get_bot_info":
            info = await get_bot_info()
            return _tool_result(json.dumps(info, indent=2))

        elif name == "send_photo":
            chat_id = args.get("chat_id")
            photo_url = args.get("photo_url", "")
            caption = args.get("caption", "")
            if not chat_id or not photo_url:
                return _tool_error("chat_id and photo_url are required")
            if not _is_allowed_chat(int(chat_id)):
                return _tool_error(f"Chat {chat_id} not in allowed list")
            result = await telegram_api("sendPhoto", {
                "chat_id": int(chat_id),
                "photo": photo_url,
                "caption": caption,
            })
            if result.get("ok"):
                return _tool_result(f"Photo sent to chat {chat_id}")
            else:
                return _tool_error(result.get("description", "Failed to send photo"))

        elif name == "list_allowed_chats":
            chats = list(ALLOWED_CHAT_IDS) if ALLOWED_CHAT_IDS else ["all (no restriction)"]
            return _tool_result(json.dumps(chats))

        else:
            return _tool_error(f"Unknown tool: {name}")

    except Exception as e:
        logger.exception("Tool call failed: %s", name)
        return _tool_error(str(e))


def _tool_result(text: str) -> dict:
    return {"content": [{"type": "text", "text": text}], "isError": False}


def _tool_error(text: str) -> dict:
    return {"content": [{"type": "text", "text": f"Error: {text}"}], "isError": True}


# ── Background: Forward Telegram messages to KRIA ──────────────

async def telegram_polling_loop():
    """Long-poll Telegram for new messages and forward to KRIA's chat API."""
    global _last_update_id
    conflict_retries = 0

    logger.info("Starting Telegram polling loop (KRIA API: %s)", KRIA_API_URL)

    while True:
        try:
            updates = await get_telegram_updates(
                offset=_last_update_id + 1,
                limit=10,
                timeout=30,
            )
            conflict_retries = 0

            for update in updates:
                _last_update_id = update["update_id"]
                message = update.get("message", {})
                if not message:
                    continue

                chat = message.get("chat", {})
                chat_id = chat.get("id")
                text = message.get("text", "")
                from_user = message.get("from", {}).get("first_name", "User")

                if not text or not chat_id:
                    continue

                if not _is_allowed_chat(chat_id):
                    logger.warning("Ignoring message from unauthorized chat %s", chat_id)
                    continue

                logger.info("Telegram message from %s (chat %s): %s", from_user, chat_id, text[:100])

                # Forward to KRIA server
                try:
                    async with httpx.AsyncClient(timeout=120) as client:
                        resp = await client.post(
                            f"{KRIA_API_URL}/api/chat",
                            json={
                                "message": text,
                                "source": "telegram",
                                "chat_id": chat_id,
                                "from_user": from_user,
                            },
                        )
                        if resp.status_code == 200:
                            reply = resp.json().get("reply", "I processed your request.")
                            await send_telegram_message(chat_id, reply)
                        else:
                            logger.error("KRIA API returned %s: %s", resp.status_code, resp.text[:200])
                            await send_telegram_message(
                                chat_id, "⚠️ Sorry, I couldn't process that right now."
                            )
                except Exception as e:
                    logger.error("Failed to forward to KRIA: %s", e)
                    try:
                        await send_telegram_message(
                            chat_id, "⚠️ Connection to KRIA failed. Is the server running?"
                        )
                    except Exception:
                        pass

        except httpx.TimeoutException:
            # Normal for long polling
            continue
        except httpx.HTTPStatusError as e:
            description = ""
            try:
                body = e.response.json()
                description = body.get("description", "")
            except Exception:
                description = e.response.text

            if is_get_updates_conflict(description):
                conflict_retries += 1
                delay = conflict_backoff_seconds(conflict_retries)
                logger.warning(
                    "Telegram getUpdates conflict: %s. Retrying in %.1fs.",
                    description,
                    delay,
                )
                await asyncio.sleep(delay)
                continue

            logger.error("Polling HTTP error: %s", description or str(e))
            await asyncio.sleep(5)
        except Exception as e:
            logger.error("Polling error: %s", e)
            await asyncio.sleep(5)


# ── MCP Protocol: stdio JSON-RPC handler ───────────────────────

async def handle_jsonrpc(request: dict) -> dict | None:
    """Handle a single JSON-RPC request."""
    method = request.get("method", "")
    req_id = request.get("id")
    params = request.get("params")

    # Notifications (no id) — just acknowledge
    if method == "notifications/initialized":
        logger.info("MCP client initialized")
        return None  # No response for notifications

    if method == "initialize":
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {},
                },
                "serverInfo": {
                    "name": "kria-telegram",
                    "version": "0.1.0",
                },
            },
        }

    if method == "tools/list":
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "tools": MCP_TOOLS,
            },
        }

    if method == "tools/call":
        tool_name = params.get("name", "") if params else ""
        arguments = params.get("arguments") if params else None
        result = await handle_tool_call(tool_name, arguments)
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": result,
        }

    # Unknown method
    return {
        "jsonrpc": "2.0",
        "id": req_id,
        "error": {
            "code": -32601,
            "message": f"Method not found: {method}",
        },
    }


async def stdio_loop():
    """Read JSON-RPC requests from stdin, write responses to stdout."""
    logger.info("Telegram MCP server starting on stdio")

    loop = asyncio.get_event_loop()
    reader = asyncio.StreamReader()
    protocol = asyncio.StreamReaderProtocol(reader)
    await loop.connect_read_pipe(lambda: protocol, sys.stdin)

    # We need raw stdout for writing (not logging)
    stdout_transport, _ = await loop.connect_write_pipe(
        asyncio.streams.FlowControlMixin, sys.stdout
    )
    writer = asyncio.StreamWriter(stdout_transport, protocol, reader, loop)

    while True:
        line = await reader.readline()
        if not line:
            logger.info("stdin closed, shutting down")
            break

        line_str = line.decode("utf-8").strip()
        if not line_str:
            continue

        try:
            request = json.loads(line_str)
        except json.JSONDecodeError as e:
            logger.warning("Invalid JSON: %s", e)
            continue

        response = await handle_jsonrpc(request)
        if response is not None:
            response_line = json.dumps(response) + "\n"
            writer.write(response_line.encode("utf-8"))
            await writer.drain()


async def main():
    """Entry point — start MCP stdio server + Telegram polling loop."""
    global BOT_TOKEN, ALLOWED_CHAT_IDS, KRIA_API_URL

    BOT_TOKEN = os.environ.get("TELEGRAM_BOT_TOKEN", "")
    KRIA_API_URL = os.environ.get("KRIA_API_URL", "http://127.0.0.1:3001")

    chat_ids_str = os.environ.get("TELEGRAM_CHAT_IDS", "")
    if chat_ids_str:
        ALLOWED_CHAT_IDS = {int(cid.strip()) for cid in chat_ids_str.split(",") if cid.strip()}

    if not BOT_TOKEN:
        logger.warning("TELEGRAM_BOT_TOKEN not set — tools will fail, polling disabled")
        # Still start MCP server so it can report the error via tools
        await stdio_loop()
        return

    # Verify the bot token works
    try:
        info = await get_bot_info()
        logger.info("Connected as @%s (id: %s)", info.get("username"), info.get("id"))
    except Exception as e:
        logger.error("Failed to verify bot token: %s", e)

    if await check_kria_api():
        logger.info("KRIA API bridge reachable at %s", KRIA_API_URL)
    else:
        logger.warning(
            "KRIA API bridge is not reachable at %s. Incoming Telegram messages will fail until KRIA starts its local API bridge.",
            KRIA_API_URL,
        )

    # Run MCP stdio handler + Telegram polling concurrently
    await asyncio.gather(
        stdio_loop(),
        telegram_polling_loop(),
    )


def entry():
    """Sync entry point for console_scripts."""
    asyncio.run(main())


if __name__ == "__main__":
    entry()
