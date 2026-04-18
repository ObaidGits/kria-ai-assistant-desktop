"""
K.R.I.A. Sidecar Bridge — JSON-RPC 2.0 over stdio dispatcher.

Rust spawns this process and communicates via stdin/stdout.
Stderr is reserved for logging only (never mixed with RPC responses).

Protocol:
    - Each request is a single JSON line on stdin
    - Each response is a single JSON line on stdout
    - Methods are routed to processor modules
"""

import json
import sys
import time
import os
import traceback
import logging
from typing import Any

# Configure logging to stderr only
logging.basicConfig(
    stream=sys.stderr,
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
    datefmt="%H:%M:%S",
)
logger = logging.getLogger("kria.bridge")

# Global state
_start_time = time.monotonic()
_tier = os.environ.get("KRIA_TIER", "standard")
_request_count = 0
MAX_REQUEST_SIZE_BYTES = int(os.environ.get("KRIA_MAX_REQUEST_MB", "50")) * 1024 * 1024

# ── Processor registry ──────────────────────────────────────────

_processors: dict[str, Any] = {}


def _json_safe(value: Any) -> Any:
    """Convert non-JSON-native types (e.g. numpy scalars) into serializable values."""
    if value is None or isinstance(value, (str, int, float, bool)):
        return value

    if isinstance(value, dict):
        return {str(k): _json_safe(v) for k, v in value.items()}

    if isinstance(value, (list, tuple, set)):
        return [_json_safe(v) for v in value]

    # numpy scalar compatibility without importing numpy directly.
    if hasattr(value, "item"):
        try:
            return _json_safe(value.item())
        except Exception:
            pass

    if isinstance(value, (bytes, bytearray)):
        return value.decode("utf-8", errors="replace")

    return str(value)


def _validate_params(method: str, params: dict[str, Any]) -> str | None:
    """Validate method-specific params for critical paths before dispatch."""
    if method == "image.analyze":
        file_path = params.get("file_path") or params.get("file")
        if not isinstance(file_path, str) or not file_path.strip():
            return "image.analyze requires params.file_path or params.file as a non-empty string"
        operations = params.get("operations")
        if operations is not None and not isinstance(operations, list):
            return "image.analyze params.operations must be a list when provided"
    return None


def _load_processors() -> None:
    """Lazily import processor modules to avoid startup cost for unused ones."""
    global _processors

    try:
        from kria_modules.processors import image as img_proc
        _processors["image"] = img_proc
        logger.info("Loaded image processor")
    except ImportError as e:
        logger.warning("Image processor unavailable: %s", e)

    try:
        from kria_modules.processors import document as doc_proc
        _processors["document"] = doc_proc
        logger.info("Loaded document processor")
    except ImportError as e:
        logger.warning("Document processor unavailable: %s", e)

    try:
        from kria_modules.processors import code as code_proc
        _processors["code"] = code_proc
        logger.info("Loaded code processor")
    except ImportError as e:
        logger.warning("Code processor unavailable: %s", e)

    try:
        from kria_modules.processors import web as web_proc
        _processors["web"] = web_proc
        logger.info("Loaded web processor")
    except ImportError as e:
        logger.warning("Web processor unavailable: %s", e)

    try:
        from kria_modules.processors import embeddings as emb_proc
        _processors["embeddings"] = emb_proc
        logger.info("Loaded embeddings processor")
    except ImportError as e:
        logger.warning("Embeddings processor unavailable: %s", e)

    try:
        from kria_modules.processors import audio as audio_proc
        _processors["audio"] = audio_proc
        logger.info("Loaded audio processor")
    except ImportError as e:
        logger.warning("Audio processor unavailable: %s", e)

    try:
        from kria_modules.processors import news as news_proc
        _processors["news"] = news_proc
        logger.info("Loaded news processor")
    except ImportError as e:
        logger.warning("News processor unavailable: %s", e)

    try:
        from kria_modules.processors import google as google_proc
        _processors["google"] = google_proc
        logger.info("Loaded google processor")
    except ImportError as e:
        logger.warning("Google processor unavailable: %s", e)


# ── JSON-RPC helpers ────────────────────────────────────────────

def _success_response(id: Any, result: Any) -> dict:
    return {"jsonrpc": "2.0", "id": _json_safe(id), "result": _json_safe(result)}


def _error_response(id: Any, code: int, message: str, data: Any = None) -> dict:
    err: dict[str, Any] = {"code": code, "message": message}
    if data is not None:
        err["data"] = _json_safe(data)
    return {"jsonrpc": "2.0", "id": _json_safe(id), "error": err}


# ── Built-in methods ───────────────────────────────────────────

def _handle_ping(params: dict) -> dict:
    memory_mb: float | None = None
    try:
        import psutil  # noqa: lazy import — only used for health

        proc = psutil.Process()
        memory_mb = round(proc.memory_info().rss / (1024 * 1024), 1)
    except Exception:
        pass

    return {
        "status": "pong",
        "uptime_secs": round(time.monotonic() - _start_time, 2),
        "memory_mb": memory_mb,
        "request_count": _request_count,
    }


def _handle_health_check(params: dict) -> dict:
    caps = list(_processors.keys())
    return {
        "status": "ready",
        "tier": _tier,
        "capabilities": caps,
        "uptime_secs": round(time.monotonic() - _start_time, 2),
    }


def _handle_configure_tier(params: dict) -> dict:
    global _tier
    new_tier = params.get("tier", "standard")
    if new_tier not in ("lite", "standard", "performance", "high"):
        return {"error": f"unknown tier: {new_tier}"}
    old = _tier
    _tier = new_tier
    logger.info("Tier changed: %s → %s", old, _tier)
    return {"status": "ok", "tier": _tier}


def _handle_list_capabilities(params: dict) -> dict:
    result = {}
    for name, mod in _processors.items():
        methods = []
        if hasattr(mod, "METHODS"):
            methods = mod.METHODS
        result[name] = {"loaded": True, "methods": methods}
    return {"capabilities": result, "tier": _tier}


def _handle_shutdown(params: dict) -> dict:
    logger.info("Shutdown requested")
    return {"status": "shutting_down"}


# Built-in method dispatch table
_builtins: dict[str, Any] = {
    "ping": _handle_ping,
    "health_check": _handle_health_check,
    "configure_tier": _handle_configure_tier,
    "list_capabilities": _handle_list_capabilities,
    "shutdown": _handle_shutdown,
}


# ── Main dispatch ──────────────────────────────────────────────

def _dispatch(method: str, params: dict) -> Any:
    """Route a method call to the appropriate handler."""
    # Built-in methods
    if method in _builtins:
        return _builtins[method](params)

    # Processor methods: "module.method_name"
    parts = method.split(".", 1)
    if len(parts) != 2:
        raise ValueError(f"Invalid method format: {method}. Expected 'module.method'")

    module_name, func_name = parts

    if module_name not in _processors:
        raise ValueError(f"Unknown processor: {module_name}. Available: {list(_processors.keys())}")

    processor = _processors[module_name]
    handler = getattr(processor, func_name, None)
    if handler is None:
        raise ValueError(f"Unknown method '{func_name}' on processor '{module_name}'")

    # Inject tier into params so processors can adapt
    dispatch_params = dict(params)
    dispatch_params["_tier"] = _tier
    return handler(dispatch_params)


def _process_request(line: str) -> str | None:
    """Parse a JSON-RPC request and return a JSON-RPC response string."""
    global _request_count

    if len(line.encode("utf-8")) > MAX_REQUEST_SIZE_BYTES:
        return json.dumps(
            _error_response(
                None,
                -32600,
                f"Request too large (max {MAX_REQUEST_SIZE_BYTES} bytes)",
            )
        )

    try:
        req = json.loads(line)
    except json.JSONDecodeError as e:
        return json.dumps(_error_response(None, -32700, f"Parse error: {e}"))

    if not isinstance(req, dict):
        return json.dumps(_error_response(None, -32600, "Invalid request object"))

    req_id = req.get("id")
    method = req.get("method")
    params = req.get("params", {})

    if not isinstance(method, str) or not method:
        return json.dumps(_error_response(req_id, -32600, "Missing 'method' field"))

    if params is None:
        params = {}
    if not isinstance(params, dict):
        return json.dumps(_error_response(req_id, -32602, "Invalid params: expected JSON object"))

    validation_error = _validate_params(method, params)
    if validation_error:
        return json.dumps(_error_response(req_id, -32602, validation_error))

    _request_count += 1
    logger.info("Request #%d: %s", _request_count, method)

    try:
        result = _dispatch(method, params)
        return json.dumps(_success_response(req_id, result))
    except ValueError as e:
        return json.dumps(_error_response(req_id, -32601, str(e)))
    except FileNotFoundError as e:
        return json.dumps(_error_response(req_id, -32602, f"File not found: {e}"))
    except Exception as e:
        logger.error("Unhandled error in %s: %s\n%s", method, e, traceback.format_exc())
        return json.dumps(_error_response(req_id, -32603, f"Internal error: {type(e).__name__}: {e}"))


# ── Self-test mode ─────────────────────────────────────────────

def _selftest() -> int:
    """Run a basic self-test of all available processors."""
    _load_processors()

    print(f"KRIA Sidecar Self-Test")
    print(f"  Python: {sys.version}")
    print(f"  Tier:   {_tier}")
    print(f"  Processors loaded: {list(_processors.keys())}")

    # Test built-in methods
    for method in ("ping", "health_check", "list_capabilities"):
        try:
            result = _dispatch(method, {})
            print(f"  ✓ {method}: OK")
        except Exception as e:
            print(f"  ✗ {method}: {e}")
            return 1

    print(f"\nAll checks passed. {len(_processors)} processor(s) available.")
    return 0


# ── Entry point ────────────────────────────────────────────────

def main() -> None:
    if "--selftest" in sys.argv:
        sys.exit(_selftest())

    logger.info("KRIA sidecar starting (tier=%s, pid=%d)", _tier, os.getpid())
    _load_processors()
    logger.info("Ready — %d processor(s) loaded", len(_processors))

    # Send a ready signal so Rust knows we're alive
    ready = json.dumps({"jsonrpc": "2.0", "method": "ready", "params": {"pid": os.getpid()}})
    sys.stdout.write(ready + "\n")
    sys.stdout.flush()

    # Main loop: read JSON-RPC requests from stdin, write responses to stdout
    try:
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue

            response = _process_request(line)
            if response:
                sys.stdout.write(response + "\n")
                sys.stdout.flush()

            # Check for shutdown
            try:
                req = json.loads(line)
                if req.get("method") == "shutdown":
                    break
            except Exception:
                pass

    except KeyboardInterrupt:
        logger.info("Interrupted")
    except BrokenPipeError:
        logger.info("Pipe closed (Rust process exited)")

    logger.info("Sidecar exiting")


if __name__ == "__main__":
    main()
