# K.R.I.A. — Implementation Guide

## Complete AI Assistant — Phased Feature Implementation Plan

| Field | Detail |
|---|---|
| **Project** | K.R.I.A. (Kernel-Responsive Intelligent Agent) |
| **Developer** | Obaidullah Zeeshan |
| **Document Version** | 3.0.0 |
| **Date** | April 2026 |
| **Companion Docs** | SYSTEM_DESIGN_DOCUMENT.md · SAFETY_SPECIFICATION.md · PROJECT_STRUCTURE.md · SPEECH_RECOGNITION.md · QUERIES.md |

---

## Table of Contents

1. [Guiding Principles](#1-guiding-principles)
2. [Architecture Patterns for Resilience](#2-architecture-patterns-for-resilience)
3. [Phase 0 — Project Skeleton & Infrastructure](#3-phase-0--project-skeleton--infrastructure)
4. [Phase 1 — Infrastructure Layer](#4-phase-1--infrastructure-layer)
5. [Phase 2 — The Reasoning Brain](#5-phase-2--the-reasoning-brain)
6. [Phase 3 — Safety & Guardrail System](#6-phase-3--safety--guardrail-system)
7. [Phase 4 — Core Tool System](#7-phase-4--core-tool-system)
8. [Phase 5 — Internet & Connectivity Tools](#8-phase-5--internet--connectivity-tools)
9. [Phase 6 — Advanced File & Document Intelligence](#9-phase-6--advanced-file--document-intelligence)
10. [Phase 7 — OS-Level Task Management Tools](#10-phase-7--os-level-task-management-tools)
11. [Phase 8 — Application Lifecycle Management](#11-phase-8--application-lifecycle-management)
12. [Phase 9 — Notification & Communication Hub](#12-phase-9--notification--communication-hub)
13. [Phase 10 — Knowledge Base & Learning System](#13-phase-10--knowledge-base--learning-system)
14. [Phase 11 — Automation & Workflow Engine](#14-phase-11--automation--workflow-engine)
15. [Phase 12 — Plugin Architecture](#15-phase-12--plugin-architecture)
16. [Phase 13 — Sensory Pipeline & STT Enhancement](#16-phase-13--sensory-pipeline-voice)
17. [Phase 14 — Memory & Context](#17-phase-14--memory--context)
18. [Phase 15 — Web Dashboard](#18-phase-15--web-dashboard)
19. [Phase 16 — Docker Deployment & GPU](#19-phase-16--docker-deployment--gpu)
20. [Phase 17 — Integration, Testing & Hardening](#20-phase-17--integration-testing--hardening)
21. [Phase 18 — Post-Launch Roadmap](#21-phase-18--post-launch-roadmap)
22. [Phase 19 — Dynamic Model Routing & Cascading Inference](#22-phase-19--dynamic-model-routing--cascading-inference)
23. [Phase 20 — Multi-Language Voice Support](#23-phase-20--multi-language-voice-support)
24. [Phase 21 — Screen Vision Module](#24-phase-21--screen-vision-module)
25. [Phase 22 — Extended Interface Layer](#25-phase-22--extended-interface-layer)
26. [Phase 23 — Context Awareness & Proactive Intelligence](#26-phase-23--context-awareness--proactive-intelligence)
27. [Phase 24 — Performance Benchmarking & Safety Demo Mode](#27-phase-24--performance-benchmarking--safety-demo-mode)
28. [Dependency Graph](#28-dependency-graph)
29. [Risk Register](#29-risk-register)

---

## 1. Guiding Principles

Every implementation decision must align with these non-negotiable principles:

| Principle | Rule |
|---|---|
| **Fault Isolation** | A crash in any single function, tool, or service must NEVER propagate to other services. Every call boundary is a blast radius wall. |
| **Loose Coupling** | Services communicate only through well-defined interfaces (HTTP APIs, Redis pub/sub, WebSocket messages). No shared in-process state between modules. |
| **Graceful Degradation** | If STT fails → fall back to text input. If TTS fails → return text response. If Redis is down → use in-memory fallback. If ChromaDB is down → skip RAG, use conversation buffer only. If internet is down → use local-only tools. |
| **Fail-Safe Defaults** | Unknown actions default to RED (blocked). Unknown paths default to protected. Unknown errors default to safe abort + audit log. |
| **Scalable from Day 1** | Interface-driven design. Every component has an abstract base → concrete implementation. Swap any model, database, or service without touching callers. |
| **Internet is a Tool, Not a Dependency** | Core assistant works fully offline. Internet enhances capabilities but never blocks local function. |
| **Privacy by Design** | No telemetry, no cloud calls for core function. Internet requests are transparent and logged. |

---

## 2. Architecture Patterns for Resilience

These patterns are used **everywhere** across the codebase. Implement them as shared utilities in `src/kria/infra/` before building any feature.

### 2.1 Circuit Breaker

Prevents cascading failures when a downstream service (llama.cpp, whisper.cpp, Redis, external APIs) becomes unhealthy.

```
src/kria/infra/circuit_breaker.py
```

```python
import asyncio
import time
from enum import Enum
from typing import Callable, TypeVar, Any
from functools import wraps

T = TypeVar("T")

class CircuitState(Enum):
    CLOSED = "closed"       # Normal operation
    OPEN = "open"           # Failing — reject calls immediately
    HALF_OPEN = "half_open" # Testing recovery — allow one probe call

class CircuitBreaker:
    """
    Wrap any async callable with automatic failure detection.
    After `failure_threshold` consecutive failures, the circuit opens
    and all calls return the fallback value for `recovery_timeout` seconds
    before probing again.
    """

    def __init__(
        self,
        name: str,
        failure_threshold: int = 3,
        recovery_timeout: float = 30.0,
        fallback: Any = None,
    ):
        self.name = name
        self.failure_threshold = failure_threshold
        self.recovery_timeout = recovery_timeout
        self.fallback = fallback
        self._state = CircuitState.CLOSED
        self._failure_count = 0
        self._last_failure_time = 0.0
        self._lock = asyncio.Lock()

    @property
    def state(self) -> CircuitState:
        if self._state == CircuitState.OPEN:
            if time.monotonic() - self._last_failure_time >= self.recovery_timeout:
                self._state = CircuitState.HALF_OPEN
        return self._state

    async def call(self, func: Callable[..., Any], *args, **kwargs) -> Any:
        async with self._lock:
            current_state = self.state

        if current_state == CircuitState.OPEN:
            return self.fallback

        try:
            result = await func(*args, **kwargs)
            async with self._lock:
                self._failure_count = 0
                self._state = CircuitState.CLOSED
            return result
        except Exception:
            async with self._lock:
                self._failure_count += 1
                self._last_failure_time = time.monotonic()
                if self._failure_count >= self.failure_threshold:
                    self._state = CircuitState.OPEN
            return self.fallback
```

### 2.2 Supervised Task Runner

Every async background task (wake word listener, file watcher, scheduler, RSS poller) runs under a supervisor that restarts it on crash.

```
src/kria/infra/supervisor.py
```

```python
import asyncio
import logging

logger = logging.getLogger("kria.supervisor")

class SupervisedTask:
    """
    Runs an async coroutine in a loop. If it crashes, logs the error
    and restarts after a back-off delay. Max retries prevents infinite loops.
    """

    def __init__(
        self,
        name: str,
        coro_factory,            # Callable that returns a coroutine
        max_retries: int = 10,
        base_delay: float = 1.0,
        max_delay: float = 60.0,
    ):
        self.name = name
        self.coro_factory = coro_factory
        self.max_retries = max_retries
        self.base_delay = base_delay
        self.max_delay = max_delay
        self._task: asyncio.Task | None = None
        self._retries = 0

    async def _run_loop(self):
        while self._retries < self.max_retries:
            try:
                logger.info(f"[{self.name}] Starting (attempt {self._retries + 1})")
                await self.coro_factory()
                break  # Clean exit
            except asyncio.CancelledError:
                logger.info(f"[{self.name}] Cancelled")
                break
            except Exception as e:
                self._retries += 1
                delay = min(self.base_delay * (2 ** self._retries), self.max_delay)
                logger.error(f"[{self.name}] Crashed: {e}. Restarting in {delay:.1f}s")
                await asyncio.sleep(delay)

        if self._retries >= self.max_retries:
            logger.critical(f"[{self.name}] Exceeded max retries. Giving up.")

    def start(self) -> asyncio.Task:
        self._task = asyncio.create_task(self._run_loop())
        return self._task

    async def stop(self):
        if self._task and not self._task.done():
            self._task.cancel()
            try:
                await self._task
            except asyncio.CancelledError:
                pass
```

### 2.3 Service Health Registry

Central registry tracking which services are alive. All modules check this before calling dependencies.

```
src/kria/infra/health.py
```

```python
from enum import Enum
from dataclasses import dataclass, field
import time

class ServiceStatus(Enum):
    HEALTHY = "healthy"
    DEGRADED = "degraded"
    DOWN = "down"
    UNKNOWN = "unknown"

@dataclass
class ServiceHealth:
    name: str
    status: ServiceStatus = ServiceStatus.UNKNOWN
    last_check: float = 0.0
    error: str | None = None

class HealthRegistry:
    """Global health registry. Modules query this to decide fallback behavior."""

    def __init__(self):
        self._services: dict[str, ServiceHealth] = {}

    def register(self, name: str):
        self._services[name] = ServiceHealth(name=name)

    def update(self, name: str, status: ServiceStatus, error: str | None = None):
        if name in self._services:
            self._services[name].status = status
            self._services[name].last_check = time.monotonic()
            self._services[name].error = error

    def is_healthy(self, name: str) -> bool:
        svc = self._services.get(name)
        return svc is not None and svc.status == ServiceStatus.HEALTHY

    def get_all(self) -> dict[str, ServiceHealth]:
        return dict(self._services)

# Singleton instance — imported by all modules
health_registry = HealthRegistry()
```

### 2.4 Isolated Exception Handling Decorator

Every tool function and every service call is wrapped in a try/except that catches, logs, and returns a structured error — never propagating to the caller.

```
src/kria/infra/isolation.py
```

```python
import logging
import traceback
from functools import wraps
from dataclasses import dataclass

logger = logging.getLogger("kria.isolation")

@dataclass
class ToolResult:
    success: bool
    data: any = None
    error: str | None = None

def isolated(func):
    """
    Decorator that guarantees the wrapped async function never raises.
    Returns ToolResult(success=False, error=...) on any exception.
    """
    @wraps(func)
    async def wrapper(*args, **kwargs) -> ToolResult:
        try:
            result = await func(*args, **kwargs)
            return ToolResult(success=True, data=result)
        except Exception as e:
            logger.error(f"[{func.__name__}] Failed: {e}\n{traceback.format_exc()}")
            return ToolResult(success=False, error=str(e))
    return wrapper
```

### 2.5 Platform Detection Utility

Cross-platform tools need to branch by OS. Centralize detection:

```
src/kria/infra/platform_detect.py
```

```python
import platform
import shutil
from enum import Enum

class OSType(Enum):
    LINUX = "linux"
    WINDOWS = "windows"
    MACOS = "macos"
    UNKNOWN = "unknown"

def get_os() -> OSType:
    system = platform.system().lower()
    if system == "linux":
        return OSType.LINUX
    elif system == "windows":
        return OSType.WINDOWS
    elif system == "darwin":
        return OSType.MACOS
    return OSType.UNKNOWN

def has_command(cmd: str) -> bool:
    """Check if a command-line tool is available."""
    return shutil.which(cmd) is not None

def get_package_manager() -> str | None:
    """Detect the system package manager."""
    os_type = get_os()
    if os_type == OSType.LINUX:
        for pm in ["apt", "dnf", "pacman", "zypper", "apk"]:
            if has_command(pm):
                return pm
    elif os_type == OSType.WINDOWS:
        if has_command("winget"):
            return "winget"
        if has_command("choco"):
            return "choco"
    elif os_type == OSType.MACOS:
        if has_command("brew"):
            return "brew"
    return None

OS = get_os()
PACKAGE_MANAGER = get_package_manager()
```

---

## 3. Phase 0 — Project Skeleton & Infrastructure

**Goal:** Establish the complete directory structure, Python project configuration, dependency management, and foundational shared code so all subsequent phases build on solid ground.

### Step 0.1 — Initialize Python Project

Create `pyproject.toml` at repo root:

```toml
[project]
name = "kria"
version = "2.0.0"
description = "Kernel-Responsive Intelligent Agent — Complete AI Assistant"
requires-python = ">=3.12"
dependencies = [
    "fastapi>=0.115.0",
    "uvicorn[standard]>=0.30.0",
    "httpx>=0.27.0",
    "redis[hiredis]>=5.0.0",
    "chromadb>=0.5.0",
    "pydantic>=2.9.0",
    "pydantic-settings>=2.5.0",
    "psutil>=6.0.0",
    "aiosqlite>=0.20.0",
    "websockets>=13.0",
    "trafilatura>=1.12.0",
    "PyMuPDF>=1.24.0",
    "python-docx>=1.1.0",
    "openpyxl>=3.1.0",
    "pandas>=2.2.0",
    "watchdog>=4.0.0",
    "apscheduler>=4.0.0",
    "plyer>=2.1.0",
    "pyyaml>=6.0",
    "feedparser>=6.0",
    "Pillow>=11.0.0",
]

[project.optional-dependencies]
dev = [
    "pytest>=8.0",
    "pytest-asyncio>=0.24",
    "pytest-cov>=5.0",
    "ruff>=0.6.0",
    "mypy>=1.11",
]
voice = [
    "openwakeword>=0.6.0",
    "onnxruntime>=1.19.0",
    "sounddevice>=0.5.0",
    "numpy>=2.0",
]
ocr = [
    "pytesseract>=0.3.10",
]
windows = [
    "pywin32>=306",
    "pycaw>=20240210",
    "comtypes>=1.4.0",
    "win10toast>=0.9",
]

[tool.ruff]
target-version = "py312"
line-length = 100

[tool.pytest.ini_options]
asyncio_mode = "auto"
testpaths = ["src/tests"]
```

### Step 0.2 — Create Full Directory Tree

Create every directory and `__init__.py` per `PROJECT_STRUCTURE.md`:

```
src/kria/__init__.py
src/kria/main.py
src/kria/agent/__init__.py
src/kria/agent/loop.py
src/kria/agent/llm_client.py
src/kria/agent/router.py
src/kria/agent/planner.py
src/kria/agent/prompts.py
src/kria/tools/__init__.py
src/kria/tools/registry.py
src/kria/tools/app_control.py
src/kria/tools/app_lifecycle.py
src/kria/tools/file_ops.py
src/kria/tools/document_parser.py
src/kria/tools/document_convert.py
src/kria/tools/file_organizer.py
src/kria/tools/file_watcher.py
src/kria/tools/system_info.py
src/kria/tools/system_config.py
src/kria/tools/service_mgmt.py
src/kria/tools/disk_mgmt.py
src/kria/tools/network_mgmt.py
src/kria/tools/power_mgmt.py
src/kria/tools/env_mgmt.py
src/kria/tools/task_scheduler.py
src/kria/tools/process_mgmt.py
src/kria/tools/code_executor.py
src/kria/tools/web_tools.py
src/kria/tools/download_mgr.py
src/kria/tools/rss_reader.py
src/kria/tools/api_tools.py
src/kria/tools/notification.py
src/kria/tools/email_composer.py
src/kria/tools/clipboard_mgr.py
src/kria/tools/reminder.py
src/kria/tools/knowledge_tools.py
src/kria/tools/doc_ingest.py
src/kria/tools/snippet_lib.py
src/kria/voice/__init__.py
src/kria/voice/pipeline.py
src/kria/voice/wake_word.py
src/kria/voice/vad.py
src/kria/voice/stt_client.py
src/kria/voice/tts_client.py
src/kria/safety/__init__.py
src/kria/safety/policy_engine.py
src/kria/safety/hitl.py
src/kria/safety/rollback.py
src/kria/safety/audit.py
src/kria/memory/__init__.py
src/kria/memory/conversation.py
src/kria/memory/persistent.py
src/kria/memory/semantic.py
src/kria/memory/context_manager.py
src/kria/memory/user_prefs.py
src/kria/automation/__init__.py
src/kria/automation/scheduler.py
src/kria/automation/workflow_engine.py
src/kria/automation/event_bus.py
src/kria/automation/macro_recorder.py
src/kria/plugins/__init__.py
src/kria/plugins/manager.py
src/kria/plugins/api.py
src/kria/plugins/loader.py
src/kria/api/__init__.py
src/kria/api/routes.py
src/kria/api/websocket.py
src/kria/infra/__init__.py
src/kria/infra/config.py
src/kria/infra/vram_orchestrator.py
src/kria/infra/redis_bus.py
src/kria/infra/circuit_breaker.py
src/kria/infra/supervisor.py
src/kria/infra/health.py
src/kria/infra/isolation.py
src/kria/infra/logging_config.py
src/kria/infra/platform_detect.py
src/tests/__init__.py
```

### Step 0.3 — Centralized Configuration

```
src/kria/infra/config.py
```

Use `pydantic-settings` for type-safe, environment-variable-driven config:

```python
from pydantic_settings import BaseSettings
from pydantic import Field

class KriaSettings(BaseSettings):
    model_config = {"env_prefix": "KRIA_"}

    # Service URLs
    llama_api_url: str = "http://localhost:8080"
    whisper_api_url: str = "http://localhost:8081"
    piper_api_url: str = "http://localhost:8082"
    redis_url: str = "redis://localhost:6379/0"
    chroma_url: str = "http://localhost:8083"
    bridge_url: str = "http://localhost:9000"

    # Paths
    sqlite_path: str = "./data/kria.db"
    rollback_dir: str = "~/.kria/rollback"
    audit_log_path: str = "./data/audit.db"
    plugins_dir: str = "~/.kria/plugins"
    workflows_dir: str = "~/.kria/workflows"
    snippets_dir: str = "~/.kria/snippets"
    downloads_dir: str = "~/Downloads/kria"
    knowledge_dir: str = "~/.kria/knowledge"

    # Limits
    max_context_turns: int = 20
    tool_timeout_seconds: float = 30.0
    hitl_timeout_seconds: float = 30.0
    rollback_retention_hours: int = 72
    rollback_max_size_gb: float = 5.0

    # Internet
    internet_enabled: bool = True
    internet_https_only: bool = True
    internet_max_response_mb: int = 50
    internet_rate_limit_per_min: int = 60
    internet_search_cache_ttl: int = 3600
    internet_page_cache_ttl: int = 86400
    max_download_size_mb: int = 500

    # Safety
    default_risk_level: str = "RED"    # Unknown actions default to RED
    emergency_mode: bool = False        # Only GREEN actions when True

    # Performance
    redis_cache_ttl_seconds: int = 60
    max_concurrent_tools: int = 3

    # Voice
    voice_enabled: bool = False
    wake_word_threshold: float = 0.5

    # Automation
    automation_enabled: bool = True
    max_scheduled_tasks: int = 50
    max_workflows: int = 20

    # Plugins
    plugins_enabled: bool = True

    # Notifications
    notifications_enabled: bool = True

settings = KriaSettings()
```

### Deliverables — Phase 0

- [ ] `pyproject.toml` with all deps (including document, web, automation libs)
- [ ] Full directory tree with `__init__.py` files (30+ new files)
- [ ] `config.py` with Pydantic settings (internet, automation, plugins sections)
- [ ] `main.py` FastAPI shell with lifespan
- [ ] `circuit_breaker.py`, `supervisor.py`, `health.py`, `isolation.py`, `platform_detect.py`
- [ ] `.env.example` with all new config vars
- [ ] Passes `ruff check` and `mypy` with zero errors

---

## 4. Phase 1 — Infrastructure Layer

**Goal:** Build the communication backbone. All other modules depend on Redis (message bus + cache), SQLite (persistence), and the health system being operational.

*(Implementation identical to v1.0 — see existing code in `redis_bus.py`, `persistent.py`, `logging_config.py`)*

### Deliverables — Phase 1

- [ ] `redis_bus.py` — full pub/sub + cache with in-memory fallback
- [ ] `persistent.py` — SQLite with schema migrations
- [ ] `logging_config.py` — structured logging
- [ ] Health checks registered for `redis` and `sqlite`
- [ ] Unit tests: `tests/test_redis_bus.py`, `tests/test_sqlite.py`

---

## 5. Phase 2 — The Reasoning Brain

**Goal:** Build the LLM client, intent router, ReAct agent loop, and prompt management. This is the core intelligence of K.R.I.A.

*(Implementation identical to v1.0 — see existing code in `llm_client.py`, `router.py`, `loop.py`, `prompts.py`, `planner.py`)*

### Update: System Prompt for Full AI Assistant

The system prompt must be updated to reflect the expanded tool set:

```python
CORE_SYSTEM_PROMPT = """You are K.R.I.A. (Kernel-Responsive Intelligent Agent), a complete AI
Assistant running locally on the user's machine. You can:

- Search the internet, fetch web pages, get weather/news/stocks
- Read, write, organize, and convert documents (PDF, DOCX, XLSX, CSV, etc.)
- Manage the operating system: services, scheduled tasks, environment, disk, network, power
- Install, update, and uninstall applications via system package managers
- Execute code in sandboxed environments (Python, Bash, PowerShell)
- Send desktop notifications, compose emails, manage clipboard
- Remember facts, search knowledge base, manage code snippets
- Create automated workflows and scheduled tasks
- Monitor files for changes and trigger actions

RULES:
1. For each user request, think step-by-step before acting.
2. Call tools with precise parameters. Never guess file paths — use search tools first.
3. If a tool fails, try an alternative approach. Do not retry the same failing call.
4. After completing all actions, give the user a concise summary of what was done.
5. Never fabricate tool outputs. If you don't know, say so.
6. For destructive actions (delete, modify system, execute code), always explain what
   you're about to do and why.
7. If the safety system blocks an action, inform the user and suggest alternatives.
8. For internet queries, prefer using tools (web_search, get_weather) over your training data.
9. When working with files, always confirm paths before destructive operations.
10. Proactively suggest related actions when appropriate.

Current date: {date}
OS: {os_type}
"""
```

### Deliverables — Phase 2

- [ ] `llm_client.py` — streaming + non-streaming, circuit breaker, health check
- [ ] `router.py` — 3-way intent classification
- [ ] `loop.py` — ReAct loop with max iterations, tool call handling, safety gate
- [ ] `prompts.py` — dynamic system prompt with full tool awareness
- [ ] `planner.py` — multi-step plan decomposition
- [ ] Unit tests: mock LLM responses, test loop termination, test router fallback

---

## 6. Phase 3 — Safety & Guardrail System

**Goal:** Implement the 4-tier risk classification, HITL approval, rollback, and audit. Updated with internet-specific safety rules.

*(Core implementation identical to v1.0 — see `policy_engine.py`, `hitl.py`, `rollback.py`, `audit.py`)*

### Update: Internet Safety Rules

Add to `policy_engine.py`:

```python
# Internet-specific safety additions
INTERNET_AUDIT_ACTIONS = frozenset({
    "web_search", "fetch_webpage", "download_file",
    "get_weather", "get_news", "get_stock_price",
    "check_url_status", "rss_feed_read",
})

# These are GREEN but all internet requests are logged
# Download is YELLOW (modifies filesystem)
```

### Deliverables — Phase 3

- [ ] `policy_engine.py` — 4-tier + internet safety + path escalation + emergency mode
- [ ] `hitl.py` — async approval gateway with timeout
- [ ] `rollback.py` — snapshot creation + restore + cleanup
- [ ] `audit.py` — append-only SQLite logging with network_request field
- [ ] Safety wired into agent loop
- [ ] Unit tests for all risk tiers including internet-specific tools

---

## 7. Phase 4 — Core Tool System

**Goal:** Build the tool registry framework and implement basic OS interaction tools (system info, app control, file ops, process management, code executor).

*(Implementation identical to v1.0 — see existing tool files)*

### Deliverables — Phase 4

- [ ] `registry.py` — decorator-based, timeout-wrapped, OpenAI schema export
- [ ] `system_info.py` — CPU, RAM, disk, network, battery, GPU
- [ ] `app_control.py` — open, list, close, kill
- [ ] `file_ops.py` — read, write, search, list, delete, mkdir, copy, move, rename
- [ ] `system_config.py` — volume, brightness, WiFi toggle, power plan
- [ ] `process_mgmt.py` — kill, priority, list services
- [ ] `code_executor.py` — sandboxed Python/Bash/PowerShell
- [ ] Unit tests for each tool module

---

## 8. Phase 5 — Internet & Connectivity Tools

**Goal:** Give K.R.I.A. full internet connectivity for web search, content extraction, downloads, real-time data, and RSS feed consumption.

### Step 5.1 — Enhanced Web Search

```
src/kria/tools/web_tools.py
```

Already partially implemented. Enhance with better result parsing, multiple search engines, and caching:

```python
@tool_registry.register(
    name="web_search",
    description="Search the web for information. Returns titles, URLs, and snippets.",
    risk_level="GREEN",
    parameters={
        "query": {"type": "string", "description": "Search query"},
        "max_results": {"type": "integer", "description": "Max results (1-10)", "default": 5},
    },
)
async def web_search(query: str, max_results: int = 5) -> dict:
    # Check cache first
    cache_key = f"search:{hashlib.md5(query.encode()).hexdigest()}"
    cached = await redis_bus.cache_get(cache_key)
    if cached:
        return cached

    # DuckDuckGo HTML scraping (no API key)
    results = await _search_duckduckgo(query, max_results)

    # Cache for 1 hour
    await redis_bus.cache_set(cache_key, results, ttl=settings.internet_search_cache_ttl)
    return results
```

### Step 5.2 — Content Extraction (fetch_webpage)

Already implemented with trafilatura. Enhance error handling and caching.

### Step 5.3 — Download Manager

```
src/kria/tools/download_mgr.py
```

```python
import asyncio
import httpx
import os
from pathlib import Path
from kria.tools.registry import tool_registry
from kria.infra.config import settings

@tool_registry.register(
    name="download_file",
    description="Download a file from a URL to local disk. Reports progress.",
    risk_level="YELLOW",
    parameters={
        "url": {"type": "string", "description": "URL to download"},
        "filename": {"type": "string", "description": "Save as filename", "default": ""},
        "directory": {"type": "string", "description": "Save directory", "default": ""},
    },
)
async def download_file(url: str, filename: str = "", directory: str = "") -> dict:
    """Download a file with progress tracking and size limits."""
    save_dir = Path(directory or settings.downloads_dir).expanduser()
    save_dir.mkdir(parents=True, exist_ok=True)

    async with httpx.AsyncClient(timeout=120.0, follow_redirects=True) as client:
        # HEAD request for metadata
        head = await client.head(url)
        content_length = int(head.headers.get("content-length", 0))
        content_type = head.headers.get("content-type", "")

        # Size check
        max_bytes = settings.max_download_size_mb * 1024 * 1024
        if content_length > max_bytes:
            return {"error": f"File too large ({content_length} bytes). Max: {max_bytes}"}

        # Determine filename
        if not filename:
            from urllib.parse import urlparse
            filename = Path(urlparse(url).path).name or "download"

        save_path = save_dir / filename

        # Stream download
        downloaded = 0
        async with client.stream("GET", url) as resp:
            resp.raise_for_status()
            with open(save_path, "wb") as f:
                async for chunk in resp.aiter_bytes(chunk_size=65536):
                    f.write(chunk)
                    downloaded += len(chunk)

    return {
        "path": str(save_path),
        "size_bytes": downloaded,
        "content_type": content_type,
        "url": url,
    }
```

### Step 5.4 — RSS Feed Reader

```
src/kria/tools/rss_reader.py
```

```python
import feedparser
import httpx
from kria.tools.registry import tool_registry

@tool_registry.register(
    name="rss_feed_read",
    description="Read an RSS/Atom feed and return recent entries.",
    risk_level="GREEN",
    parameters={
        "url": {"type": "string", "description": "Feed URL"},
        "max_entries": {"type": "integer", "description": "Max entries", "default": 10},
    },
)
async def rss_feed_read(url: str, max_entries: int = 10) -> dict:
    async with httpx.AsyncClient(timeout=15.0) as client:
        resp = await client.get(url)
        resp.raise_for_status()

    feed = feedparser.parse(resp.text)
    entries = []
    for entry in feed.entries[:max_entries]:
        entries.append({
            "title": entry.get("title", ""),
            "link": entry.get("link", ""),
            "published": entry.get("published", ""),
            "summary": entry.get("summary", "")[:500],
        })

    return {
        "feed_title": feed.feed.get("title", ""),
        "entries": entries,
        "count": len(entries),
    }
```

### Step 5.5 — Real-Time Data (Weather, News, etc.)

Already implemented `get_weather` in `web_tools.py`. Add:

```python
@tool_registry.register(
    name="get_news",
    description="Get latest news headlines from RSS feeds.",
    risk_level="GREEN",
    parameters={
        "category": {"type": "string", "description": "News category: general, tech, science, business", "default": "general"},
        "max_items": {"type": "integer", "default": 5},
    },
)
async def get_news(category: str = "general", max_items: int = 5) -> dict:
    # Map categories to well-known RSS feeds
    feeds = {
        "general": "https://feeds.bbci.co.uk/news/rss.xml",
        "tech": "https://feeds.arstechnica.com/arstechnica/index",
        "science": "https://rss.nytimes.com/services/xml/rss/nyt/Science.xml",
        "business": "https://feeds.bbci.co.uk/news/business/rss.xml",
    }
    feed_url = feeds.get(category, feeds["general"])
    return await rss_feed_read(feed_url, max_items)
```

### Step 5.6 — Network Utility Tools

```
src/kria/tools/network_mgmt.py
```

```python
import asyncio
import socket
from kria.tools.registry import tool_registry

@tool_registry.register(
    name="ping_host",
    description="Ping a host and return round-trip time stats.",
    risk_level="GREEN",
    parameters={
        "host": {"type": "string", "description": "Hostname or IP"},
        "count": {"type": "integer", "default": 4},
    },
)
async def ping_host(host: str, count: int = 4) -> dict:
    proc = await asyncio.create_subprocess_exec(
        "ping", "-c", str(count), host,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=30.0)
    return {
        "host": host,
        "output": stdout.decode(errors="replace"),
        "reachable": proc.returncode == 0,
    }

@tool_registry.register(
    name="dns_lookup",
    description="Perform DNS lookup for a hostname.",
    risk_level="GREEN",
    parameters={
        "hostname": {"type": "string", "description": "Domain to look up"},
    },
)
async def dns_lookup(hostname: str) -> dict:
    try:
        results = socket.getaddrinfo(hostname, None)
        ips = list(set(r[4][0] for r in results))
        return {"hostname": hostname, "ips": ips, "count": len(ips)}
    except socket.gaierror as e:
        return {"hostname": hostname, "error": str(e)}

@tool_registry.register(
    name="get_public_ip",
    description="Get public IP address and basic geo info.",
    risk_level="GREEN",
)
async def get_public_ip() -> dict:
    import httpx
    async with httpx.AsyncClient(timeout=10.0) as client:
        resp = await client.get("https://ipinfo.io/json")
        return resp.json()

@tool_registry.register(
    name="check_url_status",
    description="Check if a URL is reachable (HTTP HEAD request).",
    risk_level="GREEN",
    parameters={
        "url": {"type": "string", "description": "URL to check"},
    },
)
async def check_url_status(url: str) -> dict:
    import httpx
    try:
        async with httpx.AsyncClient(timeout=10.0, follow_redirects=True) as client:
            resp = await client.head(url)
            return {
                "url": url,
                "status_code": resp.status_code,
                "reachable": resp.status_code < 400,
                "content_type": resp.headers.get("content-type", ""),
            }
    except Exception as e:
        return {"url": url, "reachable": False, "error": str(e)}
```

### Deliverables — Phase 5

- [ ] Enhanced `web_tools.py` with caching + result ranking
- [ ] `download_mgr.py` — streaming downloads with progress + size limits
- [ ] `rss_reader.py` — RSS/Atom feed parser
- [ ] `network_mgmt.py` — ping, DNS, public IP, URL check
- [ ] `api_tools.py` — generic REST API consumer
- [ ] News headlines via RSS (no API key needed)
- [ ] All internet tools audited and cached via Redis
- [ ] Unit tests with mocked HTTP responses

---

## 9. Phase 6 — Advanced File & Document Intelligence

**Goal:** Go beyond basic file CRUD. Parse documents, extract data, convert formats, organize files intelligently, and watch directories for changes.

### Step 6.1 — Document Parser

```
src/kria/tools/document_parser.py
```

```python
import fitz  # PyMuPDF
from docx import Document as DocxDocument
import openpyxl
import pandas as pd
import csv
from pathlib import Path
from kria.tools.registry import tool_registry

@tool_registry.register(
    name="parse_pdf",
    description="Extract text content from a PDF file.",
    risk_level="GREEN",
    parameters={
        "path": {"type": "string", "description": "Path to PDF file"},
        "max_pages": {"type": "integer", "default": 50},
    },
)
async def parse_pdf(path: str, max_pages: int = 50) -> dict:
    doc = fitz.open(path)
    pages = []
    for i, page in enumerate(doc):
        if i >= max_pages:
            break
        pages.append({
            "page": i + 1,
            "text": page.get_text().strip(),
        })
    return {
        "path": path,
        "total_pages": len(doc),
        "parsed_pages": len(pages),
        "content": pages,
    }

@tool_registry.register(
    name="parse_docx",
    description="Extract text and tables from a DOCX file.",
    risk_level="GREEN",
    parameters={
        "path": {"type": "string", "description": "Path to DOCX file"},
    },
)
async def parse_docx(path: str) -> dict:
    doc = DocxDocument(path)
    paragraphs = [p.text for p in doc.paragraphs if p.text.strip()]
    tables = []
    for table in doc.tables:
        rows = []
        for row in table.rows:
            rows.append([cell.text for cell in row.cells])
        tables.append(rows)
    return {
        "path": path,
        "paragraphs": paragraphs,
        "tables": tables,
        "paragraph_count": len(paragraphs),
        "table_count": len(tables),
    }

@tool_registry.register(
    name="parse_xlsx",
    description="Extract data from an Excel spreadsheet.",
    risk_level="GREEN",
    parameters={
        "path": {"type": "string", "description": "Path to XLSX file"},
        "sheet": {"type": "string", "description": "Sheet name (default: first)", "default": ""},
        "max_rows": {"type": "integer", "default": 100},
    },
)
async def parse_xlsx(path: str, sheet: str = "", max_rows: int = 100) -> dict:
    df = pd.read_excel(path, sheet_name=sheet or 0, nrows=max_rows)
    return {
        "path": path,
        "shape": list(df.shape),
        "columns": list(df.columns),
        "data": df.head(max_rows).to_dict(orient="records"),
        "dtypes": {col: str(dtype) for col, dtype in df.dtypes.items()},
    }

@tool_registry.register(
    name="parse_csv",
    description="Read and analyze a CSV file.",
    risk_level="GREEN",
    parameters={
        "path": {"type": "string", "description": "Path to CSV file"},
        "max_rows": {"type": "integer", "default": 100},
    },
)
async def parse_csv(path: str, max_rows: int = 100) -> dict:
    df = pd.read_csv(path, nrows=max_rows)
    return {
        "path": path,
        "shape": list(df.shape),
        "columns": list(df.columns),
        "data": df.head(max_rows).to_dict(orient="records"),
        "summary": df.describe().to_dict() if df.select_dtypes(include="number").columns.any() else {},
    }
```

### Step 6.2 — Document Summarizer

```python
@tool_registry.register(
    name="summarize_document",
    description="Summarize a document file (PDF, DOCX, TXT, MD, CSV).",
    risk_level="GREEN",
    parameters={
        "path": {"type": "string", "description": "Path to document"},
        "max_length": {"type": "integer", "description": "Max summary words", "default": 200},
    },
)
async def summarize_document(path: str, max_length: int = 200) -> dict:
    # Extract text based on file type
    ext = Path(path).suffix.lower()
    text = ""
    if ext == ".pdf":
        result = await parse_pdf(path, max_pages=20)
        text = "\n".join(p["text"] for p in result.data["content"])
    elif ext == ".docx":
        result = await parse_docx(path)
        text = "\n".join(result.data["paragraphs"])
    elif ext == ".csv":
        result = await parse_csv(path, max_rows=50)
        text = f"CSV with columns: {result.data['columns']}. {result.data['shape'][0]} rows."
    else:
        text = Path(path).read_text(encoding="utf-8", errors="replace")[:50000]

    # Truncate for LLM context
    text = text[:10000]

    # Send to LLM for summarization
    from kria.agent.llm_client import llm_client
    result = await llm_client.chat_completion(
        messages=[
            {"role": "system", "content": f"Summarize the following document in {max_length} words or fewer. Focus on key points."},
            {"role": "user", "content": text},
        ],
        max_tokens=500,
    )
    summary = result["choices"][0]["message"]["content"] if result else "Summarization failed."
    return {"path": path, "summary": summary}
```

### Step 6.3 — Document Converter

```
src/kria/tools/document_convert.py
```

```python
import asyncio
from pathlib import Path
from kria.tools.registry import tool_registry
from kria.infra.platform_detect import has_command

@tool_registry.register(
    name="convert_document",
    description="Convert a document between formats (e.g., MD→PDF, DOCX→PDF, XLSX→CSV).",
    risk_level="YELLOW",
    parameters={
        "input_path": {"type": "string", "description": "Source file path"},
        "output_format": {"type": "string", "description": "Target format: pdf, docx, txt, html, csv, md"},
        "output_path": {"type": "string", "description": "Output file path", "default": ""},
    },
)
async def convert_document(input_path: str, output_format: str, output_path: str = "") -> dict:
    src = Path(input_path)
    if not src.exists():
        return {"error": f"File not found: {input_path}"}

    if not output_path:
        output_path = str(src.with_suffix(f".{output_format}"))

    # Use pandoc if available (supports many format pairs)
    if has_command("pandoc"):
        proc = await asyncio.create_subprocess_exec(
            "pandoc", str(src), "-o", output_path,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        _, stderr = await proc.communicate()
        if proc.returncode == 0:
            return {"input": input_path, "output": output_path, "format": output_format}
        return {"error": f"Pandoc failed: {stderr.decode()}"}

    # Fallback: handle specific conversions
    if src.suffix == ".xlsx" and output_format == "csv":
        import pandas as pd
        df = pd.read_excel(input_path)
        df.to_csv(output_path, index=False)
        return {"input": input_path, "output": output_path, "format": "csv"}

    return {"error": f"Cannot convert {src.suffix} → .{output_format}. Install pandoc for more format support."}
```

### Step 6.4 — File Watcher

```
src/kria/tools/file_watcher.py
```

```python
import asyncio
import logging
from pathlib import Path
from watchdog.observers import Observer
from watchdog.events import FileSystemEventHandler
from kria.tools.registry import tool_registry
from kria.automation.event_bus import event_bus

logger = logging.getLogger("kria.tools.file_watcher")

class KriaFileHandler(FileSystemEventHandler):
    def on_created(self, event):
        if not event.is_directory:
            asyncio.get_event_loop().call_soon_threadsafe(
                event_bus.emit, "file_created", {"path": event.src_path}
            )

    def on_modified(self, event):
        if not event.is_directory:
            asyncio.get_event_loop().call_soon_threadsafe(
                event_bus.emit, "file_modified", {"path": event.src_path}
            )

    def on_deleted(self, event):
        if not event.is_directory:
            asyncio.get_event_loop().call_soon_threadsafe(
                event_bus.emit, "file_deleted", {"path": event.src_path}
            )

class FileWatcherManager:
    def __init__(self):
        self._observers: dict[str, Observer] = {}

    def watch(self, directory: str) -> bool:
        if directory in self._observers:
            return False
        observer = Observer()
        observer.schedule(KriaFileHandler(), directory, recursive=False)
        observer.start()
        self._observers[directory] = observer
        return True

    def unwatch(self, directory: str) -> bool:
        obs = self._observers.pop(directory, None)
        if obs:
            obs.stop()
            obs.join(timeout=5)
            return True
        return False

    def list_watched(self) -> list[str]:
        return list(self._observers.keys())

    def stop_all(self):
        for obs in self._observers.values():
            obs.stop()
        for obs in self._observers.values():
            obs.join(timeout=5)
        self._observers.clear()

file_watcher = FileWatcherManager()
```

### Deliverables — Phase 6

- [ ] `document_parser.py` — PDF, DOCX, XLSX, CSV parsing
- [ ] `summarize_document` tool — LLM-powered document summarization
- [ ] `document_convert.py` — format conversion via pandoc/built-in
- [ ] `file_organizer.py` — rule-based file organization
- [ ] `file_watcher.py` — directory monitoring with event emission
- [ ] Unit tests with sample documents

---

## 10. Phase 7 — OS-Level Task Management Tools

**Goal:** Implement service management, scheduled tasks, environment variables, disk management, and power control.

### Step 7.1 — Service Manager

```
src/kria/tools/service_mgmt.py
```

```python
import asyncio
from kria.tools.registry import tool_registry
from kria.infra.platform_detect import OS, OSType

@tool_registry.register(
    name="list_services",
    description="List system services and their status.",
    risk_level="GREEN",
    parameters={
        "filter": {"type": "string", "description": "Filter by name (optional)", "default": ""},
    },
)
async def list_services(filter: str = "") -> dict:
    if OS == OSType.LINUX:
        cmd = ["systemctl", "list-units", "--type=service", "--no-pager", "--plain"]
        if filter:
            cmd.extend(["--pattern", f"*{filter}*"])
    else:
        cmd = ["powershell", "-Command", f"Get-Service | Where-Object Name -like '*{filter}*' | Format-Table Name, Status, DisplayName -AutoSize"]

    proc = await asyncio.create_subprocess_exec(
        *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
    )
    stdout, _ = await proc.communicate()
    return {"services": stdout.decode(errors="replace"), "os": OS.value}

@tool_registry.register(
    name="manage_service",
    description="Start, stop, restart, or check status of a service.",
    risk_level="RED",
    parameters={
        "name": {"type": "string", "description": "Service name"},
        "action": {"type": "string", "description": "start | stop | restart | status"},
    },
)
async def manage_service(name: str, action: str) -> dict:
    if action == "status":
        # Status checks are GREEN-equivalent
        pass

    if OS == OSType.LINUX:
        cmd = ["systemctl", action, name]
    else:
        actions_map = {"start": "Start", "stop": "Stop", "restart": "Restart"}
        cmd = ["powershell", "-Command", f"{actions_map.get(action, action)}-Service -Name '{name}'"]

    proc = await asyncio.create_subprocess_exec(
        *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
    )
    stdout, stderr = await proc.communicate()
    return {
        "service": name,
        "action": action,
        "success": proc.returncode == 0,
        "output": stdout.decode(errors="replace"),
        "error": stderr.decode(errors="replace") if proc.returncode != 0 else None,
    }
```

### Step 7.2 — Disk Management

```
src/kria/tools/disk_mgmt.py
```

```python
import os
import hashlib
from pathlib import Path
from collections import defaultdict
from kria.tools.registry import tool_registry

@tool_registry.register(
    name="find_large_files",
    description="Find the largest files in a directory.",
    risk_level="GREEN",
    parameters={
        "directory": {"type": "string", "description": "Directory to scan"},
        "top_n": {"type": "integer", "default": 20},
        "min_size_mb": {"type": "integer", "default": 10},
    },
)
async def find_large_files(directory: str, top_n: int = 20, min_size_mb: int = 10) -> list:
    min_bytes = min_size_mb * 1024 * 1024
    files = []
    for root, dirs, filenames in os.walk(directory):
        for fname in filenames:
            fpath = Path(root) / fname
            try:
                size = fpath.stat().st_size
                if size >= min_bytes:
                    files.append({"path": str(fpath), "size_mb": round(size / (1024 * 1024), 2)})
            except (PermissionError, OSError):
                continue
        if len(files) > 1000:
            break
    files.sort(key=lambda x: x["size_mb"], reverse=True)
    return files[:top_n]

@tool_registry.register(
    name="find_duplicate_files",
    description="Find duplicate files by hash in a directory.",
    risk_level="GREEN",
    parameters={
        "directory": {"type": "string", "description": "Directory to scan"},
        "min_size_kb": {"type": "integer", "default": 100},
    },
)
async def find_duplicate_files(directory: str, min_size_kb: int = 100) -> dict:
    min_bytes = min_size_kb * 1024
    size_groups = defaultdict(list)

    for root, _, filenames in os.walk(directory):
        for fname in filenames:
            fpath = Path(root) / fname
            try:
                size = fpath.stat().st_size
                if size >= min_bytes:
                    size_groups[size].append(str(fpath))
            except (PermissionError, OSError):
                continue

    duplicates = []
    for size, paths in size_groups.items():
        if len(paths) < 2:
            continue
        hash_groups = defaultdict(list)
        for p in paths:
            try:
                h = hashlib.md5(open(p, "rb").read(8192)).hexdigest()
                hash_groups[h].append(p)
            except Exception:
                continue
        for h, hpaths in hash_groups.items():
            if len(hpaths) >= 2:
                duplicates.append({
                    "hash": h,
                    "size_kb": round(size / 1024, 1),
                    "files": hpaths,
                })

    return {"duplicate_groups": duplicates, "total_groups": len(duplicates)}

@tool_registry.register(
    name="calculate_dir_size",
    description="Calculate total size of a directory recursively.",
    risk_level="GREEN",
    parameters={
        "path": {"type": "string", "description": "Directory path"},
    },
)
async def calculate_dir_size(path: str) -> dict:
    total = 0
    file_count = 0
    for root, dirs, files in os.walk(path):
        for f in files:
            try:
                total += (Path(root) / f).stat().st_size
                file_count += 1
            except (PermissionError, OSError):
                continue
    return {
        "path": path,
        "size_mb": round(total / (1024 * 1024), 2),
        "size_gb": round(total / (1024 ** 3), 2),
        "file_count": file_count,
    }
```

### Step 7.3 — Power Management

```
src/kria/tools/power_mgmt.py
```

```python
import asyncio
from kria.tools.registry import tool_registry
from kria.infra.platform_detect import OS, OSType

@tool_registry.register(
    name="lock_screen",
    description="Lock the display/screen.",
    risk_level="GREEN",
)
async def lock_screen() -> str:
    if OS == OSType.LINUX:
        await asyncio.create_subprocess_exec("loginctl", "lock-session")
    else:
        await asyncio.create_subprocess_exec(
            "powershell", "-Command", "rundll32.exe user32.dll,LockWorkStation"
        )
    return "Screen locked"

@tool_registry.register(
    name="shutdown_system",
    description="Shut down the computer. Requires approval.",
    risk_level="RED",
    parameters={
        "delay_minutes": {"type": "integer", "default": 0},
    },
)
async def shutdown_system(delay_minutes: int = 0) -> str:
    if OS == OSType.LINUX:
        cmd = ["shutdown", "-h", f"+{delay_minutes}" if delay_minutes else "now"]
    else:
        seconds = delay_minutes * 60
        cmd = ["shutdown", "/s", f"/t", str(seconds)]
    proc = await asyncio.create_subprocess_exec(*cmd)
    return f"Shutdown scheduled in {delay_minutes} minutes"

@tool_registry.register(
    name="reboot_system",
    description="Reboot the computer. Requires approval.",
    risk_level="RED",
)
async def reboot_system() -> str:
    if OS == OSType.LINUX:
        cmd = ["reboot"]
    else:
        cmd = ["shutdown", "/r", "/t", "0"]
    await asyncio.create_subprocess_exec(*cmd)
    return "Rebooting..."
```

### Step 7.4 — Environment Manager

```
src/kria/tools/env_mgmt.py
```

```python
import os
from kria.tools.registry import tool_registry

@tool_registry.register(
    name="get_environment_variable",
    description="Read an environment variable value.",
    risk_level="GREEN",
    parameters={
        "name": {"type": "string", "description": "Variable name"},
    },
)
async def get_environment_variable(name: str) -> dict:
    value = os.environ.get(name)
    return {"name": name, "value": value, "exists": value is not None}

@tool_registry.register(
    name="list_environment_variables",
    description="List environment variables (filtered, no secrets).",
    risk_level="GREEN",
    parameters={
        "filter": {"type": "string", "default": ""},
    },
)
async def list_environment_variables(filter: str = "") -> dict:
    # Redact sensitive-looking values
    sensitive_patterns = ["key", "secret", "password", "token", "auth"]
    result = {}
    for k, v in sorted(os.environ.items()):
        if filter and filter.lower() not in k.lower():
            continue
        if any(p in k.lower() for p in sensitive_patterns):
            result[k] = "****REDACTED****"
        else:
            result[k] = v
    return {"variables": result, "count": len(result)}
```

### Deliverables — Phase 7

- [ ] `service_mgmt.py` — cross-platform service list/start/stop/restart
- [ ] `disk_mgmt.py` — large files, duplicates, dir size
- [ ] `power_mgmt.py` — lock, shutdown, reboot, sleep
- [ ] `env_mgmt.py` — environment variable read/list (write is RED)
- [ ] `task_scheduler.py` — cron/Task Scheduler management
- [ ] Unit tests with platform-specific mocks

---

## 11. Phase 8 — Application Lifecycle Management

**Goal:** Install, uninstall, update, and query packages via system package managers.

### Step 8.1 — Application Lifecycle Tools

```
src/kria/tools/app_lifecycle.py
```

```python
import asyncio
from kria.tools.registry import tool_registry
from kria.infra.platform_detect import OS, OSType, PACKAGE_MANAGER

@tool_registry.register(
    name="search_package",
    description="Search for an installable package in system repositories.",
    risk_level="GREEN",
    parameters={
        "query": {"type": "string", "description": "Package name to search"},
    },
)
async def search_package(query: str) -> dict:
    if PACKAGE_MANAGER == "apt":
        cmd = ["apt", "search", query]
    elif PACKAGE_MANAGER == "dnf":
        cmd = ["dnf", "search", query]
    elif PACKAGE_MANAGER == "winget":
        cmd = ["winget", "search", query]
    elif PACKAGE_MANAGER == "brew":
        cmd = ["brew", "search", query]
    else:
        return {"error": "No supported package manager found"}

    proc = await asyncio.create_subprocess_exec(
        *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
    )
    stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=30.0)
    return {
        "query": query,
        "package_manager": PACKAGE_MANAGER,
        "results": stdout.decode(errors="replace")[:5000],
    }

@tool_registry.register(
    name="install_application",
    description="Install an application using system package manager. Requires approval.",
    risk_level="RED",
    parameters={
        "package_name": {"type": "string", "description": "Package to install"},
    },
)
async def install_application(package_name: str) -> dict:
    if PACKAGE_MANAGER == "apt":
        cmd = ["sudo", "apt", "install", "-y", package_name]
    elif PACKAGE_MANAGER == "dnf":
        cmd = ["sudo", "dnf", "install", "-y", package_name]
    elif PACKAGE_MANAGER == "winget":
        cmd = ["winget", "install", "--accept-source-agreements", "--accept-package-agreements", package_name]
    elif PACKAGE_MANAGER == "brew":
        cmd = ["brew", "install", package_name]
    else:
        return {"error": "No supported package manager found"}

    proc = await asyncio.create_subprocess_exec(
        *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
    )
    stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=300.0)
    return {
        "package": package_name,
        "success": proc.returncode == 0,
        "output": stdout.decode(errors="replace")[:3000],
        "error": stderr.decode(errors="replace")[:1000] if proc.returncode != 0 else None,
    }

@tool_registry.register(
    name="uninstall_application",
    description="Uninstall an application. Requires approval.",
    risk_level="RED",
    parameters={
        "package_name": {"type": "string", "description": "Package to uninstall"},
    },
)
async def uninstall_application(package_name: str) -> dict:
    if PACKAGE_MANAGER == "apt":
        cmd = ["sudo", "apt", "remove", "-y", package_name]
    elif PACKAGE_MANAGER == "winget":
        cmd = ["winget", "uninstall", package_name]
    elif PACKAGE_MANAGER == "brew":
        cmd = ["brew", "uninstall", package_name]
    else:
        return {"error": "No supported package manager found"}

    proc = await asyncio.create_subprocess_exec(
        *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
    )
    stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=120.0)
    return {
        "package": package_name,
        "success": proc.returncode == 0,
        "output": stdout.decode(errors="replace")[:3000],
    }

@tool_registry.register(
    name="check_updates_available",
    description="List available package updates.",
    risk_level="GREEN",
)
async def check_updates_available() -> dict:
    if PACKAGE_MANAGER == "apt":
        await asyncio.create_subprocess_exec("sudo", "apt", "update",
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
        cmd = ["apt", "list", "--upgradable"]
    elif PACKAGE_MANAGER == "winget":
        cmd = ["winget", "upgrade"]
    elif PACKAGE_MANAGER == "brew":
        cmd = ["brew", "outdated"]
    else:
        return {"error": "No supported package manager found"}

    proc = await asyncio.create_subprocess_exec(
        *cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
    )
    stdout, _ = await asyncio.wait_for(proc.communicate(), timeout=60.0)
    return {
        "package_manager": PACKAGE_MANAGER,
        "updates": stdout.decode(errors="replace")[:5000],
    }
```

### Deliverables — Phase 8

- [ ] `app_lifecycle.py` — search, install, uninstall, update, check updates
- [ ] Cross-platform: apt, dnf, winget, brew abstraction
- [ ] All installs/uninstalls are RED tier
- [ ] Package search/list is GREEN tier
- [ ] Unit tests with subprocess mocks

---

## 12. Phase 9 — Notification & Communication Hub

**Goal:** Desktop notifications, email drafting, clipboard management, and timed reminders.

### Step 9.1 — Desktop Notifications

```
src/kria/tools/notification.py
```

```python
import logging
from kria.tools.registry import tool_registry
from kria.infra.platform_detect import OS, OSType

logger = logging.getLogger("kria.tools.notification")

@tool_registry.register(
    name="send_notification",
    description="Send a desktop notification.",
    risk_level="GREEN",
    parameters={
        "title": {"type": "string", "description": "Notification title"},
        "body": {"type": "string", "description": "Notification body"},
        "urgency": {"type": "string", "description": "low | normal | critical", "default": "normal"},
    },
)
async def send_notification(title: str, body: str, urgency: str = "normal") -> dict:
    try:
        if OS == OSType.LINUX:
            import asyncio
            urgency_map = {"low": "low", "normal": "normal", "critical": "critical"}
            await asyncio.create_subprocess_exec(
                "notify-send", "-u", urgency_map.get(urgency, "normal"), title, body
            )
        else:
            from plyer import notification as plyer_notify
            plyer_notify.notify(title=title, message=body, timeout=10)
        return {"sent": True, "title": title}
    except Exception as e:
        logger.error(f"Notification failed: {e}")
        return {"sent": False, "error": str(e)}
```

### Step 9.2 — Email Composer

```
src/kria/tools/email_composer.py
```

```python
import webbrowser
import urllib.parse
from kria.tools.registry import tool_registry

@tool_registry.register(
    name="compose_email",
    description="Draft an email. Does NOT send — opens in default email client for review.",
    risk_level="GREEN",
    parameters={
        "to": {"type": "string", "description": "Recipient email(s), comma-separated"},
        "subject": {"type": "string", "description": "Email subject"},
        "body": {"type": "string", "description": "Email body text"},
    },
)
async def compose_email(to: str, subject: str, body: str) -> dict:
    # Create mailto URI
    params = urllib.parse.urlencode({"subject": subject, "body": body}, quote_via=urllib.parse.quote)
    mailto_uri = f"mailto:{to}?{params}"
    return {
        "mailto_uri": mailto_uri,
        "to": to,
        "subject": subject,
        "body": body,
        "note": "Email draft ready. Use open_email_draft to open in email client.",
    }

@tool_registry.register(
    name="open_email_draft",
    description="Open an email draft in the default email client via mailto: URI.",
    risk_level="GREEN",
    parameters={
        "mailto_uri": {"type": "string", "description": "mailto: URI from compose_email"},
    },
)
async def open_email_draft(mailto_uri: str) -> str:
    webbrowser.open(mailto_uri)
    return "Email draft opened in default email client"
```

### Step 9.3 — Clipboard Manager

```
src/kria/tools/clipboard_mgr.py
```

```python
import subprocess
from collections import deque
from kria.tools.registry import tool_registry
from kria.infra.platform_detect import OS, OSType

_clipboard_history: deque = deque(maxlen=20)

@tool_registry.register(
    name="get_clipboard",
    description="Read current clipboard content.",
    risk_level="GREEN",
)
async def get_clipboard() -> dict:
    try:
        if OS == OSType.LINUX:
            result = subprocess.run(["xclip", "-selection", "clipboard", "-o"],
                                     capture_output=True, text=True, timeout=5)
            content = result.stdout
        else:
            result = subprocess.run(
                ["powershell", "-Command", "Get-Clipboard"],
                capture_output=True, text=True, timeout=5)
            content = result.stdout.strip()
        _clipboard_history.append(content)
        return {"content": content[:5000], "length": len(content)}
    except Exception as e:
        return {"error": str(e)}

@tool_registry.register(
    name="set_clipboard",
    description="Write text to clipboard.",
    risk_level="YELLOW",
    parameters={
        "text": {"type": "string", "description": "Text to copy to clipboard"},
    },
)
async def set_clipboard(text: str) -> str:
    try:
        if OS == OSType.LINUX:
            proc = subprocess.Popen(["xclip", "-selection", "clipboard"],
                                     stdin=subprocess.PIPE)
            proc.communicate(text.encode())
        else:
            subprocess.run(
                ["powershell", "-Command", f"Set-Clipboard -Value '{text}'"],
                timeout=5)
        _clipboard_history.append(text)
        return f"Copied {len(text)} chars to clipboard"
    except Exception as e:
        return f"Failed: {e}"

@tool_registry.register(
    name="clipboard_history",
    description="Get recent clipboard history (session only, last 20 entries).",
    risk_level="GREEN",
)
async def clipboard_history() -> dict:
    return {"entries": list(_clipboard_history), "count": len(_clipboard_history)}
```

### Step 9.4 — Reminder System

```
src/kria/tools/reminder.py
```

```python
from datetime import datetime, timedelta
from kria.tools.registry import tool_registry

@tool_registry.register(
    name="schedule_reminder",
    description="Set a reminder that triggers a desktop notification at a specified time.",
    risk_level="GREEN",
    parameters={
        "message": {"type": "string", "description": "Reminder message"},
        "minutes_from_now": {"type": "integer", "description": "Minutes from now to trigger"},
    },
)
async def schedule_reminder(message: str, minutes_from_now: int) -> dict:
    from kria.automation.scheduler import scheduler
    trigger_time = datetime.now() + timedelta(minutes=minutes_from_now)
    job_id = scheduler.add_one_shot(
        trigger_time=trigger_time,
        tool_name="send_notification",
        params={"title": "Reminder", "body": message},
    )
    return {
        "reminder_set": True,
        "message": message,
        "trigger_at": trigger_time.isoformat(),
        "job_id": job_id,
    }
```

### Deliverables — Phase 9

- [ ] `notification.py` — cross-platform desktop notifications
- [ ] `email_composer.py` — email drafting + mailto: opener
- [ ] `clipboard_mgr.py` — read/write/history
- [ ] `reminder.py` — timed notification scheduler
- [ ] Unit tests for all communication tools

---

## 13. Phase 10 — Knowledge Base & Learning System

**Goal:** Document RAG, persistent facts, user preference learning, and code snippet library.

### Step 10.1 — Document Ingestion (RAG)

```
src/kria/tools/doc_ingest.py
```

```python
import uuid
from pathlib import Path
from kria.tools.registry import tool_registry
from kria.memory.semantic import semantic_memory

@tool_registry.register(
    name="ingest_document",
    description="Ingest a document into the knowledge base for future Q&A.",
    risk_level="GREEN",
    parameters={
        "path": {"type": "string", "description": "Path to document"},
        "chunk_size": {"type": "integer", "default": 512},
    },
)
async def ingest_document(path: str, chunk_size: int = 512) -> dict:
    # Parse document
    ext = Path(path).suffix.lower()
    from kria.tools.document_parser import parse_pdf, parse_docx
    if ext == ".pdf":
        result = await parse_pdf(path)
        text = "\n".join(p["text"] for p in result.data["content"])
    elif ext == ".docx":
        result = await parse_docx(path)
        text = "\n".join(result.data["paragraphs"])
    else:
        text = Path(path).read_text(encoding="utf-8", errors="replace")

    # Chunk text
    chunks = [text[i:i+chunk_size] for i in range(0, len(text), chunk_size - 50)]  # 50 char overlap

    # Store in ChromaDB
    for i, chunk in enumerate(chunks):
        await semantic_memory.add(
            text=chunk,
            metadata={"source": path, "chunk": i, "type": "document"},
            doc_id=f"doc_{uuid.uuid4().hex[:8]}",
        )

    return {"path": path, "chunks_ingested": len(chunks), "total_chars": len(text)}
```

### Step 10.2 — Knowledge Tools

```
src/kria/tools/knowledge_tools.py
```

```python
from kria.tools.registry import tool_registry
from kria.memory.persistent import sqlite_manager

@tool_registry.register(
    name="remember_fact",
    description="Store a fact or piece of information for later recall.",
    risk_level="GREEN",
    parameters={
        "key": {"type": "string", "description": "Short label (e.g., 'project_deadline')"},
        "value": {"type": "string", "description": "The information to remember"},
    },
)
async def remember_fact(key: str, value: str) -> str:
    await sqlite_manager.execute(
        "INSERT OR REPLACE INTO user_preferences (key, value) VALUES (?, ?)",
        (f"fact:{key}", value),
    )
    return f"Remembered: {key} = {value}"

@tool_registry.register(
    name="recall_fact",
    description="Recall a previously stored fact.",
    risk_level="GREEN",
    parameters={
        "key": {"type": "string", "description": "Fact label to recall"},
    },
)
async def recall_fact(key: str) -> dict:
    rows = await sqlite_manager.execute(
        "SELECT value FROM user_preferences WHERE key = ?",
        (f"fact:{key}",),
    )
    if rows:
        return {"key": key, "value": rows[0][0]}
    return {"key": key, "value": None, "note": "No fact found with this key"}

@tool_registry.register(
    name="list_remembered",
    description="List all stored facts.",
    risk_level="GREEN",
)
async def list_remembered() -> dict:
    rows = await sqlite_manager.execute(
        "SELECT key, value FROM user_preferences WHERE key LIKE 'fact:%'",
    )
    facts = {r[0].replace("fact:", ""): r[1] for r in rows}
    return {"facts": facts, "count": len(facts)}

@tool_registry.register(
    name="search_knowledge",
    description="Semantic search across ingested documents and stored knowledge.",
    risk_level="GREEN",
    parameters={
        "query": {"type": "string", "description": "Search query"},
    },
)
async def search_knowledge(query: str) -> dict:
    from kria.memory.semantic import semantic_memory
    results = await semantic_memory.search(query, n_results=5)
    return {"query": query, "results": results, "count": len(results)}
```

### Deliverables — Phase 10

- [ ] `doc_ingest.py` — chunk + embed documents into ChromaDB
- [ ] `knowledge_tools.py` — remember/recall/list/search facts
- [ ] `snippet_lib.py` — code/text snippet CRUD
- [ ] `user_prefs.py` — user preference learning (memory module)
- [ ] Unit tests for knowledge operations

---

## 14. Phase 11 — Automation & Workflow Engine

**Goal:** Build the event bus, scheduler, YAML workflow engine, and macro recorder.

### Step 11.1 — Event Bus

```
src/kria/automation/event_bus.py
```

```python
import asyncio
import logging
from typing import Callable
from collections import defaultdict

logger = logging.getLogger("kria.automation.events")

class EventBus:
    """Pub/sub event bus for system events (file changes, app launches, etc.)."""

    def __init__(self):
        self._handlers: dict[str, list[Callable]] = defaultdict(list)

    def subscribe(self, event_type: str, handler: Callable):
        self._handlers[event_type].append(handler)
        logger.debug(f"Subscribed to '{event_type}'")

    def unsubscribe(self, event_type: str, handler: Callable):
        self._handlers[event_type].remove(handler)

    def emit(self, event_type: str, data: dict):
        for handler in self._handlers.get(event_type, []):
            try:
                if asyncio.iscoroutinefunction(handler):
                    asyncio.ensure_future(handler(event_type, data))
                else:
                    handler(event_type, data)
            except Exception as e:
                logger.error(f"Event handler error for '{event_type}': {e}")

event_bus = EventBus()
```

### Step 11.2 — Scheduler

```
src/kria/automation/scheduler.py
```

```python
import uuid
import logging
from datetime import datetime
from apscheduler.schedulers.asyncio import AsyncIOScheduler
from kria.tools.registry import tool_registry as _registry

logger = logging.getLogger("kria.automation.scheduler")

class KriaScheduler:
    def __init__(self):
        self._scheduler = AsyncIOScheduler()
        self._jobs: dict[str, dict] = {}

    def start(self):
        self._scheduler.start()
        logger.info("Scheduler started")

    def stop(self):
        self._scheduler.shutdown()

    def add_one_shot(self, trigger_time: datetime, tool_name: str, params: dict) -> str:
        job_id = f"oneshot_{uuid.uuid4().hex[:8]}"

        async def _execute():
            await _registry.execute(tool_name, params)
            self._jobs.pop(job_id, None)

        self._scheduler.add_job(_execute, "date", run_date=trigger_time, id=job_id)
        self._jobs[job_id] = {"tool": tool_name, "params": params, "trigger": trigger_time.isoformat()}
        return job_id

    def add_cron(self, cron_expr: str, tool_name: str, params: dict, name: str = "") -> str:
        job_id = f"cron_{uuid.uuid4().hex[:8]}"
        parts = cron_expr.split()

        async def _execute():
            await _registry.execute(tool_name, params)

        self._scheduler.add_job(
            _execute, "cron",
            minute=parts[0], hour=parts[1],
            day=parts[2], month=parts[3], day_of_week=parts[4],
            id=job_id,
        )
        self._jobs[job_id] = {"name": name, "tool": tool_name, "cron": cron_expr}
        return job_id

    def remove(self, job_id: str) -> bool:
        try:
            self._scheduler.remove_job(job_id)
            self._jobs.pop(job_id, None)
            return True
        except Exception:
            return False

    def list_jobs(self) -> list[dict]:
        return [{"id": k, **v} for k, v in self._jobs.items()]

scheduler = KriaScheduler()
```

### Step 11.3 — Workflow Engine

```
src/kria/automation/workflow_engine.py
```

```python
import yaml
import logging
from pathlib import Path
from kria.tools.registry import tool_registry
from kria.infra.config import settings

logger = logging.getLogger("kria.automation.workflow")

class WorkflowEngine:
    def __init__(self):
        self._workflows_dir = Path(settings.workflows_dir).expanduser()
        self._workflows_dir.mkdir(parents=True, exist_ok=True)

    def list_workflows(self) -> list[dict]:
        workflows = []
        for f in self._workflows_dir.glob("*.yml"):
            try:
                data = yaml.safe_load(f.read_text())
                workflows.append({
                    "file": f.name,
                    "name": data.get("name", f.stem),
                    "trigger": data.get("trigger", {}),
                    "steps": len(data.get("steps", [])),
                })
            except Exception as e:
                logger.warning(f"Bad workflow {f.name}: {e}")
        return workflows

    async def run_workflow(self, name: str, variables: dict = None) -> dict:
        wf_path = self._workflows_dir / f"{name}.yml"
        if not wf_path.exists():
            return {"error": f"Workflow '{name}' not found"}

        wf = yaml.safe_load(wf_path.read_text())
        variables = variables or {}
        results = []

        for step in wf.get("steps", []):
            step_name = step.get("name", "unnamed")

            # Evaluate condition
            condition = step.get("condition")
            if condition and not self._eval_condition(condition, variables):
                results.append({"step": step_name, "skipped": True, "reason": "Condition not met"})
                continue

            # Resolve template variables in params
            params = self._resolve_vars(step.get("params", {}), variables)
            tool_name = step["tool"]

            result = await tool_registry.execute(tool_name, params)
            results.append({"step": step_name, "tool": tool_name, "result": result.data if result.success else result.error})

            # Save result to variables
            save_as = step.get("save_as")
            if save_as and result.success:
                variables[save_as] = result.data

        return {"workflow": name, "steps_executed": len(results), "results": results}

    def _resolve_vars(self, params: dict, variables: dict) -> dict:
        import re
        resolved = {}
        for k, v in params.items():
            if isinstance(v, str):
                def replace_var(match):
                    var_path = match.group(1)
                    parts = var_path.split(".")
                    val = variables
                    for p in parts:
                        if isinstance(val, dict):
                            val = val.get(p, match.group(0))
                        else:
                            return match.group(0)
                    return str(val)
                resolved[k] = re.sub(r"\{\{(\w+(?:\.\w+)*)\}\}", replace_var, v)
            else:
                resolved[k] = v
        return resolved

    def _eval_condition(self, condition: str, variables: dict) -> bool:
        # Simple condition evaluator: "{{var}} > 85"
        import re
        resolved = re.sub(
            r"\{\{(\w+(?:\.\w+)*)\}\}",
            lambda m: str(self._get_nested(variables, m.group(1))),
            condition
        )
        try:
            return bool(eval(resolved))  # Sandboxed in workflow context only
        except Exception:
            return False

    def _get_nested(self, data: dict, path: str):
        parts = path.split(".")
        val = data
        for p in parts:
            if isinstance(val, dict):
                val = val.get(p)
            else:
                return None
        return val

workflow_engine = WorkflowEngine()
```

### Deliverables — Phase 11

- [ ] `event_bus.py` — pub/sub system event bus
- [ ] `scheduler.py` — APScheduler wrapper for one-shot + cron jobs
- [ ] `workflow_engine.py` — YAML workflow parser + executor
- [ ] `macro_recorder.py` — record tool calls + replay
- [ ] Tool wrappers for creating/listing/running workflows
- [ ] Unit tests for event bus, scheduler, workflow variable resolution

---

## 15. Phase 12 — Plugin Architecture

**Goal:** Enable third-party plugins that add new tools, event handlers, and capabilities.

### Step 12.1 — Plugin Loader

```
src/kria/plugins/loader.py
```

```python
import importlib.util
import logging
from pathlib import Path
import yaml
from kria.infra.config import settings

logger = logging.getLogger("kria.plugins")

class PluginLoader:
    def __init__(self):
        self._plugins_dir = Path(settings.plugins_dir).expanduser()
        self._plugins_dir.mkdir(parents=True, exist_ok=True)
        self._loaded: dict[str, dict] = {}

    def discover(self) -> list[dict]:
        """Find all plugins in the plugins directory."""
        plugins = []
        for d in self._plugins_dir.iterdir():
            if d.is_dir():
                manifest = d / "plugin.yml"
                if manifest.exists():
                    data = yaml.safe_load(manifest.read_text())
                    data["path"] = str(d)
                    data["enabled"] = data.get("enabled", True)
                    plugins.append(data)
        return plugins

    def load(self, plugin_name: str) -> bool:
        """Load a plugin by name."""
        plugin_dir = self._plugins_dir / plugin_name
        manifest = plugin_dir / "plugin.yml"
        init_file = plugin_dir / "__init__.py"

        if not manifest.exists() or not init_file.exists():
            logger.error(f"Plugin '{plugin_name}' missing manifest or __init__.py")
            return False

        try:
            spec = importlib.util.spec_from_file_location(
                f"kria_plugin_{plugin_name}", str(init_file)
            )
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)

            if hasattr(module, "setup"):
                module.setup()

            self._loaded[plugin_name] = {
                "module": module,
                "manifest": yaml.safe_load(manifest.read_text()),
            }
            logger.info(f"Plugin loaded: {plugin_name}")
            return True
        except Exception as e:
            logger.error(f"Plugin load failed '{plugin_name}': {e}")
            return False

    def unload(self, plugin_name: str) -> bool:
        plugin = self._loaded.pop(plugin_name, None)
        if plugin and hasattr(plugin["module"], "teardown"):
            plugin["module"].teardown()
        return plugin is not None

    def list_loaded(self) -> list[str]:
        return list(self._loaded.keys())

plugin_loader = PluginLoader()
```

### Deliverables — Phase 12

- [ ] `loader.py` — plugin discovery, loading, unloading
- [ ] `manager.py` — plugin install/enable/disable tools
- [ ] `api.py` — plugin API interface (what plugins can access)
- [ ] Plugin manifest schema (plugin.yml)
- [ ] Example plugin template
- [ ] Unit tests for plugin lifecycle

---

## 16. Phase 13 — Sensory Pipeline (Voice)

**Goal:** Build the full voice pipeline and then systematically upgrade STT quality to approach Google/Siri-level accuracy — all running 100% locally.

*(Base implementation uses existing code in `voice/` directory. This phase adds the Speech Recognition Enhancement Plan from `SPEECH_RECOGNITION.md`.)*

### Step 13.1 — Base Voice Pipeline

Implement the core voice components:

- `stt_client.py` — Whisper.cpp HTTP client
- `tts_client.py` — Piper TTS HTTP client
- `wake_word.py` — OpenWakeWord integration (custom "Hey KRIA")
- `vad.py` — Silero VAD v5 for speech boundary detection
- `pipeline.py` — Full orchestrator: wake word → VAD → STT → agent → TTS

All components must gracefully degrade (STT down → text input, TTS down → text output).

### Step 13.2 — GPU-Accelerated Whisper (Highest Impact Change)

The RTX 4050 has 6 GB VRAM and sits idle during STT. Moving Whisper to GPU yields **10-20x speedup**.

**Docker changes:**
- Build whisper.cpp with `-DGGML_CUDA=ON` in the brain Dockerfile
- Add `--gpu-layers 99` to whisper-server launch in `entrypoint.sh`
- Add NVIDIA runtime to docker-compose for kria-brain
- Keep `LLAMA_GPU_LAYERS=0` initially (LLM stays on CPU)

**Expected result:** Whisper small.en drops from ~2s → ~0.2s per utterance.

**Upgrade to medium.en on GPU:**

| Model | Params | Size | WER (LibriSpeech) | GPU Inference |
|---|---|---|---|---|
| small.en (current) | 244M | 487 MB | 7.7% | ~0.15s |
| **medium.en** | **769M** | **1.5 GB** | **5.8%** | **~0.3s** |
| large-v3-turbo | 809M | 1.6 GB | 5.2% | ~0.4s |

`medium.en` is the sweet spot — 25% more accurate and fits in 6 GB VRAM alongside the LLM.

**VRAM budget (both on GPU):**

| Component | VRAM Usage |
|---|---|
| Whisper medium.en | ~1.5 GB |
| Phi-4-mini Q4_K_M | ~2.5 GB |
| CUDA overhead | ~0.5 GB |
| **Total** | **~2.9 GB / 6.0 GB** |

### Step 13.3 — Audio Preprocessing Pipeline

Laptop fans produce low-frequency rumble (50-300 Hz) that confuses both VAD and Whisper. Add a preprocessing stack in the bridge:

```python
# Apply in sequence before Whisper:
# 1. High-pass filter at 300 Hz (remove fan noise / rumble)
# 2. Spectral gating (remove stationary noise while preserving speech)
# 3. Automatic Gain Control — normalize to -20 dBFS
```

**High-pass filter (300 Hz Butterworth):**

```python
import scipy.signal as signal
b, a = signal.butter(4, 300, btype='high', fs=16000)
audio = signal.lfilter(b, a, audio).astype(np.int16)
```

**Spectral gating (better than noisereduce):**

```python
def spectral_gate(audio, sr=16000):
    f, t, Zxx = signal.stft(audio, fs=sr, nperseg=512)
    magnitude = np.abs(Zxx)
    phase = np.angle(Zxx)
    noise_frames = int(0.2 * sr / 256)
    noise_profile = np.mean(magnitude[:, :noise_frames], axis=1, keepdims=True)
    mask = np.maximum(magnitude - 2 * noise_profile, 0) / (magnitude + 1e-10)
    mask = np.clip(mask, 0.05, 1.0)
    cleaned = magnitude * mask * np.exp(1j * phase)
    _, audio_out = signal.istft(cleaned, fs=sr)
    return audio_out.astype(np.int16)
```

**Automatic Gain Control:**

```python
def agc(audio, target_db=-20):
    peak = np.max(np.abs(audio.astype(np.float32)))
    if peak < 100:
        return audio
    target_peak = 32768 * (10 ** (target_db / 20))
    gain = target_peak / peak
    return (audio.astype(np.float32) * gain).clip(-32768, 32767).astype(np.int16)
```

### Step 13.4 — faster-whisper Alternative

[faster-whisper](https://github.com/SYSTRAN/faster-whisper) uses CTranslate2 — typically **2-4x faster** than whisper.cpp on GPU with built-in Silero VAD:

| Feature | whisper.cpp | faster-whisper |
|---|---|---|
| GPU support | CUDA (manual build) | CUDA + cuDNN out of box |
| VAD integration | No | Built-in Silero VAD |
| Word timestamps | Basic | Accurate |
| **Speed (GPU, medium.en)** | **~0.3s** | **~0.15s** |

**Implementation:** Replace whisper.cpp server with a Python FastAPI wrapper:

```python
from faster_whisper import WhisperModel
from fastapi import FastAPI, UploadFile

model = WhisperModel("medium.en", device="cuda", compute_type="float16")
app = FastAPI()

@app.post("/inference")
async def inference(file: UploadFile):
    segments, info = model.transcribe(
        file.file, language="en", beam_size=5,
        vad_filter=True,
        vad_parameters=dict(min_speech_duration_ms=250, min_silence_duration_ms=800),
    )
    text = " ".join(seg.text for seg in segments).strip()
    return {"text": text}
```

### Step 13.5 — Distil-Whisper Evaluation

[Distil-Whisper](https://github.com/huggingface/distil-whisper) `distil-large-v3` runs **faster than small.en on GPU** while offering near large-v3 accuracy (fewer decoder layers):

| Model | Params | Speed (GPU) | WER | VRAM |
|---|---|---|---|---|
| small.en (current) | 244M | 0.15s | 7.7% | ~0.5 GB |
| **distil-large-v3** | **756M** | **0.12s** | **5.7%** | **~1.8 GB** |
| large-v3 | 1.55B | 0.4s | 4.2% | ~3.0 GB |

```python
model = WhisperModel("distil-large-v3", device="cuda", compute_type="float16")
```

### Step 13.6 — NVIDIA Parakeet TDT (Zero Hallucination STT)

NVIDIA's [Parakeet TDT 0.6B](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v2) leads the Open ASR Leaderboard and **eliminates hallucinations entirely** — its Transducer architecture can only output tokens corresponding to actual audio frames:

| Feature | Whisper | Parakeet TDT |
|---|---|---|
| WER (LibriSpeech clean) | 5-8% | **2.9%** |
| Hallucinations | Common | **None** (CTC-based) |
| Streaming capable | No (offline) | **Yes** (real-time) |
| VRAM (0.6B) | ~1 GB | ~1.5 GB |

```python
import nemo.collections.asr as nemo_asr
model = nemo_asr.models.ASRModel.from_pretrained("nvidia/parakeet-tdt-0.6b-v2")
model = model.to("cuda")
transcription = model.transcribe(["audio.wav"])
```

**VRAM budget with Parakeet:**

| Component | VRAM |
|---|---|
| Parakeet TDT 0.6B | 1.5 GB |
| Phi-4-mini Q4_K_M | 2.5 GB |
| Overhead | 0.5 GB |
| **Total** | **4.5 GB / 6.0 GB** |

### Step 13.7 — Fine-Tuning for Specific Environment (LoRA)

Fine-tuning on your specific microphone, room acoustics, and voice yields dramatic accuracy improvements:

1. **Collect 30-60 minutes** of voice data with the KRIA microphone
2. **Augment with room noise** at various SNR levels (5, 10, 15, 20 dB)
3. **LoRA fine-tune** (only ~2M trainable params vs 769M total, trains in minutes on RTX 4050):

```python
from peft import LoraConfig, get_peft_model
lora_config = LoraConfig(r=16, lora_alpha=32, target_modules=["q_proj", "v_proj"], lora_dropout=0.05)
model = get_peft_model(whisper_model, lora_config)
```

### Step 13.8 — Streaming ASR (Sub-200ms Latency)

Replace "record → process" with real-time streaming via WebSocket:

```python
@app.websocket("/ws/transcribe")
async def ws_transcribe(ws: WebSocket):
    await ws.accept()
    while True:
        audio_chunk = await ws.receive_bytes()
        text = model.transcribe_stream(audio_chunk)
        if text:
            await ws.send_json({"text": text, "is_final": False})
```

Parakeet TDT supports streaming natively. Partial text appears instantly as the user speaks.

### Accuracy Targets

| Configuration | WER (Clean) | WER (Noisy) | Latency | Hallucinations |
|---|---|---|---|---|
| **Current** (small.en, CPU) | ~7.7% | ~15-20% | ~2s | Frequent |
| **Step 13.2** (medium.en, GPU) | ~5.8% | ~10-12% | ~0.3s | Moderate |
| **Step 13.4** (faster-whisper, distil-large-v3) | ~5.0% | ~8-10% | ~0.15s | Low |
| **Step 13.6** (Parakeet TDT) | ~2.9% | ~6-8% | ~0.1s | **None** |
| **Step 13.7** (Fine-tuned + Parakeet) | ~2% | ~4-5% | ~0.1s | **None** |
| Google/Siri (reference) | ~2% | ~3-5% | <0.2s | Very rare |

### Deliverables — Phase 13

- [ ] `stt_client.py`, `tts_client.py`, `wake_word.py`, `vad.py`, `pipeline.py`
- [ ] All voice components gracefully degrade
- [ ] GPU-accelerated Whisper (CUDA build + docker-compose GPU config)
- [ ] Audio preprocessing pipeline (high-pass + spectral gate + AGC)
- [ ] faster-whisper evaluation and Docker integration
- [ ] Distil-Whisper / Parakeet TDT evaluation
- [ ] VRAM orchestration for concurrent STT + LLM on GPU
- [ ] Integration test: mock audio → STT → agent → TTS
- [ ] Benchmark: measure WER and latency at each upgrade step

---

## 17. Phase 14 — Memory & Context

*(Extended from v1.0 Phase 6 — adds document memory and user preferences)*

### Deliverables — Phase 14

- [ ] `conversation.py` — sliding window buffer
- [ ] `persistent.py` — SQLite + FTS5 + conversation store
- [ ] `semantic.py` — ChromaDB for conversation + document RAG
- [ ] `context_manager.py` — 3-tier builder + document context
- [ ] `user_prefs.py` — preference tracking and learning
- [ ] All memory tiers degrade independently

---

## 18. Phase 15 — Web Dashboard

*(Extended from v1.0 Phase 7 — adds file explorer, workflow editor, plugin manager, settings)*

### Deliverables — Phase 15

- [ ] `websocket.py`, `routes.py` — full API
- [ ] React dashboard: Chat, HITL, StatusBar, AuditLog, SystemMonitor
- [ ] **New:** FileExplorer component
- [ ] **New:** WorkflowEditor component (visual workflow builder)
- [ ] **New:** PluginManager UI
- [ ] **New:** Settings/Preferences panel
- [ ] **New:** Notification center
- [ ] Responsive layout

---

## 19. Phase 16 — Docker Deployment & GPU

*(Identical to v1.0 Phase 8 — see existing Docker configs)*

### Deliverables — Phase 16

- [ ] All Dockerfiles verified
- [ ] GPU passthrough tested
- [ ] Model download init container
- [ ] Bridge daemon with secret auth
- [ ] `docker compose up -d` works from cold boot

---

## 20. Phase 17 — Integration, Testing & Hardening

### Step 17.1 — Expanded Test Suite

```
src/tests/
├── test_agent_loop.py              — Full ReAct loop with mock LLM
├── test_safety_pipeline.py         — Policy → HITL → Rollback → Audit E2E
├── test_tool_isolation.py          — One tool crash doesn't affect others
├── test_voice_pipeline.py          — Audio → STT → Agent → TTS with mocks
├── test_memory_degradation.py      — ChromaDB/Redis/SQLite down scenarios
├── test_circuit_breakers.py        — Service failure → recovery → fallback
├── test_web_tools.py               — Internet tools with mocked HTTP
├── test_file_tools.py              — File/document tools with temp files
├── test_os_tools.py                — OS tools with mocked subprocess
├── test_app_lifecycle.py           — Package manager tools with mocks
├── test_notification.py            — Notification dispatch
├── test_knowledge.py               — RAG + fact store
├── test_automation.py              — Scheduler + workflow engine
├── test_plugins.py                 — Plugin lifecycle
├── test_concurrent.py              — Multiple simultaneous requests
└── conftest.py                     — Shared fixtures
```

### Step 17.2 — Full Failure Scenario Matrix

| Test | Scenario | Expected Behavior |
|---|---|---|
| `test_llm_down` | llama.cpp unreachable | Canned error response, no crash |
| `test_stt_down` | whisper.cpp unreachable | Falls back to text input |
| `test_tts_down` | Piper unreachable | Text response only |
| `test_redis_down` | Redis refused | In-memory fallback |
| `test_chromadb_down` | ChromaDB unreachable | RAG skipped, buffer + SQLite only |
| `test_sqlite_down` | SQLite locked | Graceful skip writes |
| `test_internet_down` | No internet | Web tools fail gracefully, local tools unaffected |
| `test_tool_crash` | Tool raises exception | `ToolResult(success=False)`, loop continues |
| `test_tool_timeout` | Tool hangs 60s | Killed after timeout |
| `test_llm_infinite_loop` | LLM keeps calling tools | Stops at MAX_ITERATIONS |
| `test_black_action` | Format C: attempt | BLACK policy blocks |
| `test_hitl_timeout` | RED action, no response | Auto-deny after 30s |
| `test_download_too_large` | 1GB download attempt | Rejected by size limit |
| `test_concurrent_tools` | 5 simultaneous tool calls | Semaphore respected |
| `test_workflow_bad_yaml` | Malformed workflow | Parse error, no crash |
| `test_plugin_crash` | Plugin raises on load | Plugin disabled, core unaffected |
| `test_rollback` | Delete file → undo | File restored |
| `test_emergency_stop` | Emergency command | All killed, safe mode |

### Deliverables — Phase 17

- [ ] All 18+ failure tests pass
- [ ] Load test: 10 concurrent sessions
- [ ] Security hardening checklist complete
- [ ] Performance benchmarks meet targets
- [ ] CI pipeline running all tests

---

## 21. Phase 18 — Post-Launch Roadmap

*(See Phases 19–24 below for detailed implementation of these roadmap items.)*

| Version | Features | Phase Reference |
|---|---|---|
| **v1.0** | Complete AI Assistant — 65+ tools, voice, internet, files, OS, automation | Phases 0–17 |
| **v1.1** | Dynamic Model Routing, Multi-Language Voice, Screen Vision | Phases 19–21 |
| **v1.2** | Extended Interfaces (Telegram, System Tray, Git, Universal Search) | Phase 22 |
| **v1.3** | Context Awareness, Proactive Intelligence, Daily Briefings | Phase 23 |
| **v1.4** | Performance Benchmarking, Safety Demo Mode, Advanced Automation | Phase 24 |
| **v2.0** | Multi-Modal (camera, gestures), Distributed Agent Mesh | Future |

---

## 22. Phase 19 — Dynamic Model Routing & Cascading Inference

**Goal:** Route user requests to the optimal model (or no model) based on task complexity — achieving **up to 70x speedup** for trivial commands while preserving full reasoning for complex tasks.

### 19.1 Architecture: Task-Based Routing

```
User Command
    │
    ▼
┌──────────────┐
│ Intent Router │ ← Lightweight classifier (rules + small model)
└──────┬───────┘
       │
       ├── TRIVIAL ("open Chrome") ──────→ 🟢 Direct tool dispatch (no LLM)
       ├── SIMPLE ("search for X") ──────→ 🟡 Phi-4-mini (~10ms first token)
       ├── COMPLEX ("analyze this PDF") ─→ 🔴 Qwen2.5-VL-7B (~180ms first token)
       └── VISION ("what's on screen") ──→ 🟣 Qwen2.5-VL-7B (on demand)
```

### 19.2 Available Models (All Free, All Local)

| Model | VRAM | Purpose | Load Time |
|---|---|---|---|
| **Phi-4-mini-instruct** Q4_K_M | ~2.5 GB | Primary reasoning, tool calling, everyday tasks | ~1 second |
| **Qwen2.5-VL-7B-Instruct** Q4_K_M | ~4.7 GB | Complex reasoning, vision tasks (secondary) | ~4 seconds |
| **nomic-embed-text** GGUF | ~270 MB (CPU) | Embeddings for RAG | Already loaded |

### 19.3 Routing Logic

```python
# Pattern-matched trivial commands (no LLM needed):
TRIVIAL_PATTERNS = [
    r"^open\s+(.+)",           # "open Chrome"
    r"^close\s+(.+)",          # "close Firefox"
    r"^(what time|what's the time)",
    r"^lock\s+screen",
    r"^(battery|volume|brightness)",
    r"^screenshot",
]
# → Direct tool dispatch, ~30ms total

# Simple commands (small model sufficient):
SIMPLE_INDICATORS = [
    "search for", "what's the weather", "remind me",
    "read file", "set volume", "set brightness",
]
# → Phi-4-mini, ~80ms total

# Everything else → Qwen2.5-VL-7B
# Fallback: If 0.6B responds "I need more reasoning power" → escalate to 8B
```

### 19.4 Performance Gains

| Command | Without Routing | With Routing | Speedup |
|---|---|---|---|
| "Open Chrome" | 8B model → ~400ms | Direct tool → ~30ms | **13x** |
| "What time is it?" | 8B model → ~350ms | Direct tool → ~5ms | **70x** |
| "Search for Python tutorials" | 8B model → ~500ms | 0.6B model → ~80ms | **6x** |
| Complex multi-step task | 8B model → ~2s | 8B model → ~2s | Same |

**70-80% of daily commands are trivial or simple.** Routing makes K.R.I.A. feel instant.

### 19.5 GPU Time-Multiplexed Model Swapping

Models are swapped in/out of VRAM as needed via the existing `vram_orchestrator.py`:

| State | What's Loaded | VRAM |
|---|---|---|
| **Idle** | Whisper + Phi-4-mini | ~4.5 GB |
| **Simple task** | Whisper + Phi-4-mini | ~4.5 GB |
| **Complex task** | Whisper + Qwen2.5-VL-7B | ~6.7 GB |
| **Vision task** | Qwen2.5-VL-7B + mmproj (Whisper unloaded) | ~5.2 GB |

- **Cold swap:** 2-4 seconds (first load)
- **Warm swap:** <500ms (mmap caching keeps model in system RAM)

### 19.6 Implementation Steps

1. Update `router.py` — add regex pattern matching for trivial commands
2. Add model tier enum to `llm_client.py` — `TRIVIAL | SIMPLE | COMPLEX | VISION`
3. Create `model_switcher.py` in `src/kria/agent/` — manages llama.cpp `--model` switching
4. Update `vram_orchestrator.py` — model swap scheduling and VRAM accounting
5. Add `KRIA_MODEL_ROUTING_ENABLED` config flag (default: `true`)
6. Add benchmarking for routing decision accuracy

### Deliverables — Phase 19

- [ ] Pattern-based trivial command detection (no LLM dispatch)
- [ ] Model tier classification in router
- [ ] VRAM-aware model switching via llama.cpp API
- [ ] Config: `model_routing_enabled`, `small_model_path`, `large_model_path`
- [ ] Latency benchmarks per routing tier
- [ ] Unit tests for routing classification accuracy

---

## 23. Phase 20 — Multi-Language Voice Support

**Goal:** Enable Hindi and other language support across the voice pipeline with minimal configuration changes.

### 20.1 Current Multilingual Capability

| Component | Languages Supported | Configured Now |
|---|---|---|
| **STT (Whisper)** | 99 languages (Hindi, Urdu, Arabic, French, etc.) | English only |
| **LLM (Phi-4-mini / Qwen2.5-VL-7B)** | English, Chinese, Hindi, and 20+ natively | English only |
| **TTS (Piper)** | Voice models for 30+ languages | `en_US-lessac-high` only |
| **Wake Word** | "Hey KRIA" is phonetic — language-independent | Works for all |

### 20.2 Hindi Voice Support (Highest Priority for India BTech)

**Effort:** Very low — the technology already supports it.

**Step 1 — STT:** Change Whisper `language` param or set to `auto`:

```python
# In stt_client.py or whisper config
language = "auto"  # Auto-detect (adds ~20ms latency)
# OR
language = "hi"    # Lock to Hindi when selected
```

**Step 2 — TTS:** Download Hindi Piper voice model (~65MB):

```bash
# Available Hindi voices:
# hi_IN-*-medium.onnx  — multiple speaker options
wget https://huggingface.co/rhasspy/piper-voices/resolve/main/hi/hi_IN/...
```

**Step 3 — LLM:** No changes needed — both Phi-4-mini and Qwen2.5-VL handle Hindi natively.

### 20.3 Language Switching Tool

```python
@tool_registry.register(
    name="set_language",
    description="Switch the voice pipeline language.",
    risk_level="GREEN",
    parameters={
        "language": {"type": "string", "description": "Language code: en, hi, ur, zh, fr, es, etc."},
    },
)
async def set_language(language: str) -> dict:
    # Update STT language setting
    # Switch TTS voice model
    # Notify pipeline of language change
    return {"language": language, "stt": "updated", "tts": "updated"}
```

### 20.4 Supported Languages (By Model Availability)

| Language | Whisper STT | Qwen3 LLM | Piper TTS | Full Pipeline |
|---|---|---|---|---|
| **English** | ✅ | ✅ | ✅ (multiple voices) | ✅ |
| **Hindi** | ✅ | ✅ | ✅ | ✅ |
| **Chinese** | ✅ | ✅ (native) | ✅ | ✅ |
| **Spanish** | ✅ | ✅ | ✅ | ✅ |
| **French** | ✅ | ✅ | ✅ | ✅ |
| **Arabic** | ✅ | ✅ | ✅ | ✅ |
| **Urdu** | ✅ | ✅ | Partial | STT+LLM only |
| Other (99 langs) | ✅ | Partial | Varies | Depends on TTS |

### 20.5 Configuration Addition

Add to `config.py`:

```python
# Language
language: str = "en"
language_auto_detect: bool = False
tts_voice_models: dict = {
    "en": "en_US-lessac-high",
    "hi": "hi_IN-medium",
}
```

### Deliverables — Phase 20

- [ ] `set_language` tool for runtime language switching
- [ ] Whisper auto-detect or per-language config
- [ ] Hindi Piper voice model download in `download_models.py`
- [ ] Config: `language`, `language_auto_detect`, `tts_voice_models`
- [ ] System prompt localization (basic Hindi system prompt variant)
- [ ] Test: Hindi voice command → Hindi response pipeline

---

## 24. Phase 21 — Screen Vision Module

**Goal:** Add "What's on my screen?" capability using a local vision-language model for screenshot analysis, error reading, and visual content understanding.

### 21.1 Architecture

```
User: "What's on my screen?" / "Read this error message"
    │
    ▼
┌──────────────────┐
│  screenshot tool  │ ← Captures current screen
└────────┬─────────┘
         │
         ▼
┌──────────────────────────────┐
│  Qwen2.5-VL-3B (Q4_K_M)     │ ← Vision-Language Model
│  ~2.5 GB VRAM — loaded on    │
│  demand, unloaded after use   │
└────────┬─────────────────────┘
         │
         ▼
┌──────────────────┐
│  Structured text  │ ← Description, OCR text, error parsing
└──────────────────┘
```

### 21.2 Model Selection

| Model | VRAM | Speed | Capability |
|---|---|---|---|
| **Qwen2.5-VL-3B** Q4_K_M | ~2.5 GB | ~1.5s per image | Best local VLM at this size |
| Qwen2.5-VL-7B Q4_K_M | ~4.5 GB | ~3s per image | Higher accuracy, tight VRAM fit |
| LLaVA-1.6-7B | ~4.5 GB | ~3s per image | Alternative, strong OCR |

Recommended: **Qwen2.5-VL-3B** — fits alongside Whisper in VRAM and handles screenshots, charts, error messages accurately.

### 21.3 VRAM Strategy

Vision tasks are infrequent — load the model on demand:

1. Unload LLM from GPU (keep in RAM via mmap)
2. Load Qwen2.5-VL-3B to GPU (~2.5 GB, ~2 second load)
3. Process screenshot
4. Unload vision model, reload LLM

Total turnaround: ~4 seconds — acceptable for a "What's on my screen?" query.

### 21.4 Vision Tools

```python
@tool_registry.register(
    name="analyze_screenshot",
    description="Take a screenshot and describe what's on screen.",
    risk_level="GREEN",
    parameters={
        "question": {"type": "string", "description": "What to look for", "default": "Describe what you see"},
        "region": {"type": "string", "description": "Screen region: full, top-half, bottom-half", "default": "full"},
    },
)
async def analyze_screenshot(question: str = "Describe what you see", region: str = "full") -> dict:
    # 1. Capture screenshot
    screenshot_path = await capture_screen(region)
    # 2. Load vision model (VRAM swap)
    # 3. Send image + question to VLM
    # 4. Return structured analysis
    return {"description": response, "screenshot_path": screenshot_path}

@tool_registry.register(
    name="read_screen_text",
    description="Extract all text visible on screen (OCR via VLM).",
    risk_level="GREEN",
)
async def read_screen_text() -> dict:
    screenshot_path = await capture_screen("full")
    text = await vlm_ocr(screenshot_path)
    return {"text": text}
```

### 21.5 Use Cases

| Command | What Happens |
|---|---|
| "What's on my screen?" | Full screenshot → VLM description |
| "Read the error message" | Screenshot → OCR → extract error text |
| "Summarize this chart" | Screenshot → VLM chart analysis |
| "What app is in the foreground?" | Screenshot → VLM identifies application |
| "Is there a notification?" | Screenshot → VLM checks notification area |

### Deliverables — Phase 21

- [ ] Screen capture utility (`screenshot` tool enhancement)
- [ ] Qwen2.5-VL-3B GGUF model download in `download_models.py`
- [ ] VLM client in `src/kria/agent/vlm_client.py`
- [ ] VRAM swap logic: LLM ↔ VLM on demand
- [ ] `analyze_screenshot` and `read_screen_text` tools
- [ ] Integration test: screenshot → VLM → structured output

---

## 25. Phase 22 — Extended Interface Layer

**Goal:** Add alternative input/output interfaces beyond voice and web dashboard — Telegram bot, system tray, Git integration, and universal search.

### 22.1 Telegram Bot Interface

Control K.R.I.A. remotely from your phone via Telegram — ~100 lines of Python using `python-telegram-bot`:

```python
# src/kria/api/telegram_bot.py
from telegram.ext import Application, CommandHandler, MessageHandler, filters
import httpx

KRIA_API = "http://localhost:8000/api/v1"

async def handle_message(update, context):
    user_text = update.message.text
    async with httpx.AsyncClient() as client:
        resp = await client.post(f"{KRIA_API}/chat", json={"message": user_text})
        result = resp.json()
    await update.message.reply_text(result["response"])

async def handle_status(update, context):
    async with httpx.AsyncClient() as client:
        resp = await client.get(f"{KRIA_API}/health")
    await update.message.reply_text(f"Status: {resp.json()}")

app = Application.builder().token(settings.telegram_bot_token).build()
app.add_handler(CommandHandler("status", handle_status))
app.add_handler(MessageHandler(filters.TEXT & ~filters.COMMAND, handle_message))
```

**Safety:** Telegram bot only accepts commands from an allowlisted `chat_id` (yours). All tool calls go through the standard policy engine.

**Config addition:**

```python
telegram_enabled: bool = False
telegram_bot_token: str = ""
telegram_allowed_chat_ids: list[int] = []
```

### 22.2 System Tray Agent

Native desktop presence using `pystray` — shows K.R.I.A. status in the system tray:

```python
# scripts/kria_tray.py
import pystray
from PIL import Image

def create_tray():
    icon = pystray.Icon("kria", Image.open("assets/kria_icon.png"), "K.R.I.A.")
    icon.menu = pystray.Menu(
        pystray.MenuItem("Status", show_status),
        pystray.MenuItem("Open Dashboard", open_dashboard),
        pystray.MenuItem("Mute Wake Word", toggle_wake_word),
        pystray.MenuItem("Emergency Stop", emergency_stop),
        pystray.MenuItem("Quit", quit_kria),
    )
    icon.run()
```

Features:
- Green/yellow/red icon based on system health
- Quick access to dashboard, mute, emergency stop
- Notification display for HITL requests
- Status tooltip showing active model + memory usage

### 22.3 Git Integration Tools

Developer-friendly Git operations exposed as K.R.I.A. tools:

```python
@tool_registry.register(
    name="git_status",
    description="Show git repository status.",
    risk_level="GREEN",
    parameters={"path": {"type": "string", "description": "Repository path", "default": "."}},
)
async def git_status(path: str = ".") -> dict: ...

@tool_registry.register(
    name="git_log",
    description="Show recent git commit history.",
    risk_level="GREEN",
    parameters={
        "path": {"type": "string", "default": "."},
        "count": {"type": "integer", "default": 10},
    },
)
async def git_log(path: str = ".", count: int = 10) -> dict: ...

@tool_registry.register(
    name="git_diff",
    description="Show uncommitted changes.",
    risk_level="GREEN",
    parameters={"path": {"type": "string", "default": "."}},
)
async def git_diff(path: str = ".") -> dict: ...

@tool_registry.register(
    name="git_commit",
    description="Stage all changes and commit with a message. Requires approval.",
    risk_level="RED",
    parameters={
        "message": {"type": "string", "description": "Commit message"},
        "path": {"type": "string", "default": "."},
    },
)
async def git_commit(message: str, path: str = ".") -> dict: ...
```

Additional tools: `git_branch`, `git_checkout`, `git_pull`, `git_push` (RED tier).

### 22.4 Universal Search

A single "search everything" tool combining files, documents, knowledge base, and web:

```python
@tool_registry.register(
    name="universal_search",
    description="Search across files, documents, knowledge base, and optionally the web.",
    risk_level="GREEN",
    parameters={
        "query": {"type": "string", "description": "Search query"},
        "scope": {"type": "string", "description": "all | files | docs | knowledge | web", "default": "all"},
    },
)
async def universal_search(query: str, scope: str = "all") -> dict:
    results = {}
    if scope in ("all", "files"):
        results["files"] = await search_files(query)
    if scope in ("all", "docs"):
        results["documents"] = await search_knowledge(query)
    if scope in ("all", "knowledge"):
        results["facts"] = await recall_fact(query)
    if scope in ("all", "web") and settings.internet_enabled:
        results["web"] = await web_search(query, max_results=3)
    return results
```

### Deliverables — Phase 22

- [ ] `telegram_bot.py` — Telegram interface with allowlisted users
- [ ] `kria_tray.py` — system tray agent with status + quick actions
- [ ] `git_tools.py` — git status/log/diff/commit/branch/push tools
- [ ] `universal_search` tool — combined search across all data sources
- [ ] Config additions: Telegram token, tray settings
- [ ] Unit tests for each interface module

---

## 26. Phase 23 — Context Awareness & Proactive Intelligence

**Goal:** Make K.R.I.A. feel like a real personal assistant by understanding context (time, battery, WiFi, running apps) and proactively offering help.

### 23.1 Context Signals

K.R.I.A. continuously monitors ambient system state:

| Signal | Source | Update Frequency |
|---|---|---|
| **Time of day** | `datetime` | Continuous |
| **Day of week** | `datetime` | Continuous |
| **Battery level** | `psutil` | Every 60 seconds |
| **WiFi network** | `nmcli` / `netsh` | On change |
| **Running applications** | `psutil` | Every 30 seconds |
| **Idle time** | Input monitoring | Continuous |
| **Disk usage** | `psutil` | Every 5 minutes |
| **Active displays** | `xrandr` / WMI | On change |

### 23.2 Daily Briefing

Triggered by wake word or on schedule (e.g., 9 AM weekdays):

```yaml
# ~/.kria/workflows/daily_briefing.yml
name: "Daily Briefing"
trigger:
  type: schedule
  cron: "0 9 * * 1-5"
steps:
  - tool: get_weather
    params: { location: "auto" }
    save_as: weather
  - tool: get_disk_space
    params: { path: "/" }
    save_as: disk
  - tool: list_scheduled_tasks
    save_as: tasks
  - tool: check_updates_available
    save_as: updates
  - tool: send_notification
    params:
      title: "Good Morning!"
      body: >
        Weather: {{weather.description}}, {{weather.temp_c}}°C.
        Disk: {{disk.percent}}% used.
        {{tasks.count}} tasks today.
        {{updates.count}} packages need updating.
```

### 23.3 Proactive Suggestions

Context-triggered suggestions based on learned patterns and system state:

| Context | Trigger | Suggestion |
|---|---|---|
| Morning + Work WiFi | 9 AM on weekday | "Good morning! Want me to open VS Code and check your calendar?" |
| Battery < 20% | Battery monitor | "Battery is at 18%. Shall I close non-essential apps?" |
| Large download complete | File watcher | "Your download is done. Want me to open the file?" |
| Idle > 30 min | Idle monitor | "You've been idle for 30 minutes. Want me to lock the screen?" |
| New USB drive | Device monitor | "USB drive detected. Want me to back up your project?" |
| Disk > 85% | Disk monitor | "Disk is 87% full. Want me to find large files to clean up?" |

### 23.4 Context-Enriched Prompts

Inject ambient context into the LLM system prompt:

```python
CONTEXT_TEMPLATE = """
Current context:
- Time: {time} ({day_of_week})
- Battery: {battery}%{charging_status}
- WiFi: {wifi_network}
- Active apps: {running_apps}
- Disk: {disk_percent}% used
- Last interaction: {idle_time} ago
"""
```

This helps the LLM make context-aware decisions without the user explicitly stating their situation.

### Deliverables — Phase 23

- [ ] `context_monitor.py` in `src/kria/infra/` — background context signal collector
- [ ] Daily briefing workflow template
- [ ] Proactive suggestion engine (rule-based + learned patterns)
- [ ] Context injection into LLM system prompt
- [ ] Config: `proactive_suggestions_enabled`, `daily_briefing_time`
- [ ] Test: context change → appropriate suggestion triggers

---

## 27. Phase 24 — Performance Benchmarking & Safety Demo Mode

**Goal:** Build built-in benchmarking and a presentation-friendly safety demonstration mode.

### 24.1 Performance Benchmarking Dashboard

A built-in tool and dashboard page showing real-time performance metrics:

```python
@tool_registry.register(
    name="run_benchmark",
    description="Run a performance benchmark on all K.R.I.A. subsystems.",
    risk_level="GREEN",
)
async def run_benchmark() -> dict:
    results = {}
    # LLM: time to first token + tokens/sec
    results["llm_first_token_ms"] = await benchmark_llm_first_token()
    results["llm_tokens_per_sec"] = await benchmark_llm_throughput()
    # STT: time to transcribe 3s audio
    results["stt_latency_ms"] = await benchmark_stt()
    # TTS: time to first audio byte
    results["tts_latency_ms"] = await benchmark_tts()
    # Tool execution: average tool call latency
    results["tool_avg_ms"] = await benchmark_tool_calls()
    # Web search: end-to-end latency
    results["web_search_ms"] = await benchmark_web_search()
    # Memory: context retrieval latency
    results["memory_retrieval_ms"] = await benchmark_memory()
    return results
```

**Dashboard visualization:**
- Real-time graphs: LLM throughput, STT latency, memory usage
- Historical comparison: track performance across sessions
- Target vs actual: show benchmark targets from latency budget

### 24.2 Safety Demo Mode

A presentation-safe mode that demonstrates the 4-tier safety system without risking actual system changes:

```python
@tool_registry.register(
    name="enable_safety_demo",
    description="Enable safety demonstration mode for presentations.",
    risk_level="GREEN",
)
async def enable_safety_demo() -> str:
    settings.safety_demo_mode = True
    return "Safety demo mode enabled. RED/BLACK actions will be simulated, not executed."
```

**Demo scenarios (scripted for presentations):**

| Scenario | What Happens | Demonstrates |
|---|---|---|
| "Delete my Downloads folder" | RED → HITL popup → shows approval flow | Risk classification + HITL |
| "Format my hard drive" | BLACK → instant deny + audit | Hard deny + audit logging |
| "Open Chrome" | GREEN → executes normally | Auto-execute for safe actions |
| "Install Firefox" | RED → HITL → simulated install | Package management safety |
| "Undo the last action" | Shows rollback capabilities | Rollback system |
| "KRIA, emergency stop" | Full emergency protocol | Emergency stop flow |

In demo mode, RED actions show the full approval UI but simulate execution (no actual changes). BLACK actions are denied normally. GREEN/YELLOW actions execute normally.

### 24.3 Model & System Report Tool

```python
@tool_registry.register(
    name="system_report",
    description="Generate a comprehensive system and model report.",
    risk_level="GREEN",
)
async def system_report() -> dict:
    return {
        "models": {
            "llm": {"name": "Phi-4-mini-instruct", "vram_gb": 2.5, "context": 8192},
            "stt": {"name": "whisper-medium.en", "vram_gb": 1.5},
            "tts": {"name": "piper-lessac-high", "ram_mb": 300},
            "embeddings": {"name": "nomic-embed-text", "ram_mb": 270},
        },
        "hardware": await get_hardware_summary(),
        "tools_registered": len(tool_registry.list_all()),
        "services_healthy": len([s for s in health_registry.get_all().values() if s.status.value == "healthy"]),
        "uptime": await get_system_uptime(),
    }
```

### Deliverables — Phase 24

- [ ] `run_benchmark` tool — bench all subsystems with latency targets
- [ ] Dashboard performance graphs (LLM throughput, STT latency, VRAM usage)
- [ ] `enable_safety_demo` tool for presentation mode
- [ ] Scripted demo scenarios with simulated RED actions
- [ ] `system_report` tool — comprehensive model/system summary
- [ ] Config: `safety_demo_mode` flag

---

## 28. Dependency Graph

Implementation phases must follow this order. Tasks within a phase can be parallelized.

```
Phase 0 ─── Project Skeleton + Platform Detection
    │
    ▼
Phase 1 ─── Infrastructure (Redis, SQLite, Logging, Health)
    │
    ├───────────────────────────────┐
    ▼                               ▼
Phase 2 ─── Reasoning Brain     Phase 3 ─── Safety System
    │              │                 │
    │              └────────┬────────┘
    │                       │
    │                       ▼
    │               Phase 4 ─── Core Tool System
    │                       │
    ├─────────────┬────────┬┼────────┬──────────┬─────────┐
    ▼             ▼        ▼▼        ▼          ▼         ▼
Phase 5      Phase 6   Phase 7   Phase 8    Phase 9   Phase 10
Internet     File/Doc  OS Tasks  App Mgmt   Comms     Knowledge
    │             │        │         │          │         │
    └─────────────┴────────┴─────────┴──────────┴─────────┘
                           │
                           ▼
                   Phase 11 ─── Automation Engine
                           │
                           ▼
                   Phase 12 ─── Plugin Architecture
                           │
                   ┌───────┴───────┐
                   ▼               ▼
           Phase 13            Phase 14
           Voice + STT         Memory & Context
           Enhancement
                   │               │
                   └───────┬───────┘
                           ▼
                   Phase 15 ─── Web Dashboard
                           │
                           ▼
                   Phase 16 ─── Docker Deployment
                           │
                           ▼
                   Phase 17 ─── Integration & Hardening
                           │
          ┌────────────────┼────────────────┐
          ▼                ▼                ▼
    Phase 19          Phase 20         Phase 21
    Model Routing     Multi-Language    Screen Vision
          │                │                │
          └────────────────┼────────────────┘
                           │
                   ┌───────┴───────┐
                   ▼               ▼
           Phase 22            Phase 23
           Extended            Context
           Interfaces          Awareness
                   │               │
                   └───────┬───────┘
                           ▼
                   Phase 24 ─── Benchmarking & Demo
                           │
                           ▼
                   Phase 18 ── v2.0 Roadmap Features
```

**Parallelizable groups:**
- Phase 5–10 (Internet, File, OS, App, Comms, Knowledge) can ALL be built in parallel after Phase 4
- Phase 13 (Voice) and Phase 14 (Memory) can be built in parallel
- Dashboard development (Phase 15) can start as soon as API shapes are defined (Phase 4)
- Phase 19–21 (Model Routing, Multi-Language, Vision) can be built in parallel after Phase 17
- Phase 22 (Interfaces) and Phase 23 (Context Awareness) can be built in parallel

---

## 29. Risk Register

| Risk | Probability | Impact | Mitigation |
|---|---|---|---|
| VRAM overflow (>8 GB) | Medium | Service crash | VRAM orchestrator with dynamic layer offloading |
| LLM hallucinating tool params | High | Wrong action | Policy engine re-validates all params; RED HITL |
| Whisper misheard command | Medium | Wrong action | Transcription confirmation for RED-tier actions; audio preprocessing pipeline |
| Internet dependency creep | Medium | Offline breakage | Strict tool separation; internet tools isolated |
| Document parsing OOM | Medium | Crash | Max file sizes; streaming parsing; paginated PDFs |
| Package manager differences | High | Cross-platform bugs | Abstraction layer with per-platform tests |
| Plugin security | Medium | Privilege escalation | Plugin sandboxing; no access to safety engine |
| Workflow infinite loop | Low | Resource exhaustion | Max step count; per-step timeout |
| Download abuse | Low | Disk fill | Download size limits; quota per session |
| API rate limiting | Medium | Web tools fail | Caching; exponential backoff; circuit breaker |
| Stale cached data | Low | Wrong information | Configurable TTLs; cache invalidation on user request |
| Concurrent tool deadlock | Low | Agent hangs | Per-tool timeout; semaphore; supervised restart |
| Model routing misclassification | Medium | Wrong model selected | Fallback escalation from small → large model; logging for accuracy tracking |
| Vision model VRAM contention | Low | Model swap failure | Explicit VRAM accounting; load/unload sequencing in orchestrator |
| Telegram bot unauthorized access | Low | Remote command injection | Chat ID allowlist; standard policy engine for all commands |
| STT model upgrade regression | Medium | Worse accuracy on specific inputs | A/B benchmark framework; keep previous model as fallback |
| Proactive suggestion fatigue | Medium | User annoyance | Configurable frequency; learn from dismissal patterns; quiet hours |
| Multi-language TTS quality | Low | Unnatural output | Per-language quality testing; fallback to English TTS |

---

*Implementation Guide v3.0 — April 2026*
*K.R.I.A. (Kernel-Responsive Intelligent Agent) — Complete AI Assistant*
*Author: Obaidullah Zeeshan*
