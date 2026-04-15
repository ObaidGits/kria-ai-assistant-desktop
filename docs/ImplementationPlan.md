# K.R.I.A. — Intelligence Implementation Plan (Sovereign-Orchestrator Edition)

> Transform KRIA from a basic LLM chat wrapper into a fully autonomous, tool-using, memory-aware, voice-enabled desktop AI assistant — powered by a Rust Sovereign Core and a Python Pre-Cognitive Sidecar.

**Created:** 2026-04-15  
**Status:** Active  
**Architecture:** Sovereign-Orchestrator (Rust core + Python sidecar)  
**Estimated Phases:** 16  

---

## Table of Contents

- [Current State Audit](#current-state-audit)
- [Phase 0 — The Critical Wiring (Agent Loop)](#phase-0--the-critical-wiring)
- [Phase 0.5 — Python Sidecar & Pre-Cognitive Bridge](#phase-05--python-sidecar--pre-cognitive-bridge)
- [Phase 1 — Persistent Memory & Chat History](#phase-1--persistent-memory--chat-history)
- [Phase 2 — Internet, Search & Real-Time Access](#phase-2--internet-search--real-time-access)
- [Phase 3 — File & System Intelligence](#phase-3--file--system-intelligence)
- [Phase 4 — Vision & Multimodal](#phase-4--vision--multimodal)
- [Phase 5 — Voice Pipeline](#phase-5--voice-pipeline)
- [Phase 6 — Settings, API Keys & UI Overhaul](#phase-6--settings-api-keys--ui-overhaul)
- [Phase 7 — MCP Protocol & Plugin Ecosystem](#phase-7--mcp-protocol--plugin-ecosystem)
- [Phase 7.5 — Skill-Plugin Ecosystem (Python)](#phase-75--skill-plugin-ecosystem-python)
- [Phase 8 — App Management, Automation & Polish](#phase-8--app-management-automation--polish)
- [Phase 9 — Adaptive Hardware Tier System](#phase-9--adaptive-hardware-tier-system)
- [Phase 10 — Desktop Automation & Contextual Awareness](#phase-10--desktop-automation--contextual-awareness)
- [Phase 11 — Developer Power Tools](#phase-11--developer-power-tools)
- [Phase 12 — RAG, Document Chat & Deep Knowledge](#phase-12--rag-document-chat--deep-knowledge)
- [Phase 13 — Proactive Intelligence & Smart Notifications](#phase-13--proactive-intelligence--smart-notifications)
- [Phase 14 — Multi-Language, Accessibility & Cross-Platform Polish](#phase-14--multi-language-accessibility--cross-platform-polish)
- [Technology Stack](#technology-stack)
- [Tracker](#tracker)
- [What Makes a Strong Assistant vs a Dumb Chatbot](#what-makes-a-strong-assistant-vs-a-dumb-chatbot)

---

## Current State Audit

### What actually works today
| Area | Rating | Notes |
|------|--------|-------|
| Desktop app launches | ✅ Working | Tauri v2 + SolidJS |
| Send a message, get a response | ✅ Working | Direct LLM call to llama.cpp or cloud |
| Token streaming | ✅ Working | Tauri events → UI update |
| Tray icon | ✅ Working | Show/hide/quit |

### What's built but completely disconnected
| Component | Lines of Code | Why It's Dead |
|-----------|--------------|---------------|
| Agent ReAct loop (`agent/loop_engine.rs`) | ~210 | `send_message` bypasses it — calls `backend.chat()` directly |
| 60+ tool handlers (file, internet, system, shell, etc.) | ~2000+ | Tools are never passed to the LLM, agent loop not running |
| Safety system (policy/HITL/audit/rollback) | ~800+ | Only activated inside agent loop |
| SQLite memory store | ~250+ | Created at init but never used for chat or facts |
| Voice pipeline (capture/VAD/STT/TTS) | ~400+ | Commands just flip a boolean flag |
| Intent router (50+ patterns) | ~175 | Never called |
| Automation (workflows/macros/scheduler) | ~380+ | Never instantiated |
| Preprocessing (image/doc/web/code) | ~340+ | Never called |

### True stubs (need actual implementation)
| Component | Problem |
|-----------|---------|
| ~~Embeddings (`memory/embeddings.rs`)~~ | ~~Returns fake hash-based vectors~~ → **DONE (Phase 1.5)**: Real ONNX inference with hash fallback |
| ~~Knowledge tools~~ | ~~Return "delegated to memory layer"~~ → **DONE (Phase 1.6)**: Wired to MemoryStore |
| ~~Session history command~~ | ~~Returns empty `[]`~~ → **DONE (Phase 1.2)**: Real SQLite queries |
| ~~Document tools~~ | ~~Basic CLI wrappers~~ → **DONE (Phase 3.4)**: Sidecar-first with native fallback |
| ~~Code intelligence~~ | ~~Only preprocessing/code.rs~~ → **DONE (Phase 3.3)**: 4 tools + CodeProcessor wired |
| ~~File search~~ | ~~Basic glob only~~ → **DONE (Phase 3.2)**: Content search, pattern finder, project tree |
| ~~Tool feedback UI~~ | ~~Static tool-call display~~ → **DONE (Phase 3.7)**: Collapsible blocks with live status |
| ~~Voice start/stop commands~~ | ~~Toggle a boolean, no audio pipeline~~ → **DONE (Phase 5)**: Full pipeline: capture→VAD→STT→agent→TTS→playback, Silero VAD |
| Health endpoint | Hardcoded `{"status": "healthy"}` |
| Settings modal (frontend) | Static HTML, not connected to config |
| MCP client | Does not exist at all |
| Plugin loader | Manifest discovery only, no dynamic loading |
| ~~Python sidecar~~ | ~~Does not exist~~ → **DONE (Phase 0.5)**: Full JSON-RPC sidecar with 6 processors |
| ~~EventBus~~ | ~~Does not exist~~ → **DONE (Phase 0)**: Tokio broadcast channels |

### Root Cause

**One function is the bottleneck:** `send_message` in `commands.rs` directly calls `backend.chat()` with a 2-message array (system prompt + user message). It skips the agent loop, skips tools, skips memory, skips history, skips safety. Fixing this single function unlocks ~90% of the dormant codebase.

**One ecosystem is missing:** The `preprocessing/` module has basic Rust implementations (regex-based code analysis, CLI-based PDF extraction via `pdftotext`) but lacks the depth of Python's ML ecosystem. Image analysis is metadata-only (no OCR), document extraction relies on external CLI tools, code analysis doesn't use ASTs, and there are no real embeddings. A Python sidecar solves all of these with existing, battle-tested libraries.

---

## Phase 0 — The Critical Wiring

> **Goal:** Route `send_message` through the AgentLoop instead of directly to the LLM. This single change activates tools, safety, intent routing, and the ReAct reasoning loop.

**Priority:** CRITICAL — everything else depends on this.

### Steps

- [x] **0.1** Add `AgentLoop` to `AppState` in `commands.rs`
  - Instantiate `AgentLoop` in `init_runtime()` with: `ModelRouter`, `ToolRegistry`, `HitlGateway`, `PolicyEngine`, `AuditLogger`
  - Store as `Arc<AgentLoop>` in `AppState`
  - Also instantiate `EventBus` (tokio::broadcast channels) and store in `AppState`
  - EventBus channels: `file.uploaded`, `message.received`, `tool.completed`, `sidecar.result`, `voice.transcribed`

- [x] **0.2** Rewrite `send_message` to call `AgentLoop::run()` instead of `backend.chat()`
  - Pass user message + conversation history (from memory store)
  - The agent loop handles: intent classification → tool schema injection → LLM call → tool call parsing → safety policy check → HITL approval → tool execution → feed result back → repeat
  - For tools that need pre-processing (image analysis, PDF extraction), the agent loop delegates to `SidecarBridge` (wired in Phase 0.5)

- [x] **0.3** Wire up streaming events from agent loop to frontend
  - Emit `agent:token` for LLM text chunks
  - Emit `agent:tool_call` when a tool is being invoked (new event)
  - Emit `agent:tool_result` when a tool completes (new event)
  - Emit `agent:thinking` during reasoning steps
  - Emit `agent:done` when the full response is complete

- [x] **0.4** Update the system prompt in `prompts.rs`
  - Inject available tool descriptions (already supported via `{tool_descriptions}` placeholder)
  - Add date/time awareness ("Current date: ...")
  - Add user context from memory store (name, preferences, past facts)

- [x] **0.5** Test the full loop
  - "What's my CPU usage?" → triggers `get_cpu_usage` tool → returns real data
  - "Create a file called test.txt with 'hello'" → triggers `write_file` tool → HITL approval → file created
  - "Search the web for Rust 2026 edition" → triggers `web_search` tool → returns results

### Verification
- [ ] User asks a question requiring a tool → tool is called and result appears in chat
- [ ] Dangerous operations prompt HITL approval dialog
- [ ] Multi-step reasoning works (e.g., "Find the largest file in ~/Downloads and tell me what it is")

### Verification
- [ ] User asks a question requiring a tool → tool is called and result appears in chat
- [ ] Dangerous operations prompt HITL approval dialog
- [ ] Multi-step reasoning works (e.g., "Find the largest file in ~/Downloads and tell me what it is")

---

## Phase 0.5 — Python Sidecar & Pre-Cognitive Bridge

> **Goal:** Establish the Rust↔Python bridge that powers all heavy-duty AI/ML preprocessing. The Python sidecar "pre-digests" raw data (images, documents, code, web pages) into clean, structured JSON context before the LLM ever sees it. This is the **LLM Pressure Relief** layer.

**Priority:** HIGH — unlocks deep preprocessing for Phases 3, 4, 5, 12. Start in parallel with Phase 0.

### Why a Python Sidecar?

The existing `preprocessing/` module in Rust has basic implementations:
- `image.rs` — metadata only (no OCR, no feature extraction)
- `document.rs` — delegates to `pdftotext` CLI and `pandoc` CLI 
- `code.rs` — regex-based function/import extraction (~25 languages)
- `web.rs` — basic HTML text extraction via `scraper` crate

These are acceptable fast-path handlers for simple files. But for deep analysis, Python's ecosystem is unmatched:
- **OpenCV** for image analysis vs. Rust's `image` crate (limited to metadata)
- **PyMuPDF** for PDF parsing vs. shelling out to `pdftotext` (loses tables, structure)
- **tree-sitter** Python bindings for AST parsing vs. regex patterns (misses edge cases)
- **sentence-transformers** for real embeddings vs. the hash-based stub in `embeddings.rs`

The architecture keeps both: **Rust handles the fast path** (plain text, basic metadata) and **Python handles the deep path** (OCR, table extraction, AST, embeddings).

### Steps

- [x] **0.5.1** Create `kria-modules/` Python package
  - Create `kria-modules/pyproject.toml` with `uv`-managed dependencies
  - Core deps: `pymupdf`, `pillow`, `opencv-python-headless`, `pytesseract`, `tree-sitter`, `sentence-transformers`, `trafilatura`, `pandas`
  - Optional heavy deps (only on Performance/High tier): `easyocr`, `torch`
  - Entry point: `kria_modules.bridge:main`
  - Target Python 3.11+

- [x] **0.5.2** Implement `kria_modules/bridge.py` — JSON-RPC stdio dispatcher
  - Read JSON-RPC 2.0 requests from stdin, write responses to stdout
  - Method routing: `"image.analyze"` → `processors.image`, `"document.extract"` → `processors.document`, etc.
  - Stderr reserved for logging (never mixed with RPC responses)
  - Built-in methods: `health_check`, `configure_tier`, `list_capabilities`, `shutdown`
  - Graceful error handling: exceptions → JSON-RPC error response (never crash the process)
  - Heartbeat: respond to `ping` with `pong` + uptime + memory usage

- [x] **0.5.3** Implement `processors/image.py` — Image preprocessing
  - `image.analyze(file_path, operations, tier, max_tokens)` → structured JSON:
    - `metadata`: width, height, format, size, EXIF
    - `ocr_text`: extracted text (pytesseract on Standard+, easyocr on Performance+)
    - `features`: dominant colors, scene type (screenshot/photo/diagram/chart), text density, edge density
    - `thumbnail_base64`: resized image for vision model (respects tier: 512px Lite, 1024px Standard, full Performance+)
  - Tier-aware: Lite returns metadata only; High returns full feature vectors + multi-engine OCR

- [x] **0.5.4** Implement `processors/document.py` — Document extraction
  - `document.extract(file_path, operations, tier, max_tokens)` → structured JSON:
    - PDF via PyMuPDF: page-by-page text, table detection, image extraction, section headings, metadata
    - DOCX via python-docx: paragraphs, tables, headers, styles
    - CSV/Excel via pandas: schema, column types, summary statistics, row count, sample rows
    - Markdown/plaintext: pass through with section detection
  - Token budget: if extracted text exceeds `max_tokens`, intelligent truncation (keep first/last pages + section headings)
  - Tier-aware: Lite extracts first 5 pages plaintext only; High extracts full document with table structure

- [x] **0.5.5** Implement `processors/code.py` — AST-level code analysis
  - `code.analyze(file_path, operations, tier)` → structured JSON:
    - `language`: detected language (extension + content heuristics)
    - `ast_structure`: functions, classes, methods with signatures and line ranges (via tree-sitter)
    - `imports`: dependency list with resolved module names
    - `metrics`: LOC, comment ratio, complexity (cyclomatic via AST)
    - `dependency_graph`: call graph between functions (Performance+ only)
  - Tier-aware: Lite uses regex (existing Rust code.rs behavior); Standard uses tree-sitter; Performance adds semantic analysis

- [x] **0.5.6** Implement `processors/web.py` — Web content extraction
  - `web.extract(url_or_html, operations, tier)` → structured JSON:
    - `article_text`: clean article body (via trafilatura or readability-lxml)
    - `title`, `author`, `date`, `description`
    - `links`: extracted with anchor text and href
    - `tables`: if any data tables detected
  - Handles: JavaScript-heavy pages (fallback: return basic HTML extraction), paywalls (readability mode), PDFs (auto-detect Content-Type → route to document processor)
  - Tier-aware: Lite returns title + first 500 words; High returns full extraction

- [x] **0.5.7** Implement `processors/embeddings.py` — Embedding generation
  - `embeddings.embed_text(text)` → `[float...]` (384-dim vector)
  - `embeddings.embed_batch(texts)` → `[[float...], ...]` (batch mode)
  - `embeddings.chunk_and_embed(text, chunk_size, overlap)` → `{chunks: [...], vectors: [[...], ...]}`
  - Model: `all-MiniLM-L6-v2` via sentence-transformers (~22MB)
  - Tier-aware: Lite uses Rust-side `ort` embeddings (no Python); Standard uses CPU sentence-transformers; Performance uses GPU
  - This replaces the fake hash-based embeddings in `memory/embeddings.rs`

- [x] **0.5.8** Create `crates/kria-core/src/sidecar/` — Rust bridge module
  - `bridge.rs`: `SidecarBridge` struct
    - `spawn()` → launches Python process: `uv run python -m kria_modules.bridge`
    - `request(method, params)` → async: write JSON-RPC to stdin, read response from stdout
    - `health_check()` → verify sidecar is alive
    - `configure_tier(tier)` → send hardware tier config so Python adapts processing depth
    - `shutdown()` → graceful stop
  - `protocol.rs`: JSON-RPC 2.0 request/response types (serde-serializable)
  - `health.rs`: heartbeat loop (every 30s), crash detection, auto-restart (max 3 retries)
  - `tier_config.rs`: map `HardwareTier` → Python quality presets

- [x] **0.5.9** Wire `SidecarBridge` into `AppState`
  - Spawn sidecar during `init_runtime()` after hardware tier detection
  - Store as `Arc<SidecarBridge>` in `AppState`
  - On app shutdown, call `sidecar.shutdown()`
  - Emit `sidecar.ready` event via EventBus when health check passes

- [x] **0.5.10** Register pre-cognitive tools in `ToolRegistry`
  - Create `tools/precognitive.rs` with tool handlers that delegate to `SidecarBridge`:
    - `ImageAnalyze` → `sidecar.request("image.analyze", ...)`
    - `DocumentExtract` → `sidecar.request("document.extract", ...)`
    - `CodeAnalyzeAst` → `sidecar.request("code.analyze", ...)`
    - `WebExtractArticle` → `sidecar.request("web.extract", ...)`
    - `EmbeddingsGenerate` → `sidecar.request("embeddings.embed_text", ...)`
  - All registered as GREEN tier (pre-processing is read-only)
  - Fallback: if sidecar is unavailable, fall back to existing Rust `preprocessing/` modules
  - Register in `build_default_registry()` alongside existing tools

- [x] **0.5.11** Add auto-routing in agent loop for file-bearing messages
  - When a tool returns binary/file output (e.g., `read_file` returns a PDF path), the agent loop should auto-detect file type and pre-process via sidecar before feeding to LLM
  - `loop_engine.rs`: after tool execution, check if result contains `file_path` + mime type → if binary, route through `SidecarBridge` → inject structured context instead of raw bytes
  - This is the **Mediator Pattern** in action: raw data never reaches the LLM unprocessed

- [x] **0.5.12** Create `scripts/setup_python.sh` — Python environment setup
  - Check for `uv` installation, install if missing: `curl -LsSf https://astral.sh/uv/install.sh | sh`
  - Create venv: `cd kria-modules && uv sync`
  - Verify sidecar starts: `uv run python -m kria_modules.bridge --selftest`
  - Download optional models: `all-MiniLM-L6-v2` for sentence-transformers
  - Tier-aware: skip heavy deps (easyocr, torch) on Lite/Standard tier

### Verification
- [ ] `uv run python -m kria_modules.bridge --selftest` → passes all module checks
- [ ] Rust spawns sidecar → sends `health_check` → receives `ready`
- [ ] Upload a PNG → `image.analyze` → returns metadata + OCR text + scene classification
- [ ] Upload a PDF → `document.extract` → returns structured text with section headings
- [ ] Python crash → Rust detects via heartbeat → auto-restarts → pending requests retry
- [ ] On Lite tier: sidecar starts with minimal deps, returns basic (not deep) analysis

### Technology
| Component | Tool | License |
|-----------|------|---------|
| Python pkg manager | `uv` | MIT/Apache 2.0 |
| IPC protocol | JSON-RPC 2.0 over stdio | — |
| Image processing | OpenCV + Pillow | BSD-3/MIT-like |
| OCR | pytesseract + easyocr | Apache 2.0/Apache 2.0 |
| PDF extraction | PyMuPDF (fitz) | AGPL-3.0 (or commercial) |
| DOCX extraction | python-docx | MIT |
| CSV/Excel | pandas | BSD-3 |
| AST parsing | tree-sitter (Python bindings) | MIT |
| Web extraction | trafilatura + readability-lxml | Apache 2.0/Apache 2.0 |
| Embeddings | sentence-transformers + all-MiniLM-L6-v2 | Apache 2.0 |
| Rust IPC | tokio::process + serde_json | MIT |

### Bridging to Existing Stubs

| Existing Rust Stub | Becomes (After Phase 0.5) |
|---------------------|---------------------------|
| `preprocessing/image.rs` | **Fast path**: basic metadata via `image` crate. **Deep path**: delegates to Python `image.analyze` |
| `preprocessing/document.rs` | **Fast path**: plain text files read natively. **Deep path**: PDF/DOCX via Python `document.extract` |
| `preprocessing/code.rs` | **Fast path**: regex extraction (existing). **Deep path**: tree-sitter AST via Python `code.analyze` |
| `preprocessing/web.rs` | **Fast path**: basic HTML stripping. **Deep path**: article extraction via Python `web.extract` |
| `memory/embeddings.rs` | **Real-time**: Rust `ort`/`fastembed-rs` for query embedding. **Batch**: Python `embeddings.embed_batch` for RAG ingestion |
| `tools/documents.rs` | Registers `DocumentExtract` tool that calls sidecar |
| `plugin/runtime.rs` | Extended with `SkillRegistry` that discovers Python skills |

---

## Phase 1 — Persistent Memory & Chat History

> **Goal:** The assistant remembers conversations, learns user preferences, and builds a personal knowledge base.

### Steps

- [x] **1.1** Wire `MemoryStore` into `AppState`
  - Pass the `MemoryStore` instance into `AppState` (currently created but discarded)
  - Share as `Arc<MemoryStore>` so agent loop and commands can both access it

- [x] **1.2** Implement chat history persistence
  - On every `send_message`: save user message + assistant response to SQLite via `MemoryStore`
  - Tag messages with session_id and timestamp
  - Implement `get_session_history` command to actually query SQLite

- [x] **1.3** Implement session management
  - New Tauri commands: `create_session`, `list_sessions`, `delete_session`, `switch_session`
  - Each session has: id, title (auto-generated from first message), created_at, message_count
  - Current session_id stored in AppState

- [x] **1.4** Implement conversation context loading
  - When sending a message, load last N messages from current session (configurable, default 20)
  - Feed them to the agent loop as conversation history
  - Context window management: trim oldest messages when approaching token limit

- [x] **1.5** Replace fake embeddings with real ones
  - **Real-time path (Rust)**: Integrate `ort` crate (ONNX Runtime) with `all-MiniLM-L6-v2` model (~22MB)
  - Used for: query embedding during conversation (low latency, single vector)
  - Replace `EmbeddingModel::embed()` hash-based stub with real ONNX inference (+ fallback)
  - **Batch path (Python sidecar)**: Use `sentence-transformers` via `SidecarBridge::embeddings.embed_batch()`
  - Used for: RAG ingestion (many documents → many vectors at once, GPU-accelerated on Performance+ tier)
  - This dual-path approach enables semantic search over facts and knowledge

- [x] **1.6** Wire up knowledge tools to memory layer
  - `remember_fact` → `MemoryStore::store_fact()` with real persistence
  - `recall_fact` → `MemoryStore::search_facts()` via FTS5 + access tracking
  - `search_knowledge` → hybrid FTS5 facts + conversation search
  - `save_snippet`, `get_snippet`, `list_snippets` → real SQLite CRUD
  - `ingest_document` → reads file, stores as fact with truncation
  - All tools use injected `StoreHandle(Arc<MemoryStore>)` pattern

- [x] **1.7** Implement automatic fact extraction
  - After each conversation, run FactManager::extract_from_turn() in event consumer
  - Pattern: "My name is X", "I prefer Y", "I work at Z", "My project uses A"
  - Store extracted facts with source attribution (which conversation, when)
  - Deduplication via vector similarity (0.92 threshold)
  - Feed relevant facts into system prompt for personalization

- [x] **1.8** Frontend: Session sidebar
  - Wire `SessionSidebar.tsx` to real Tauri commands (list_sessions, switch_session, create_session, delete_session)
  - Click to switch sessions → loads history → renders messages
  - New session button, delete session with × button
  - Auto-title from first message, auto-refresh on agent:done
  - Store exports: loadSessions, createSession, switchSession, deleteSession, renameSession

### Verification
- [ ] Close app, reopen → previous conversations are visible
- [ ] "What did I ask you about yesterday?" → retrieves from history
- [ ] "Remember that my favorite language is Rust" → stored, recalled in future sessions
- [ ] Session sidebar shows real conversation list

### Technology
| Component | Tool | License |
|-----------|------|---------|
| Database | SQLite via `rusqlite` (already in use) | Public Domain |
| Embeddings (real-time) | `ort` or `fastembed-rs` + `all-MiniLM-L6-v2` ONNX | Apache 2.0 |
| Embeddings (batch/RAG) | Python `sentence-transformers` via sidecar | Apache 2.0 |
| Vector search | `usearch` crate or existing brute-force (upgrade later) | Apache 2.0 |

---

## Phase 2 — Internet, Search & Real-Time Access

> **Goal:** The assistant can access the internet, search the web, fetch live data, and answer questions about current events.

### Steps

- [x] **2.1** Verify internet tools work standalone
  - Existing tools: `web_search` (DuckDuckGo), `fetch_webpage`, `download_file`, `check_url_status`, `get_public_ip`, `ping_host`, `dns_lookup`, `speed_test`
  - All registered in tool registry and available to agent loop
  - Added fallback search via SearXNG (step 2.2)

- [x] **2.2** Add SearXNG integration as primary search backend
  - `searxng_search` tool queries any SearXNG instance (local or remote)
  - Returns structured JSON results (title, URL, snippet, engine)
  - Added `SearchConfig` to `KriaConfig`: `engine`, `searxng_url`, `news_feeds`
  - Configurable in `config/default.toml`: `[search] engine = "searxng"`

- [x] **2.3** Improve web page fetching
  - Existing `fetch_webpage` strips HTML, extracts body text, respects max_chars
  - For deep extraction, Python sidecar `web.extract` provides trafilatura-based article extraction (Phase 0.5)
  - Rate limiting via reqwest timeout (15s)

- [x] **2.4** Add time/date awareness
  - System prompt already includes `Current Date/Time: {datetime}` via `chrono::Local::now()`
  - Added `get_current_time` tool for explicit timezone conversions
  - Supports: UTC, EST, CST, MST, PST, CET, JST, IST, PKT, AEST, and numeric offsets

- [x] **2.5** Add weather, news, and information tools
  - `get_weather` — Open-Meteo API (free, no key): geocoding → forecast with WMO weather code decoding
  - `get_news` — RSS/Atom feed parser with configurable feed URLs, extracts title/link/description
  - `get_exchange_rate` — open.er-api.com (free): currency conversion with rate + amount
  - `calculate` — Safe recursive-descent math evaluator: +, -, *, /, %, ^, sqrt, abs, sin, cos, tan, log, ln, pi, e, ceil, floor, round. Rejects code injection.

- [x] **2.6** Wire internet tools into the agent loop's tool registry
  - All 14 internet tools registered via `build_default_registry()` (8 original + 6 new Phase 2)
  - Agent can now answer: "What's the weather in Berlin?", "Search for Rust async patterns", "What time is it in Tokyo?"

### Verification
- [ ] "Search the web for latest Rust news" → real search results returned and summarized
- [ ] "What's the weather in New York?" → real weather data
- [ ] "Fetch and summarize this URL: ..." → actual page content
- [ ] "What time is it in Tokyo?" → correct answer

### Technology
| Component | Tool | License |
|-----------|------|---------|
| Meta-search | SearXNG (self-hosted or public instance) | AGPL-3.0 |
| Weather | Open-Meteo API (free, no key) | CC BY 4.0 |
| Math | Custom safe recursive-descent evaluator | — |
| HTTP client | `reqwest` (already in use) | MIT/Apache 2.0 |
| Currency | open.er-api.com (free, no key) | — |

---

## Phase 3 — File & System Intelligence

> **Goal:** The assistant can intelligently browse, search, read, write, and manage files and system resources.

### Steps

- [x] **3.1** Verify file tools work through agent loop
  - All 12 original file tools (read, write, list, search, info, dir_size, create_dir, rename, copy, delete, delete_dir, move) registered and functional
  - PolicyEngine correctly classifies: read_file=GREEN, write_file=YELLOW, delete_file=RED
  - Protected path escalation: writing to /etc → RED + approval required
  - 26 passing tests cover file tool registration/execution

- [x] **3.2** Enhance file search with content awareness
  - `search_file_contents` — grep-like full-text search across files with context lines, case-insensitive by default, configurable context_lines
  - `find_files_by_pattern` — glob file finder with min_size/max_size/type filters (file/dir/any)
  - `get_project_structure` — tree-like directory structure with configurable depth, skips node_modules/target/.git/etc.
  - All three registered as GREEN tier in file_ops category

- [x] **3.3** Add code intelligence tools
  - `analyze_code` — Wires `preprocessing/code.rs` CodeProcessor into tool: detects language (25+ extensions), extracts functions/imports via regex
  - `count_lines_of_code` — Recursive LOC breakdown by language using walkdir + CodeProcessor::detect_language
  - `diff_files` — Line-by-line comparison of two files, returns differences with line numbers (limited 100 diffs)
  - `find_todos` — Scans for TODO/FIXME/HACK/XXX/BUG/OPTIMIZE/REFACTOR patterns with file/line/tag/message
  - All four registered as GREEN tier in code_intelligence category
  - Deep path via Python sidecar (code.analyze_ast with tree-sitter) available through precognitive auto-routing

- [x] **3.4** Add document understanding (via Python sidecar)
  - Unified `parse_document` tool replaces separate parse_pdf/parse_docx — auto-detects format by extension
  - **Sidecar-first**: For PDF/DOCX/XLSX, tries `SidecarBridge::document.extract(file, operations=["text", "tables", "sections"])` for structured extraction
  - **Fallback chain**: PDF→pdftotext CLI, DOCX→pandoc CLI, text formats→native read
  - `parse_csv` — Enhanced with column detection, row count, sample rows (native) or sidecar pandas analysis
  - `summarize_document` — Sidecar path extracts sections + word count; native path provides basic stats + preview
  - Backend field in all responses identifies extraction method (sidecar/native/pdftotext/pandoc)

- [x] **3.5** Verify HITL approval flow for destructive operations
  - PolicyEngine: GREEN (read-only) → auto-execute, YELLOW (write) → auto-execute, RED (destructive) → HITL approval required
  - BlacklistChecker: ~25 regex patterns block dangerous commands (rm -rf /, mimikatz, reverse shells, crypto miners)
  - Protected path patterns: /etc, /usr, /var, /boot, /sys, /proc, /root auto-escalate to RED
  - HitlGateway: mpsc broadcast + oneshot per request, configurable timeout, cancel_all emergency stop
  - AuditLogger: SQLite-backed, logs every action with policy decision, execution result, and timing

- [x] **3.6** Add clipboard intelligence
  - `get_clipboard`, `set_clipboard`, `transform_clipboard` (uppercase/lowercase/trim/reverse/snake_case/title_case) all wired
  - `screenshot` — Linux support with gnome-screenshot/scrot/imagemagick fallback chain
  - `type_text` — xdotool keyboard simulation (Linux only)
  - Risk tiers: get/transform=GREEN, set/type=YELLOW

- [x] **3.7** Frontend: Tool execution feedback
  - `agent:tool_call` event listener → adds running tool call to last assistant message's toolCalls array
  - `agent:tool_result` event listener → updates tool call status to done/error with result preview
  - `agent:approval_result` listener → marks denied tools with "denied" status
  - Collapsible `ToolCallBlock` component: status icon (⏳/✅/❌/🚫), name, args preview, expandable details
  - Color-coded left borders: running=accent pulse, done=green, error=red, denied=orange
  - Tool calls shown above message content for better flow visibility

### Verification
- [ ] "What's in my Downloads folder?" → real directory listing
- [ ] "Find all Python files in this project" → real file search
- [ ] "Delete test.txt" → safety prompt → user approves → file deleted
- [ ] "Read and summarize this PDF" → content extracted and summarized

---

## Phase 4 — Vision & Multimodal

> **Goal:** The assistant can see images (screenshots, uploaded photos, clipboard images) and reason about them.

### Steps

- [x] **4.1** Add image upload to frontend
  - File input button (📎) in chat area with `accept="image/*"`
  - Drag-and-drop support on the chat window (visual feedback with blue dashed outline)
  - Paste image from clipboard (Ctrl+V intercepts `clipboardData.items`)
  - Show image thumbnail in the message bubble (`message-image-thumb`, max 300×200)
  - Pending image preview bar with remove button before sending

- [x] **4.2** Implement image-to-LLM pipeline (with Pre-Cognitive preprocessing)
  - `ImageAttachment` struct: `{ data: String (base64), mime_type: String }`
  - `ChatMessage.images: Option<Vec<ImageAttachment>>` field added
  - `has_images()` method for detection; `to_multimodal_content()` produces OpenAI multimodal format
  - `[{"type": "text", "text": ...}, {"type": "image_url", "image_url": {"url": "data:...;base64,..."}}]`
  - All 9 `ChatMessage` constructors across 3 files updated with `images: None`
  - `ImageProcessor::to_base64(path)` for encoding; `thumbnail(path, max_dim)` for resizing

- [x] **4.3** Add screenshot tool for self-initiated vision
  - `screenshot_analyze` tool: captures screen → runs OCR → returns text + path
  - Tries sidecar `image.analyze(file, ["ocr"])` → falls back to tesseract CLI
  - Chains with existing `screenshot` tool in `interaction.rs`

- [x] **4.4** Add model routing for vision
  - `ModelRouter.vision_local: Option<Arc<dyn LlmBackend>>` field
  - `from_config()` scans `config.llm.models` for `capabilities.contains("vision") && mmproj_file.is_some()`
  - `route_vision()` → vision_local → cloud fallback → local text model
  - `has_vision()` check method
  - `AgentLoop.run()` detects `messages.last().has_images()` → routes to `route_vision()`

- [x] **4.5** Add OCR capabilities (via Python sidecar)
  - `ocr_image` tool: sidecar `image.analyze(file, ["ocr"])` → tesseract CLI fallback
  - `analyze_image` tool: metadata + OCR + features via sidecar, native fallback
  - `VisionSidecar` wrapper with `try_ocr()` and `try_analyze()` async methods
  - Vision tools module (`tools/vision.rs`) with 3 tools registered in `vision` category

- [x] **4.6** Add Tauri command for image message
  - `send_image_message(image_data: Vec<u8>, mime_type: String, text: Option<String>)`
  - MIME type validation (png/jpeg/gif/webp/bmp only), 10 MB size limit
  - Stores image in `~/.kria/attachments/` with hash-based filename
  - Base64-encodes via `ImageProcessor::to_base64()` for LLM pipeline
  - Full agent loop integration with event streaming (same pattern as `send_message`)
  - Auto-title with 📷 prefix for image sessions

### Verification
- [x] Drag image into chat → "What's in this image?" → accurate description
- [x] Paste screenshot → "Explain this error message" → reads text from screenshot
- [x] "Take a screenshot and tell me what apps are open" → works end-to-end

### Technology
| Component | Tool | License |
|-----------|------|---------|
| Vision LLM | Qwen2.5-VL-7B via llama.cpp (already downloaded) | Apache 2.0 |
| Multimodal projection | mmproj-F16.gguf (already downloaded) | Apache 2.0 |
| Image preprocessing | OpenCV + Pillow (Python sidecar) | BSD-3/MIT-like |
| OCR (Standard tier) | pytesseract (Python sidecar) | Apache 2.0 |
| OCR (Performance+ tier) | easyocr (Python sidecar, GPU) | Apache 2.0 |
| OCR fallback | Tesseract CLI | Apache 2.0 |
| Image metadata | `image` crate (Rust, existing) | MIT |

---

## Phase 5 — Voice Pipeline

> **Goal:** Full voice interaction — speak to the assistant, hear it respond. Push-to-talk and hands-free modes.

### Steps

- [x] **5.1** Wire voice capture to STT
  - `VoicePipeline` orchestrator (`voice/pipeline.rs`) wires `AudioCapture::start()` → `VoiceActivityDetector` → buffer speech segments
  - Capture runs on dedicated `std::thread` (cpal::Stream is !Send), forwards chunks via mpsc channel to async VAD→STT task
  - When VAD detects end-of-speech, sends audio buffer to `SpeechToText::transcribe_samples()`
  - Safety limits: max 30s speech buffer, minimum 0.3s speech length, "[BLANK_AUDIO]" filtered

- [x] **5.2** Wire STT output to agent loop
  - Transcribed text emitted as `VoicePipelineEvent::Transcript(String)`
  - `start_voice` command spawns event consumer that forwards transcripts to full agent loop (system prompt + history + tools)
  - Frontend receives `voice:transcript` event, creates 🎤-prefixed user message

- [x] **5.3** Wire agent response to TTS
  - Event consumer collects agent response, calls `pipeline.speak(response_text)`
  - `speak()` runs `TextToSpeech::synthesize_samples()` → `AudioPlayer::play_samples()`
  - Pipeline state: Speaking → Listening (if capture active) or Idle

- [x] **5.4** Implement proper voice commands in `commands.rs`
  - `start_voice`: Starts VoicePipeline, spawns event consumer with full agent loop integration
  - `stop_voice`: Signals capture thread to stop, sends stop to processing task, emits idle state
  - `get_voice_status`: Returns JSON `{ active: bool, state: VoicePipelineState }`
  - `which_binary()` helper finds whisper-cpp/piper on PATH
  - Pipeline state machine: Idle → Listening → Processing → Speaking → Listening

- [x] **5.5** Download/verify voice models
  - whisper.cpp `ggml-base.en.bin` present in `models/stt/`
  - Piper `en_US-lessac-high.onnx` + config present in `models/piper/`
  - Silero VAD download added to `scripts/download_models.py` (COMMON_DOWNLOADS)

- [x] **5.6** Frontend: Voice UI
  - `voiceState` signal in stores/app.ts: "idle" | "listening" | "processing" | "speaking"
  - VoiceOverlay.tsx: state-specific labels (Listening.../Processing.../Speaking...), dynamic icons (🎤/🔊), volume bar
  - ChatView.tsx: mic button shows state-specific colors and icons
  - CSS: `.voice-state-listening` (pulsing green), `.voice-state-processing` (amber), `.voice-state-speaking` (blue accent)
  - Event listeners: `voice:state`, `voice:transcript`, `voice:error`

- [x] **5.7** Upgrade VAD
  - `VoiceActivityDetector::with_silero()` loads Silero VAD ONNX model via `ort` crate
  - Falls back to energy-based detection if model not found or load fails
  - `is_using_silero()` method to check which backend is active
  - LSTM state (h, c) maintained across inferences for temporal context
  - Processes 512-sample windows, returns max speech probability
  - Pipeline passes `vad_model_path` to capture task via `with_vad_model()` builder

### Verification
- [x] Click mic button → speak question → assistant transcribes → responds with text + voice
- [ ] Push-to-talk (hold hotkey) → release → processes speech
- [ ] "Hey KRIA" wake word detection (stretch goal)

### Technology
| Component | Tool | License |
|-----------|------|---------|
| STT | whisper.cpp / `whisper-rs` | MIT |
| TTS | Piper / `piper-rs` | MIT |
| VAD | Silero VAD ONNX | MIT |
| Audio I/O | CPAL + rodio (already in use) | Apache 2.0 |
| ONNX Runtime | `ort` crate | MIT |

---

## Phase 6 — Settings, API Keys & UI Overhaul

> **Goal:** Professional-looking, feature-rich UI with real settings management, API key input, themes, and interactivity.

### Steps

- [ ] **6.1** Wire settings modal to real config
  - `get_settings` already works — connect it to UI form fields on modal open
  - `update_settings` already works — save form values on "Save" button
  - Persist changes to disk: write updated TOML to `~/.kria/config.toml`
  - Restart relevant subsystems on config change (e.g., switch LLM backend)

- [ ] **6.2** Add API key management
  - Dedicated field in Settings for Gemini API key
  - Store securely: use system keychain via `keyring` crate (or `tauri-plugin-stronghold`)
  - Never display full key after save (mask with `****...last4`)
  - "Test Connection" button → tries a simple LLM call → shows success/failure
  - Environment variable override still works (`KRIA_CLOUD_API_KEY`)

- [ ] **6.3** Add model management UI
  - Tab in Settings: list available models (scanned from `models/` directory)
  - Show: model name, size, quantization, capabilities (text/vision/code)
  - Select active model for chat / vision / code tasks
  - Download model button (using `ModelManager::download()` which already supports resumable downloads)
  - Delete model button

- [ ] **6.4** Redesign chat interface
  - **Markdown rendering** in messages: headers, bold, italic, code blocks with syntax highlighting, lists, tables, links
  - Use `solid-markdown` or a lightweight markdown-to-HTML renderer
  - Code blocks: copy button, language label, syntax highlighting (highlight.js or Shiki)
  - Math rendering: KaTeX for LaTeX expressions
  - Message actions: copy, regenerate, edit & resend, delete

- [ ] **6.5** Add rich input area
  - Expandable textarea (auto-grow)
  - File attachment button (images, documents)
  - Voice input button
  - Keyboard shortcuts: Enter to send, Shift+Enter for newline, Ctrl+/ for commands
  - Slash commands: `/clear`, `/session new`, `/model phi-4`, `/voice on`

- [ ] **6.6** Improve layout and navigation
  - Collapsible sidebar (sessions list + navigation)
  - Top bar: model selector dropdown, connection status indicator, token count
  - Bottom status bar: LLM backend status (connected/disconnected), memory usage, active tools
  - Responsive design: handle window resizing gracefully

- [ ] **6.7** Add dark/light themes
  - Theme toggle in settings (already `ui.theme` in config)
  - CSS variables for all colors
  - System theme auto-detection

- [ ] **6.8** Add keyboard shortcuts overlay
  - `Ctrl+K` or `?` → shows all available shortcuts
  - `Ctrl+N` → new session
  - `Ctrl+L` → clear current view
  - `Ctrl+,` → open settings
  - `Ctrl+Shift+V` → toggle voice

- [ ] **6.9** Add notification system
  - Toast notifications for: tool execution complete, HITL approval needed, voice transcript ready, errors
  - Use `tauri-plugin-notification` for OS-level notifications when app is minimized
  - In-app notification center for history

### Verification
- [ ] Settings modal shows current config values, saves changes, persists across restarts
- [ ] API key can be entered, tested, saved securely
- [ ] Markdown messages render with syntax highlighted code blocks
- [ ] Theme toggle works, persists across sessions
- [ ] File upload works from the chat input area

### Technology
| Component | Tool | License |
|-----------|------|---------|
| Markdown | `solid-markdown` or `marked` + `DOMPurify` | MIT |
| Syntax highlighting | `highlight.js` or `Shiki` | BSD-3 / MIT |
| Math rendering | KaTeX | MIT |
| Keychain | `keyring` crate or `tauri-plugin-stronghold` | MIT / Apache 2.0 |
| Icons | Lucide Icons (already SolidJS compatible) | ISC |

---

## Phase 7 — MCP Protocol & Plugin Ecosystem

> **Goal:** Allow external tools, services, and community plugins to extend the assistant via the Model Context Protocol.

### Steps

- [ ] **7.1** Implement MCP client in `kria-core`
  - Create `crates/kria-core/src/mcp/` module
  - Implement MCP client that connects to MCP servers via stdio or HTTP/SSE
  - Parse MCP tool definitions → convert to internal `ToolDef` format
  - Support MCP resources (read-only data sources) and prompts (template injection)

- [ ] **7.2** Add MCP server configuration
  - Config section in `config.toml`:
    ```toml
    [[mcp.servers]]
    name = "filesystem"
    command = "npx"
    args = ["-y", "@anthropic/mcp-filesystem-server", "/home/user"]

    [[mcp.servers]]
    name = "github"
    command = "npx"  
    args = ["-y", "@anthropic/mcp-github-server"]
    env = { GITHUB_TOKEN = "..." }
    ```
  - Auto-start configured MCP servers on app launch
  - Health monitoring: restart crashed servers

- [ ] **7.3** Register MCP tools in the tool registry
  - On MCP server connection, pull its tools list
  - Register each as a tool in `ToolRegistry` with proper schemas
  - Agent loop can now invoke MCP tools alongside built-in tools
  - Prefix MCP tools: `mcp_filesystem_read_file` to avoid name collisions

- [ ] **7.4** Frontend: MCP server management
  - Settings tab: "Connected Services"
  - List configured MCP servers with status (running/stopped/error)
  - Add/remove/edit server configurations
  - View available tools per server
  - Enable/disable individual servers

- [ ] **7.5** Build example Python MCP server
  - Create `plugins/example-mcp-server/` with a simple Python MCP server
  - Demonstrates how developers can extend KRIA with custom tools
  - Include: a web scraper tool, a database query tool, a custom API integration
  - Document the MCP server development workflow

- [ ] **7.6** Plugin marketplace foundation (stretch)
  - Plugin manifest format: `plugin.json` with name, version, description, MCP server command
  - Plugin directory: `~/.kria/plugins/`
  - Discovery: load plugins from directory on startup
  - Install from URL: download, verify checksum, register

### Verification
- [ ] Configure an MCP filesystem server → agent can read/write files through it
- [ ] Add a custom Python MCP server → its tools appear in the agent's capabilities
- [ ] MCP server crash → auto-restart → tools still accessible

### Technology
| Component | Tool | License |
|-----------|------|---------|
| MCP protocol | Custom implementation (JSON-RPC 2.0 over stdio) | — |
| MCP servers | `@anthropic/mcp-*` npm packages (reference implementations) | MIT |
| Process management | `tokio::process` (already available) | MIT |

---

## Phase 7.5 — Skill-Plugin Ecosystem (Python)

> **Goal:** Allow community-developed Python "skills" to extend KRIA's capabilities. Each skill is a self-contained Python package with its own dependencies, isolated in its own virtual environment. Skills register as callable tools in the agent loop.

**Depends on:** Phase 0.5 (Python sidecar must be running)

### Steps

- [ ] **7.5.1** Define skill manifest format (`skill.json`)
  - Required fields: `name`, `version`, `description`, `methods[]`, `python_requires`
  - Each method: `name`, `description`, `parameters` (JSON Schema), `returns`, `safety_tier`
  - Optional: `min_tier` (hardware requirement), `author`, `license`, `homepage`
  - Store in `~/.kria/skills/{skill_name}/skill.json`

- [ ] **7.5.2** Implement `SkillRegistry` in `crates/kria-core/src/plugin/skill_registry.rs`
  - `discover_skills(skills_dir)` → scan `~/.kria/skills/` for directories with `skill.json`
  - Validate manifests, check `min_tier` against current hardware
  - For each skill: verify `.venv/` exists, else run `uv sync` to create it
  - Register each skill's methods as tools in `ToolRegistry` with prefix `skill_`
  - Example: skill "translator" with method "translate" → tool name `skill_translator_translate`

- [ ] **7.5.3** Implement skill invocation in Python sidecar
  - `kria_modules/skills/loader.py`: discover skills, import their handlers
  - Each skill handler must implement: `def process(method: str, params: dict) -> dict`
  - Skills run in their own venv — sidecar activates the correct venv per invocation
  - IPC method: `skill.invoke(skill_name, method_name, params)` → routes to correct handler

- [ ] **7.5.4** Implement dependency isolation via `uv`
  - Each skill has its own `pyproject.toml` and `.venv/`
  - `uv sync --directory ~/.kria/skills/{skill_name}` creates isolated environment
  - Skill A can use `pymupdf==1.24` while Skill B uses `pymupdf==1.23` — no conflicts
  - Rust manages venv lifecycle: create on install, delete on uninstall

- [ ] **7.5.5** Implement skill install/uninstall commands
  - `install_skill(url)` → git clone or download + extract → validate manifest → create venv → register
  - `uninstall_skill(name)` → deregister tools → delete directory + venv
  - `list_skills()` → return installed skills with status (active/disabled/error)
  - `enable_skill(name)` / `disable_skill(name)` → toggle without uninstalling
  - All skill install/uninstall requires HITL approval (RED tier) — skills can contain arbitrary code

- [ ] **7.5.6** Frontend: Skill management panel
  - Settings tab: "Skills" — list installed skills with name, version, description, status
  - Install button (paste URL or browse marketplace)
  - Per-skill toggles, uninstall button, "View tools" accordion
  - Show skill resource usage and last invocation time

- [ ] **7.5.7** Build example skills
  - `skills/summarizer/`: Multi-strategy text summarization (extractive + abstractive)
  - `skills/translator/`: LLM-powered translation with language detection
  - `skills/code-reviewer/`: AI-assisted code review with style/bug/security checks
  - Each with full `skill.json`, `pyproject.toml`, and `handler.py`
  - Document the skill development workflow in `docs/SKILL_DEVELOPMENT.md`

### Verification
- [ ] Drop a skill folder into `~/.kria/skills/` → restart → new tools appear in agent capabilities
- [ ] "Use the translator skill to translate this to Spanish" → skill method invoked successfully
- [ ] Skill with conflicting deps doesn't affect core sidecar or other skills
- [ ] Install skill from URL → HITL approval → skill installed and available

### Technology
| Component | Tool | License |
|-----------|------|---------|
| Dependency isolation | `uv` (per-skill venvs) | MIT/Apache 2.0 |
| Skill runtime | Python subprocess or dynamic import | — |
| Manifest format | JSON Schema | — |
| Skill discovery | Filesystem scanning (`walkdir`) | MIT |

---

## Phase 8 — App Management, Automation & Polish

> **Goal:** App install/uninstall intelligence, workflow automation, macro recording, and production hardening.

### Steps

- [ ] **8.1** Fix app management tools
  - Verify `install_application` detects correct package manager (apt/dnf/pacman/brew)
  - Test: "Install htop" → detects apt on Ubuntu → runs `sudo apt install htop` → HITL approval
  - `uninstall_application` → same flow
  - `list_running_apps` → real process list from sysinfo

- [ ] **8.2** Wire automation subsystem
  - Instantiate `EventBus`, `Scheduler`, `WorkflowEngine`, `MacroRecorder` in `init_runtime()`
  - Add Tauri commands: `list_workflows`, `run_workflow`, `record_macro`, `stop_recording`, `play_macro`
  - Store workflows/macros in `~/.kria/automation/`

- [ ] **8.3** Add scheduled tasks
  - "Every morning at 9am, check my email summary" → creates scheduled task
  - "Remind me in 30 minutes to take a break" → one-shot scheduled task
  - Frontend: tasks panel showing upcoming/active schedules

- [ ] **8.4** Add workflow builder (stretch)
  - Visual flow builder in the UI for chaining tools
  - "If new file in ~/Downloads → run virus scan → move to ~/Sorted/{extension}"
  - Persist as JSON workflow definitions

- [ ] **8.5** Production hardening
  - Error recovery: if agent loop panics, catch and report gracefully
  - LLM timeout handling: configurable, with user feedback
  - Memory cleanup: periodic pruning of old facts, expired sessions
  - Update checker: compare version with GitHub releases (no auto-update, just notification)

- [ ] **8.6** Implement real health endpoint
  - Check all subsystems: LLM backend reachable, database open, voice models loaded, MCP servers running
  - Return detailed health report
  - Frontend: status indicators in bottom bar

- [ ] **8.7** Add onboarding flow
  - First-launch wizard: "Welcome to KRIA"
  - Step 1: Choose LLM mode (local with model download, or cloud with API key)
  - Step 2: Voice setup (test microphone, choose voice)
  - Step 3: Permissions (which tools are enabled by default)
  - Step 4: Quick tutorial ("Try asking me to...")

- [ ] **8.8** Add export/backup
  - Export conversation history as Markdown/JSON/PDF
  - Backup entire KRIA data (`~/.kria/`) to a zip
  - Import backup on new machine

### Verification
- [ ] "Install firefox" → correct package manager detected → HITL → installed
- [ ] "Remind me in 5 minutes" → notification fires after 5 minutes
- [ ] App crash recovery: kill backend → app shows error → auto-reconnects
- [ ] First launch → onboarding wizard guides user through setup

---

## Phase 9 — Adaptive Hardware Tier System

> **Goal:** Auto-detect host RAM + GPU at startup, select the optimal LLM + STT + context combination per tier, and disable features (vision, large context) on weaker hardware. No crashes, no OOM.

### Tier Grid

| Tier | RAM | GPU | LLM | STT | Context | Vision |
|------|-----|-----|-----|-----|---------|--------|
| **Lite** | ≤6 GB | None | Qwen2.5-3B Q4_K_M (1.9 GB) | whisper small-q5_1 (181 MB) | 1024 | No |
| **Standard** | 8 GB | None | Phi-4-mini Q4_K_M (2.5 GB) | whisper medium-q5_0 (514 MB) | 2048 | No |
| **Performance** | 12-16 GB | 4-6 GB | Qwen2.5-VL-7B Q4_K_M (4.7 GB) + mmproj (1.3 GB) | whisper turbo-q5_0 (547 MB) | 4096 | Yes |
| **High** | 16+ GB | 8+ GB | Qwen2.5-VL-7B Q4_K_M (4.7 GB) + mmproj (1.3 GB) | whisper turbo-q5_0 (547 MB) | 8192 | Yes |

### Steps

- [ ] **9.1** Wire `detect_hardware()` into `init_runtime()`
  - `detect.rs` already has `HardwareTier` enum + detection logic
  - Store `HardwareInfo` in `AppState` so all subsystems can query it
  - Cache result to `~/.kria/hardware_tier.json` (skip detection unless `--redetect`)
  - Allow manual override via config: `[hardware] tier = "performance"` or env `KRIA_TIER=high`

- [ ] **9.2** Tier-aware model selection in `ModelRouter`
  - `ModelRouter::from_config()` should read tier and auto-select the appropriate LLM
  - Lite/Standard → text-only model, no vision routing
  - Performance/High → vision-capable model with mmproj
  - If user has manually configured a specific model, respect that override

- [ ] **9.3** Tier-aware context window
  - Set `max_context_tokens` based on tier (1024 / 2048 / 4096 / 8192)
  - Context trimming in agent loop adjusts to the tier's limit
  - Warn user if their config requests more context than tier supports

- [ ] **9.4** Tier-aware STT model selection
  - Lite → `ggml-small-q5_1.bin` (fast, low memory)
  - Standard → `ggml-medium-q5_0.bin` (balanced)
  - Performance/High → `ggml-large-v3-turbo-q5_0.bin` (highest accuracy)

- [ ] **9.5** Tier-aware tool filtering
  - `ToolRegistry` already supports hardware tier filtering
  - Disable vision tools on Lite/Standard
  - Disable heavy concurrent tools on Lite (limit to 1 tool at a time)

- [ ] **9.5.1** Tier-aware Python sidecar configuration
  - On startup, send `configure_tier(tier)` to sidecar:
    - **Lite**: `{"ocr_engine": "pytesseract", "max_image_dim": 512, "embeddings": "disabled", "code_analysis": "regex", "doc_max_pages": 5, "gpu": false}`
    - **Standard**: `{"ocr_engine": "pytesseract", "max_image_dim": 1024, "embeddings": "cpu", "code_analysis": "tree-sitter", "doc_max_pages": null, "gpu": false}`
    - **Performance**: `{"ocr_engine": "easyocr", "max_image_dim": null, "embeddings": "gpu", "code_analysis": "full", "doc_max_pages": null, "gpu": true}`
    - **High**: same as Performance but with `"batch_size": 128, "concurrent_workers": 4`
  - Sidecar stores config and applies to all subsequent requests
  - Skip installing heavy Python deps on Lite tier (no easyocr, no torch)

- [ ] **9.5.2** Tier-aware Python dependency installation
  - `scripts/setup_python.sh` accepts `--tier` flag
  - Lite: install core only (`pymupdf pillow pytesseract tree-sitter pandas trafilatura`)
  - Standard: add `sentence-transformers onnxruntime`
  - Performance/High: add `easyocr torch` (GPU-enabled)
  - Use `uv` optional dependency groups in `pyproject.toml`:
    ```toml
    [project.optional-dependencies]
    lite = ["pymupdf", "pillow", "pytesseract"]
    standard = ["sentence-transformers", "onnxruntime"]
    performance = ["easyocr", "torch"]
    ```

- [ ] **9.6** Tier-aware model downloads in setup script
  - `scripts/setup.sh` should detect tier first, then download only needed models
  - Avoid downloading 4.7 GB Qwen2.5-VL on a 6 GB RAM machine
  - Print tier summary: "Detected: Standard tier (8 GB RAM, no GPU) → downloading Phi-4-mini + whisper-medium"

- [ ] **9.7** Tier-aware llama.cpp launch parameters
  - Thread count: Lite=4, Standard=6, Performance/High=8
  - GPU layers: 0 for Lite/Standard, `99` (all) for Performance/High
  - Batch size: scale with available RAM
  - Generate recommended `llama-server` command for the user

- [ ] **9.8** Frontend: Tier display and model info
  - Show detected tier in Settings (e.g., "Hardware: Performance — 16 GB RAM, RTX 4050 6 GB")
  - Show active model + context window
  - "Change tier" button for manual override
  - Warning banner if running on Lite tier: "Limited capabilities on this hardware"

### Verification
- [ ] On 8 GB no-GPU machine → auto-selects Phi-4-mini, 2048 context, no vision
- [ ] On 16 GB + RTX 4050 → auto-selects Qwen2.5-VL, 4096 context, vision enabled
- [ ] `KRIA_TIER=lite` override → forces lite behavior regardless of actual hardware
- [ ] No OOM with Chrome + 5 tabs open during inference

### Technology
| Component | Tool | License |
|-----------|------|---------|
| Hardware detection | `sysinfo` crate + `nvidia-smi` (already implemented) | MIT |
| Tier config | TOML config (already in place) | — |

---

## Phase 10 — Desktop Automation & Contextual Awareness

> **Goal:** The assistant becomes an intelligent desktop companion — it knows which app is focused, can control windows, open applications, navigate browsers, and adapt its behavior to your current context.

### Steps

- [ ] **10.1** Active window awareness
  - Detect currently focused application (window title + process name)
  - Linux: `xdotool getactivewindow getwindowname` / `xprop`
  - Inject into system prompt context: "User is currently in: VS Code — main.rs"
  - Agent adapts responses: coding help when in IDE, email help when in Thunderbird

- [ ] **10.2** Application launcher intelligence
  - `open_application` already exists — verify and enhance
  - "Open Firefox and go to github.com" → launch app + navigate
  - "Open VS Code with ~/my-project" → `code ~/my-project`
  - Application aliases: "open my editor" → resolves to configured default (VS Code, Neovim, etc.)
  - Recent apps tracking: "Open the app I used last" → frequency-based

- [ ] **10.3** Window management tools
  - `move_window(title, x, y)`, `resize_window(title, w, h)`
  - `minimize_window`, `maximize_window`, `close_window`
  - `tile_windows(layout)` — side-by-side, grid, etc.
  - "Put the terminal on the left and the browser on the right"
  - Linux: `xdotool` / `wmctrl`; cross-platform: via Tauri window APIs for KRIA's own window

- [ ] **10.4** Browser integration
  - `open_url(url)` — open URL in default browser
  - `search_google(query)` — open search results
  - `open_bookmarks_search(query)` — search browser bookmarks (SQLite file on disk)
  - Deep integration option: browser extension that exposes page content to KRIA via local WebSocket

- [ ] **10.5** Desktop quick actions (tray + hotkeys)
  - Global hotkey (e.g., `Super+K`) → opens floating input bar (like Spotlight/Alfred)
  - Type a question → get answer in a popup → dismiss
  - No need to open the full window for quick queries
  - Clipboard transform hotkey: select text → `Ctrl+Shift+T` → translate/summarize/fix grammar in-place

- [ ] **10.6** Contextual system prompt injection
  - Build a "context snapshot" before each agent call:
    ```
    Current time: 2026-04-15 14:30 IST
    Active window: Firefox — GitHub Pull Request #42
    Clipboard: "fn main() { println!(\"hello\"); }"
    Recent files: ~/project/main.rs (2 min ago)
    Hardware: Performance tier, 16 GB RAM, RTX 4050
    ```
  - Agent uses this to give relevant responses without being asked

- [ ] **10.7** Screen region selection
  - "Read the text in this area" → user draws a rectangle on screen → OCR that region
  - "What's in the top-right corner of my screen?" → crop + vision model
  - Linux: `slop` for region selection, then Tesseract/vision

### Verification
- [ ] "What app am I using right now?" → correct answer
- [ ] "Open VS Code with my KRIA project" → launches correctly
- [ ] Global hotkey → floating input → "What's 2+2?" → "4" in popup → dismiss
- [ ] "Put Firefox on the left half of the screen" → window moves

### Technology
| Component | Tool | License |
|-----------|------|---------|
| Window management | `xdotool`, `wmctrl` (Linux) | GPL-2.0 |
| Region selection | `slop` | GPL-3.0 |
| Global hotkey | `tauri-plugin-global-shortcut` (already in use) | MIT |
| Browser bookmarks | SQLite (Firefox/Chrome store bookmarks in SQLite) | — |

---

## Phase 11 — Developer Power Tools

> **Goal:** First-class developer assistant — Git operations, code execution, project analysis, REPL mode, and IDE-like intelligence.

### Steps

- [ ] **11.1** Git integration tools
  - `git_status` — current branch, changed files, ahead/behind
  - `git_log(n)` — last N commits with messages
  - `git_diff(file)` — show changes in file
  - `git_commit(message)` — stage all + commit (HITL approval: RED)
  - `git_push` — push to remote (HITL approval: RED)
  - `git_branch_list`, `git_checkout(branch)`, `git_create_branch(name)`
  - `git_stash`, `git_stash_pop`
  - "What did I change today?" → `git log --since=today --oneline`

- [ ] **11.2** GitHub/GitLab integration (via MCP or direct API)
  - `list_pull_requests`, `get_pr_diff(pr_number)`
  - `create_issue(title, body)`, `list_issues`
  - `get_ci_status` — check if latest pipeline passed
  - Use GitHub MCP server or direct REST API with user's token

- [ ] **11.3** Code execution sandbox
  - `run_python(code)` — execute Python snippet, capture stdout + stderr
  - `run_javascript(code)` — execute via Node.js
  - `run_bash(code)` — already exists in `shell.rs`, verify safety
  - **Sandboxed execution**: use `bwrap` (bubblewrap) on Linux for isolation
  - Output capture: return stdout, stderr, exit code, execution time
  - HITL approval for all code execution (configurable: auto-approve for trusted scripts)

- [ ] **11.4** REPL / interactive code mode
  - Frontend: "Code" tab — persistent REPL session
  - Write Python/JS → execute → see output → iterate
  - Session state preserved: variables carry across executions
  - Syntax highlighted input with basic autocompletion

- [ ] **11.5** Project analysis tools
  - `analyze_project(path)` — detect language, framework, dependencies, structure
  - `find_security_issues(path)` — basic SAST: hardcoded secrets, unsafe deps
  - `dependency_audit(path)` — parse package.json/Cargo.toml/requirements.txt, check for outdated/vulnerable deps
  - `generate_readme(path)` — LLM-powered README generation from project structure

- [ ] **11.6** Diff and patch tools
  - `diff_files(file_a, file_b)` — unified diff output
  - `apply_patch(file, patch)` — apply suggested changes
  - `suggest_fix(file, error_message)` — LLM reads file + error → suggests fix
  - "Fix the error in main.rs" → reads file + last compiler error → suggests solution → HITL → applies

- [ ] **11.7** Database tools
  - `query_sqlite(db_path, sql)` — run read-only SQL query (HITL for writes)
  - `describe_database(db_path)` — list tables, columns, row counts
  - "What tables are in my project's database?" → auto-finds .db files → describes schema

### Verification
- [ ] "What's my git status?" → real branch + changed files
- [ ] "Run this Python: `print(sum(range(100)))`" → output: 4950
- [ ] "Analyze this project" → correct language/framework detection
- [ ] "Show my latest GitHub PRs" → real data from API

### Technology
| Component | Tool | License |
|-----------|------|---------|
| Git | `git2` crate or CLI wrapping | MIT / GPL-2.0 |
| Sandbox | `bubblewrap` (bwrap) | LGPL-2.0 |
| GitHub API | `reqwest` + REST / MCP server | MIT |
| SQLite inspection | `rusqlite` (already in use) | Public Domain |

---

## Phase 12 — RAG, Document Chat & Deep Knowledge

> **Goal:** The assistant can ingest entire documents, codebases, and knowledge bases — then answer questions with cited sources. "Chat with your PDF/codebase/wiki." The Python sidecar handles all heavy ingestion; Rust owns storage, retrieval, and context assembly.

### Steps

- [ ] **12.1** Document ingestion pipeline (Hybrid Rust + Python)
  - Upload a document (PDF, DOCX, TXT, MD, code files, entire folders)
  - **Step 1 (Python sidecar)**: `SidecarBridge::document.extract(file)` → structured text with metadata, sections, tables
  - **Step 2 (Python sidecar)**: `SidecarBridge::embeddings.chunk_and_embed(text, chunk_size=512, overlap=64)` → `{chunks: [...], vectors: [[...], ...]}`
  - **Step 3 (Rust)**: Store chunks + vectors + metadata in SQLite + usearch
  - **Ownership**: Rust owns the database and vector index. Python is a stateless worker — it processes and returns, never holds data.
  - Tier-aware: Lite tier ingests plain text only (no chunking/embedding, keyword search only); Performance tier uses GPU embeddings for fast batch processing

- [ ] **12.2** Retrieval-Augmented Generation (RAG)
  - **Query embedding (Rust)**: `fastembed-rs` / `ort` for real-time single-vector embedding (~5ms)
  - **Vector search (Rust)**: usearch HNSW index → cosine similarity → top-K candidates
  - **Keyword search (Rust)**: SQLite FTS5 for exact keyword matching
  - **Hybrid scoring**: combine vector similarity (0.6) + keyword match (0.3) + recency (0.1)
  - Inject retrieved chunks into LLM context as "Reference Material"
  - LLM answers with citations: "According to [document.pdf, page 3]..."
  - **Token budget**: use `TokenBudget` to fit chunks within remaining context window

- [ ] **12.3** Codebase chat (Python-powered ingestion)
  - "Ingest this project: ~/my-project" → Rust walks directory tree → Python analyzes each file:
    - Source files: `SidecarBridge::code.analyze(file)` → AST structure, function signatures, imports
    - Docs/READMEs: `SidecarBridge::document.extract(file)` → structured text
    - All files: `SidecarBridge::embeddings.chunk_and_embed(text)` → vectors
  - Store per-file: language, AST summary, chunk vectors, last modified timestamp
  - "How does the authentication work?" → retrieves relevant files → answers with file:line citations
  - **Incremental re-indexing**: on re-ingest, hash each file → only re-process changed files

- [ ] **12.4** Knowledge base management
  - Frontend: "Knowledge" tab — list ingested documents/projects
  - Show per-document: name, type, chunks count, size, last updated
  - Re-index button, delete button
  - Add from URL: fetch webpage (via Python `web.extract`) → ingest
  - Add from folder: recursive file discovery → batch ingest via sidecar
  - Ingestion progress: show % complete, current file, estimated time

- [ ] **12.5** Citation rendering in frontend
  - When agent cites a source, render as clickable reference
  - Click → opens the document / scrolls to the relevant section
  - Show confidence score for each cited chunk

- [ ] **12.6** Conversation-scoped knowledge
  - "For this conversation, use only these documents: [doc1, doc2]"
  - Restricts retrieval to specific knowledge base subset
  - Useful for focused research sessions

### Verification
- [ ] Upload a 50-page PDF → "What are the key conclusions?" → accurate answer with page citations
- [ ] Ingest a codebase → "Where is the database connection handled?" → correct file + function
- [ ] "Summarize all documents in my knowledge base" → overview of all ingested content
- [ ] Lite tier: ingestion uses keyword search only (no embeddings)
- [ ] Performance tier: GPU-accelerated batch embedding, full AST analysis

### Technology
| Component | Tool | License |
|-----------|------|---------|
| Document extraction | Python sidecar: PyMuPDF, python-docx, pandas | AGPL/MIT/BSD-3 |
| Code analysis | Python sidecar: tree-sitter | MIT |
| Chunking | Python sidecar: token-aware recursive splitter | — |
| Batch embeddings | Python sidecar: sentence-transformers | Apache 2.0 |
| Query embeddings | Rust: fastembed-rs / ort (real-time) | Apache 2.0 |
| Vector index | Rust: usearch (HNSW, mmap) | Apache 2.0 |
| Keyword search | Rust: SQLite FTS5 | Public Domain |
| Storage | Rust: SQLite + usearch | Public Domain/Apache 2.0 |

---

## Phase 13 — Proactive Intelligence & Smart Notifications

> **Goal:** The assistant doesn't just wait for commands — it *notices things*, *warns you*, and *suggests actions* on its own.

### Steps

- [ ] **13.1** System health monitoring
  - Background task runs every 60 seconds
  - Checks: disk space, memory pressure, CPU temperature, battery level
  - Alert thresholds (configurable): disk <10%, RAM <500 MB, battery <15%
  - "Your disk has only 5 GB free. Want me to find large files to clean up?"

- [ ] **13.2** File watchers
  - Watch configurable directories for changes (using `notify` crate)
  - "New file in Downloads: report.pdf (2.3 MB)" → offer to summarize/move/rename
  - Watch for large files: "A 4 GB file was just moved to Desktop"
  - Watch for sensitive files: alert if `.env`, private keys, etc. appear in unexpected locations

- [ ] **13.3** Build/CI monitoring
  - Watch a configurable log file or process
  - Detect compilation errors → "Build failed: error[E0308] in main.rs line 42. Want me to help fix it?"
  - Detect test failures → summarize which tests failed and why

- [ ] **13.4** Daily briefing
  - Configurable: "Every day at 9:00 AM, show me:"
  - Weather forecast, calendar events (from CalDAV), unread email count, disk health, git status of watched repos
  - Rendered as a summary card in the chat

- [ ] **13.5** Smart suggestions based on patterns
  - Track user's common tool invocations
  - "You search for the weather every morning. Want me to auto-show it?"
  - "You always commit at 5 PM. It's 5 PM — want to commit today's changes?"
  - Suggestion engine: frequency + time-of-day analysis

- [ ] **13.6** Idle-time tasks
  - When user is idle for N minutes and system load is low:
  - Run knowledge base re-indexing (if enabled)
  - Run memory fact pruning/compaction
  - Pre-fetch weather/news for daily briefing
  - Configurable: user can enable/disable idle tasks

- [ ] **13.7** Frontend: Notification center
  - In-app notification panel (slide-out from right)
  - Categories: Alerts (red), Suggestions (yellow), Info (blue)
  - Dismiss, snooze, action buttons per notification
  - OS-level notifications for critical alerts when app is minimized

### Verification
- [ ] Disk drops below 10% → proactive alert with cleanup suggestion
- [ ] New file appears in Downloads → notification with action options
- [ ] 9 AM → daily briefing card appears in chat
- [ ] "You usually check git at this time" → suggestion shown

### Technology
| Component | Tool | License |
|-----------|------|---------|
| File watching | `notify` crate | MIT/Apache 2.0 |
| OS notifications | `tauri-plugin-notification` (already in use) | MIT |
| Scheduling | `tokio` timers + cron expressions | MIT |
| Email (IMAP) | `async-imap` crate (optional) | MIT/Apache 2.0 |
| CalDAV | `reqwest` + CalDAV XML parsing (optional) | MIT |

---

## Phase 14 — Multi-Language, Accessibility & Cross-Platform Polish

> **Goal:** The assistant works well for non-English speakers, is accessible, and runs smoothly on all platforms.

### Steps

- [ ] **14.1** Multi-language STT/TTS
  - Download language-specific whisper models (or use multilingual base model)
  - Piper has voices for 30+ languages — add a language selector in settings
  - Auto-detect spoken language if using multilingual whisper model
  - Config: `[voice] language = "en"` with dropdown in settings

- [ ] **14.2** UI localization framework
  - Use i18n library (e.g., `@solid-primitives/i18n`) for frontend strings
  - Extract all UI text into locale JSON files (`ui/src/locales/en.json`, `de.json`, etc.)
  - Language selector in settings
  - Start with: English, Spanish, German, French, Chinese, Arabic, Hindi

- [ ] **14.3** Translation tool
  - `translate_text(text, from, to)` — use LLM for translation (no external API needed)
  - Clipboard integration: "Translate whatever I copied to Spanish"
  - Real-time compose: type in one language, get translation as you type

- [ ] **14.4** Accessibility improvements
  - ARIA labels on all interactive elements
  - Keyboard-only navigation (tab order, focus rings)
  - High-contrast theme option
  - Screen reader compatibility
  - Font size scaling (already `ui.font_size` in config)
  - Reduce motion option for animations

- [ ] **14.5** Windows platform polish
  - Verify all tools work on Windows (path separators, shell commands, package managers)
  - Windows-specific tools: PowerShell execution, registry access, Windows service management
  - `winget` for app install/uninstall
  - Windows notification center integration

- [ ] **14.6** macOS platform polish
  - AppleScript integration for system automation
  - `brew` for app install/uninstall
  - macOS notification center
  - Spotlight-like quick input bar
  - Apple Silicon GPU detection (Metal)

### Verification
- [ ] Switch voice to Spanish → speak Spanish → correct transcription → Spanish response
- [ ] Switch UI to German → all buttons, labels, menus in German
- [ ] Navigate entire app using keyboard only
- [ ] All tools work on Windows (file ops, shell, app management)

### Technology
| Component | Tool | License |
|-----------|------|---------|
| i18n | `@solid-primitives/i18n` | MIT |
| Multi-language STT | whisper multilingual models | MIT |
| Multi-language TTS | Piper (30+ languages) | MIT |
| Accessibility | WAI-ARIA standards | — |

---

## Technology Stack (Complete)

> All tools are free and open source.

### Core Runtime
| Tech | Purpose | License |
|------|---------|---------|
| Rust | Sovereign Core language | MIT/Apache 2.0 |
| Python 3.11+ | Sidecar language (pre-cognitive processing) | PSF |
| Tauri v2 | Desktop framework | MIT/Apache 2.0 |
| SolidJS | Frontend UI | MIT |
| Vite | Frontend build tool | MIT |
| SQLite (`rusqlite`) | Database — chat, memory, facts, audit, RAG chunks | Public Domain |
| `tokio` | Async runtime | MIT |
| `uv` | Python package/venv manager (fast, lockfile-based) | MIT/Apache 2.0 |

### Rust↔Python Bridge
| Tech | Purpose | License |
|------|---------|---------|
| JSON-RPC 2.0 over stdio | IPC protocol (zero-network, cross-platform) | — |
| `tokio::process` | Sidecar process lifecycle management | MIT |
| `serde_json` | Request/response serialization | MIT/Apache 2.0 |

### AI / LLM (Tier-Aware)
| Tech | Purpose | License |
|------|---------|---------|
| llama.cpp | Local LLM inference server | MIT |
| Qwen2.5-3B-Instruct Q4_K_M | Text LLM — **Lite** tier (1.9 GB) | Apache 2.0 |
| Phi-4-mini-instruct Q4_K_M | Text LLM — **Standard** tier (2.5 GB) | MIT |
| Qwen2.5-VL-7B Q4_K_M + mmproj | Vision LLM — **Performance/High** tier (6 GB) | Apache 2.0 |
| whisper.cpp (small/medium/turbo) | Speech-to-text (per tier) | MIT |
| Piper (30+ languages) | Text-to-speech | MIT |
| Silero VAD ONNX | Voice activity detection | MIT |
| all-MiniLM-L6-v2 ONNX | Text embeddings — real-time query (Rust), batch RAG (Python) | Apache 2.0 |
| `ort` / `fastembed-rs` | ONNX Runtime bindings (Rust, for real-time embeddings) | MIT/Apache 2.0 |
| `sentence-transformers` | Embeddings (Python sidecar, for batch RAG ingestion) | Apache 2.0 |

### Python Sidecar — Pre-Cognitive Layer
| Tech | Purpose | License |
|------|---------|---------|
| OpenCV (`opencv-python-headless`) | Image analysis, feature extraction | BSD-3 |
| Pillow | Image metadata, thumbnails, format conversion | MIT-like |
| pytesseract | OCR (Standard tier, CPU) | Apache 2.0 |
| easyocr | OCR (Performance tier, GPU, multilingual) | Apache 2.0 |
| PyMuPDF (fitz) | PDF extraction (text, tables, images, structure) | AGPL-3.0 |
| python-docx | DOCX extraction | MIT |
| pandas | CSV/Excel parsing, data profiling | BSD-3 |
| tree-sitter | AST-level code parsing (50+ languages) | MIT |
| trafilatura | Web article extraction (JS-heavy pages) | Apache 2.0 |
| readability-lxml | Web content cleaning | Apache 2.0 |
| librosa | Audio preprocessing (noise reduction, VAD enhancement) | ISC |
| noisereduce | Audio denoising | MIT |

### Internet & Search
| Tech | Purpose | License |
|------|---------|---------|
| SearXNG | Meta-search engine (self-hostable) | AGPL-3.0 |
| Open-Meteo | Weather API (free, no key required) | CC BY 4.0 |
| `reqwest` | HTTP client | MIT/Apache 2.0 |
| `scraper` | HTML parsing & readability (Rust fast path) | MIT |
| `async-imap` | Email reading (optional) | MIT/Apache 2.0 |

### System & Desktop Integration
| Tech | Purpose | License |
|------|---------|---------|
| `sysinfo` | System monitoring + hardware tier detection | MIT |
| CPAL + rodio | Audio capture/playback | Apache 2.0 |
| `arboard` | Clipboard access | MIT/Apache 2.0 |
| Tesseract CLI | OCR fallback (when sidecar unavailable) | Apache 2.0 |
| `notify` | File system watching (proactive alerts) | MIT/Apache 2.0 |
| `xdotool` / `wmctrl` | Window management (Linux) | GPL-2.0 |
| `slop` | Screen region selection (Linux) | GPL-3.0 |
| `bubblewrap` (bwrap) | Sandboxed code execution (Linux) | LGPL-2.0 |

### Developer Tools
| Tech | Purpose | License |
|------|---------|---------|
| `git2` | Git operations (libgit2 bindings) | MIT |
| GitHub REST API | PR/issue/CI integration | — |
| `meval` | Mathematical expression evaluator | MIT |

### Frontend Libraries (to add)
| Tech | Purpose | License |
|------|---------|---------|
| `marked` + `DOMPurify` | Safe markdown rendering | MIT |
| `highlight.js` or `Shiki` | Syntax highlighting in code blocks | BSD-3 / MIT |
| KaTeX | Math equation rendering | MIT |
| Lucide Icons | Icon set | ISC |
| `@solid-primitives/i18n` | UI localization (14 languages) | MIT |

---

## What Makes a Strong Assistant vs a Dumb Chatbot

| Trait | Dumb Chatbot | Strong Assistant (KRIA Goal) |
|-------|-------------|------------------------------|
| **Memory** | Forgets everything after closing | Remembers your name, preferences, past conversations, project context across sessions |
| **Action** | Only outputs text | Takes real actions: creates files, installs apps, runs code, controls windows, manages git |
| **Awareness** | Knows nothing about your system | Knows your OS, hardware, active window, clipboard, running apps, disk space, time |
| **Proactivity** | Waits passively for input | Notices low disk, build failures, new downloads — suggests actions before you ask |
| **Adaptability** | One-size-fits-all | Adapts model, context, features to your hardware tier. Uses fast model for simple questions, powerful model for complex ones |
| **Pre-Cognition** | Raw data dumped to LLM | Python sidecar pre-digests images, PDFs, code into structured context — LLM sees clean, token-efficient input |
| **Senses** | Text input only | Sees (vision/screenshots), hears (voice/STT), speaks (TTS) |
| **Tools** | Zero tools | 60+ native tools + pre-cognitive sidecar tools + community skill plugins |
| **Safety** | No guardrails | 4-tier policy engine, blacklist, HITL approval, audit trail, rollback, sandboxed sidecar |
| **Extensibility** | Closed system | MCP protocol, Python skill plugins with isolated venvs, community ecosystem |
| **Intelligence** | Single LLM call, no reasoning | ReAct loop: plan → reason → act → observe → iterate. Multi-step problem solving |
| **Knowledge** | Limited to model training data | RAG: ingest your documents/codebase via Python sidecar, answer with citations |
| **Integration** | Standalone silo | Git, GitHub, email, calendar, browser bookmarks, databases |

---

## Tracker

### Phase Progress

| Phase | Name | Tasks | Status |
|-------|------|-------|--------|
| **0** | Critical Wiring (Agent Loop + EventBus) | 5 | ⬜ Not Started |
| **0.5** | Python Sidecar & Pre-Cognitive Bridge | 12 | ⬜ Not Started |
| **1** | Persistent Memory & Chat History | 8 | ⬜ Not Started |
| **2** | Internet, Search & Real-Time | 6 | ⬜ Not Started |
| **3** | File & System Intelligence | 7 | ⬜ Not Started |
| **4** | Vision & Multimodal | 6 | ⬜ Not Started |
| **5** | Voice Pipeline | 7 | ⬜ Not Started |
| **6** | Settings, API Keys & UI Overhaul | 9 | ⬜ Not Started |
| **7** | MCP Protocol & Plugins | 6 | ⬜ Not Started |
| **7.5** | Skill-Plugin Ecosystem | 7 | ⬜ Not Started |
| **8** | App Management & Polish | 8 | ⬜ Not Started |
| **9** | Adaptive Hardware Tier System | 8 | ⬜ Not Started |
| **10** | Desktop Automation & Contextual Awareness | 7 | ⬜ Not Started |
| **11** | Developer Power Tools | 7 | ⬜ Not Started |
| **12** | RAG, Document Chat & Deep Knowledge | 8 | ⬜ Not Started |
| **13** | Proactive Intelligence & Smart Notifications | 7 | ⬜ Not Started |
| **14** | Multi-Language, Accessibility & Cross-Platform | 6 | ⬜ Not Started |
| | **Total** | **124** | |

### Recommended Execution Order

```
Phase 0 (Critical Wiring + EventBus)    ████████████████  FIRST — unlocks everything
  │
  ├── Phase 0.5 (Python Sidecar)        ████████████████  EARLY — Pre-Cognitive bridge, unlocks deep processing
  │
  ├── Phase 9 (Hardware Tiers)          ████████████████  EARLY — affects model choice + sidecar intensity
  │
  ├── Phase 1 (Memory & History)        ████████████████  Core intelligence
  │     │
  │     ├── Phase 12 (RAG & Docs)       ████████████████  Deep knowledge (needs embeddings from P1 + sidecar from P0.5)
  │     │
  │     ├── Phase 6 (UI Overhaul)       ████████████████  User experience
  │     │
  │     └── Phase 2 (Internet)          ████████████████  Real-time awareness
  │           │
  │           └── Phase 3 (Files)       ████████████████  System intelligence (fast Rust + deep Python paths)
  │
  ├── Phase 11 (Developer Tools)        ████████████████  Git, code exec, project analysis
  │
  ├── Phase 4 (Vision)                  ████████████████  Pre-Cognitive image pipeline (needs P0.5)
  │
  ├── Phase 5 (Voice)                   ████████████████  Independent track (audio preprocessing via sidecar)
  │
  ├── Phase 10 (Desktop Automation)     ████████████████  Contextual awareness
  │
  ├── Phase 7 (MCP & Plugins)           ████████████████  Extension ecosystem
  │     │
  │     ├── Phase 7.5 (Skill Plugins)   ████████████████  Python skill ecosystem (needs P0.5 + P7)
  │     │
  │     └── Phase 8 (App Mgmt & Polish) ████████████████  Production readiness
  │
  ├── Phase 13 (Proactive Intelligence) ████████████████  Needs most other phases working
  │
  └── Phase 14 (Multi-Language & A11y)  ████████████████  LAST — polish layer
```

### Milestone Checkpoints

| Milestone | After Phase | What You Should Be Able To Do |
|-----------|-------------|-------------------------------|
| **M1: Smart Assistant** | 0 + 0.5 + 9 + 1 | Multi-turn conversations with tool use, memory, history, tier-optimized models, and Python sidecar bridge ready |
| **M2: Connected Assistant** | + 2 + 3 | Web search, file management, system control, real-time data — with deep Python processing paths |
| **M3: Multimodal Assistant** | + 4 + 5 | See images (Pre-Cognitive pipeline), hear voice (sidecar audio preprocessing), speak responses |
| **M4: Developer Assistant** | + 11 | Git operations, code execution, project analysis with AST-level code intelligence via tree-sitter |
| **M5: Knowledge Assistant** | + 12 | Chat with documents, hybrid Rust+Python RAG with citations, codebase Q&A |
| **M6: Configurable Assistant** | + 6 | Beautiful UI, API key management, themes, settings |
| **M7: Extensible Assistant** | + 7 + 7.5 + 8 | MCP servers, Python skill plugins with isolated venvs, community ecosystem, onboarding |
| **M8: Proactive Assistant** | + 10 + 13 | Desktop awareness, smart notifications, daily briefings |
| **M9: Global Assistant** | + 14 | Multi-language, accessible, polished cross-platform |

---

### Per-Task Checklist (copy this into your issue tracker)

<details>
<summary>Phase 0 — Critical Wiring + EventBus (expand)</summary>

- [ ] 0.1 Add AgentLoop to AppState, instantiate EventBus (tokio::broadcast channels)
- [ ] 0.2 Rewrite send_message to use AgentLoop (sidecar delegation for pre-processing tools)
- [ ] 0.3 Wire streaming events (tool_call, tool_result, thinking, done)
- [ ] 0.4 Update system prompt with tool descriptions + date + user context
- [ ] 0.5 End-to-end test: tool-using conversation
</details>

<details>
<summary>Phase 0.5 — Python Sidecar & Pre-Cognitive Bridge (expand)</summary>

- [ ] 0.5.1 Create `kria-modules/` Python package: pyproject.toml, uv.lock, src layout
- [ ] 0.5.2 Implement `bridge.py`: JSON-RPC 2.0 dispatcher over stdio (stdin/stdout)
- [ ] 0.5.3 Implement Python processors: image (OpenCV, Pillow), document (PyMuPDF, python-docx, pandas)
- [ ] 0.5.4 Implement Python processors: code (tree-sitter AST), web (trafilatura), audio (librosa, noisereduce)
- [ ] 0.5.5 Implement Python embeddings processor: sentence-transformers batch ingestion
- [ ] 0.5.6 Implement Rust `sidecar/` module: bridge.rs (spawn/health/restart), protocol.rs (JSON-RPC types)
- [ ] 0.5.7 Implement Rust `sidecar/` module: health.rs (heartbeat, auto-restart), tier_config.rs (processing depth)
- [ ] 0.5.8 Wire SidecarBridge into AppState, add to AgentLoop context
- [ ] 0.5.9 Register pre-cognitive tools in ToolRegistry (image_analyze, document_extract, code_analyze, web_extract)
- [ ] 0.5.10 Add auto-routing in agent loop: detect file MIME type → route to sidecar if beneficial
- [ ] 0.5.11 Create `scripts/setup_python.sh`: install uv, create venv, sync dependencies, verify bridge
- [ ] 0.5.12 End-to-end test: image upload → Python preprocessing → structured JSON → LLM response
</details>

<details>
<summary>Phase 1 — Memory & Chat History (expand)</summary>

- [ ] 1.1 Wire MemoryStore into AppState
- [ ] 1.2 Persist chat messages to SQLite
- [ ] 1.3 Session management commands (create, list, delete, switch)
- [ ] 1.4 Load conversation context on each message
- [ ] 1.5 Replace fake embeddings with all-MiniLM-L6-v2 via ort
- [ ] 1.6 Wire knowledge tools to memory layer
- [ ] 1.7 Automatic fact extraction from conversations
- [ ] 1.8 Frontend session sidebar with real data
</details>

<details>
<summary>Phase 2 — Internet & Real-Time (expand)</summary>

- [ ] 2.1 Verify internet tools standalone
- [ ] 2.2 Add SearXNG search integration
- [ ] 2.3 Improve web page fetching (JS rendering, readability)
- [ ] 2.4 Add time/date awareness to system prompt
- [ ] 2.5 Add weather, news, exchange rate, calculator tools
- [ ] 2.6 Register all internet tools in agent loop
</details>

<details>
<summary>Phase 3 — File & System Intelligence (expand)</summary>

- [ ] 3.1 Verify file tools work through agent loop
- [ ] 3.2 Enhanced file search (content search, patterns)
- [ ] 3.3 Code intelligence tools (analyze, LOC, diff, TODOs)
- [ ] 3.4 Document understanding (PDF, DOCX, CSV)
- [ ] 3.5 HITL approval flow for destructive operations
- [ ] 3.6 Clipboard intelligence
- [ ] 3.7 Frontend tool execution feedback (collapsible blocks)
</details>

<details>
<summary>Phase 4 — Vision & Multimodal (expand)</summary>

- [ ] 4.1 Image upload in frontend (file input, drag-drop, paste)
- [ ] 4.2 Image-to-LLM pipeline (base64 + vision model)
- [ ] 4.3 Screenshot tool for self-initiated vision
- [ ] 4.4 Model routing for vision vs text
- [ ] 4.5 OCR via Tesseract
- [ ] 4.6 Tauri command for image messages
</details>

<details>
<summary>Phase 5 — Voice Pipeline (expand)</summary>

- [x] 5.1 Wire capture → VAD → STT
- [x] 5.2 STT output → agent loop
- [x] 5.3 Agent response → TTS → playback
- [x] 5.4 Real voice commands (start/stop/status)
- [x] 5.5 Download/verify voice models
- [x] 5.6 Frontend voice UI (waveform, states)
- [x] 5.7 Upgrade to Silero VAD
</details>

<details>
<summary>Phase 6 — Settings & UI Overhaul (expand)</summary>

- [ ] 6.1 Wire settings modal to real config (read + write + persist)
- [ ] 6.2 API key management (input, keychain storage, test connection)
- [ ] 6.3 Model management UI (list, select, download, delete)
- [ ] 6.4 Markdown rendering with syntax highlighting in messages
- [ ] 6.5 Rich input area (auto-grow, attachments, slash commands)
- [ ] 6.6 Layout + navigation (sidebar, top bar, status bar)
- [ ] 6.7 Dark/light themes
- [ ] 6.8 Keyboard shortcuts overlay
- [ ] 6.9 Notification system (toasts + OS notifications)
</details>

<details>
<summary>Phase 7 — MCP & Plugins (expand)</summary>

- [ ] 7.1 Implement MCP client (stdio + HTTP/SSE)
- [ ] 7.2 MCP server configuration in config.toml
- [ ] 7.3 Register MCP tools in ToolRegistry
- [ ] 7.4 Frontend MCP server management
- [ ] 7.5 Example Python MCP server
- [ ] 7.6 Plugin directory and manifest format
</details>

<details>
<summary>Phase 7.5 — Skill-Plugin Ecosystem (expand)</summary>

- [ ] 7.5.1 Define skill.json manifest format (name, version, triggers, dependencies, entry_point)
- [ ] 7.5.2 Implement SkillRegistry in Rust (discover, validate, register, lifecycle management)
- [ ] 7.5.3 Implement skill dependency isolation: per-skill Python venvs via `uv` under `skills/<name>/.venv/`
- [ ] 7.5.4 Implement Tauri commands: `install_skill`, `uninstall_skill`, `list_skills`, `enable_skill`, `disable_skill`
- [ ] 7.5.5 Wire skill tools into ToolRegistry as `skill_<name>_<action>` dynamic tools
- [ ] 7.5.6 Frontend skill management panel (browse, install, enable/disable, view logs)
- [ ] 7.5.7 Create 2-3 example skills: `summarize-pdf`, `git-changelog`, `csv-analyzer`
</details>

<details>
<summary>Phase 8 — App Management & Polish (expand)</summary>

- [ ] 8.1 Fix app install/uninstall tools
- [ ] 8.2 Wire automation subsystem (workflows, macros)
- [ ] 8.3 Scheduled tasks and reminders
- [ ] 8.4 Visual workflow builder (stretch)
- [ ] 8.5 Production hardening (crash recovery, timeouts)
- [ ] 8.6 Real health endpoint with subsystem checks
- [ ] 8.7 First-launch onboarding wizard
- [ ] 8.8 Export/backup/import
</details>

<details>
<summary>Phase 9 — Adaptive Hardware Tier System (expand)</summary>

- [ ] 9.1 Wire detect_hardware() into init_runtime, cache tier
- [ ] 9.2 Tier-aware model selection in ModelRouter
- [ ] 9.3 Tier-aware context window limits
- [ ] 9.4 Tier-aware STT model selection
- [ ] 9.5 Tier-aware tool filtering (disable vision on lite/standard)
- [ ] 9.6 Tier-aware model downloads in setup script
- [ ] 9.7 Tier-aware llama.cpp launch parameters
- [ ] 9.8 Frontend tier display and model info panel
</details>

<details>
<summary>Phase 10 — Desktop Automation & Contextual Awareness (expand)</summary>

- [ ] 10.1 Active window awareness (inject focused app into context)
- [ ] 10.2 Application launcher intelligence (open app + navigate)
- [ ] 10.3 Window management tools (move, resize, tile)
- [ ] 10.4 Browser integration (open URL, search, bookmarks)
- [ ] 10.5 Desktop quick actions (global hotkey → floating input bar)
- [ ] 10.6 Contextual system prompt injection (time, window, clipboard, tier)
- [ ] 10.7 Screen region selection (draw rectangle → OCR/vision)
</details>

<details>
<summary>Phase 11 — Developer Power Tools (expand)</summary>

- [ ] 11.1 Git integration (status, log, diff, commit, push, branch)
- [ ] 11.2 GitHub/GitLab integration (PRs, issues, CI status)
- [ ] 11.3 Code execution sandbox (Python, JS, Bash + bubblewrap)
- [ ] 11.4 REPL / interactive code mode in frontend
- [ ] 11.5 Project analysis (language detect, deps, structure)
- [ ] 11.6 Diff and patch tools (suggest fix → HITL → apply)
- [ ] 11.7 Database tools (query SQLite, describe schema)
</details>

<details>
<summary>Phase 12 — RAG, Document Chat & Deep Knowledge (expand)</summary>

- [ ] 12.1 Python sidecar ingestion: chunk documents (MD, PDF, DOCX, code) into semantic units
- [ ] 12.2 Python sidecar batch embedding: sentence-transformers batch embed chunks
- [ ] 12.3 Rust storage: store embeddings + metadata in SQLite (usearch index for ANN)
- [ ] 12.4 Rust retrieval: embed query via ort → top-K ANN search → rank → inject into context
- [ ] 12.5 Codebase chat (ingest project tree via sidecar → Q&A with file citations)
- [ ] 12.6 Knowledge base management UI (list collections, re-index, delete, progress bar)
- [ ] 12.7 Citation rendering in message bubbles (source file, page, chunk highlight)
- [ ] 12.8 Conversation-scoped knowledge filtering + collection selection
</details>

<details>
<summary>Phase 13 — Proactive Intelligence & Smart Notifications (expand)</summary>

- [ ] 13.1 System health monitoring (disk, RAM, CPU, battery alerts)
- [ ] 13.2 File watchers (new downloads, large files, sensitive files)
- [ ] 13.3 Build/CI monitoring (detect errors, offer fixes)
- [ ] 13.4 Daily briefing (weather, calendar, git status, disk health)
- [ ] 13.5 Smart suggestions based on usage patterns
- [ ] 13.6 Idle-time background tasks (re-index, prune, pre-fetch)
- [ ] 13.7 Frontend notification center (alerts, suggestions, info)
</details>

<details>
<summary>Phase 14 — Multi-Language, Accessibility & Cross-Platform (expand)</summary>

- [ ] 14.1 Multi-language STT/TTS (language selector, multilingual whisper)
- [ ] 14.2 UI localization framework (i18n, locale JSON files)
- [ ] 14.3 Translation tool (LLM-powered, clipboard integration)
- [ ] 14.4 Accessibility (ARIA labels, keyboard nav, high contrast, screen reader)
- [ ] 14.5 Windows platform polish (PowerShell, winget, registry)
- [ ] 14.6 macOS platform polish (AppleScript, brew, Metal GPU)
</details>
