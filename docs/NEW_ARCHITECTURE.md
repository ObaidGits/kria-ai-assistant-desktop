---

# K.R.I.A. — Sovereign-Orchestrator Architecture

## System Overview

```
One Rust workspace. Three layers. Two entry points. Python sidecar for heavy AI/ML.

┌──────────────────────────────────────────────────────────────────────────────┐
│                            KRIA Workspace                                    │
│                                                                              │
│  ┌─────────────┐  ┌──────────────┐  ┌────────────────────────────────────┐ │
│  │ kria-desktop │  │ kria-server  │  │           kria-core                │ │
│  │  (Tauri v2)  │  │  (Axum)      │  │      (Sovereign Core)             │ │
│  │              │  │              │  │                                    │ │
│  │ • Window     │  │ • HTTP API   │  │ • Agent engine (ReAct loop)       │ │
│  │ • Tray icon  │  │ • WebSocket  │  │ • LLM inference (local + cloud)   │ │
│  │ • IPC bridge │  │ • Auth layer │  │ • Voice pipeline                  │ │
│  │ • Auto-start │  │ • Static UI  │  │ • Tool system (60+ tools)        │ │
│  │ • Installer  │  │ • Multi-user │  │ • Memory & knowledge (SQLite)    │ │
│  └──────┬───────┘  └──────┬───────┘  │ • Safety & HITL                  │ │
│         │                  │          │ • Sidecar bridge (→ Python)      │ │
│         └──────────────────┴──────────┤ • Plugin runtime                │ │
│                 depends on            └──────────────┬───────────────────┘ │
│                                                      │                     │
│                                                      │ JSON-RPC / msgpack  │
│                                                      │ over stdio          │
│                                                      ▼                     │
│  ┌───────────────────────────────────────────────────────────────────────┐ │
│  │                     kria-modules (Python Sidecar)                      │ │
│  │                    "Pre-Cognitive Processing Layer"                    │ │
│  │                                                                       │ │
│  │  • Image processing (OpenCV, Pillow, Tesseract)                      │ │
│  │  • Document extraction (PyMuPDF, python-docx, pandas)                │ │
│  │  • Embeddings & RAG (sentence-transformers, chunking)                │ │
│  │  • Code analysis (tree-sitter, ast)                                  │ │
│  │  • Web extraction (readability, trafilatura)                         │ │
│  │  • Audio preprocessing (librosa, webrtcvad)                          │ │
│  │  • Skill plugins (drop-in Python modules)                            │ │
│  │                                                                       │ │
│  │  Managed by: uv (virtual environments, per-plugin isolation)          │ │
│  └───────────────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────────────────┘
```

### The Sovereign-Orchestrator Principle

**Rust is the Sovereign Core.** It owns the UI, system hooks, process management, security policies, IPC routing, and the agent loop. It never cedes control. Every action — whether tool execution, file write, or LLM call — passes through Rust's safety and audit layer.

**Python is the Specialized Sidecar.** It handles computationally heavy "pre-cognitive" tasks where Python's ecosystem is unmatched: OpenCV for vision, PyMuPDF for PDFs, sentence-transformers for embeddings, tree-sitter for code parsing. Python never touches the OS directly — it receives sanitized input from Rust and returns structured JSON context.

**The Mediator Pattern.** Rust intercepts raw data (images, documents, audio) → dispatches to Python sidecar → Python "pre-digests" into clean structured context → Rust feeds optimized context to the LLM. The LLM receives high-quality, token-efficient input instead of raw binary noise.

---

## Technology Stack

### Core Runtime

| Layer | Technology | Why |
|---|---|---|
| Language | **Rust** (2021 edition) | Memory safe, zero-cost abstractions, true multithreading, single binary output |
| Async runtime | **Tokio** | Industry standard for async I/O, channels, timers, task spawning |
| Desktop shell | **Tauri v2** | WebView-based UI, native backend, system tray, auto-updater, 5MB binary |
| Server framework | **Axum** | Tower-based HTTP/WS, compatible with Tokio, same ecosystem as Tauri |
| Build system | **Cargo workspace** | Monorepo with shared crates, single `cargo build` for everything |
| Python sidecar | **Python 3.11+** managed via **uv** | Pre-cognitive processing, ML pipelines, plugin ecosystem |
| Rust↔Python IPC | **JSON-RPC 2.0 over stdio** (msgpack optional) | Zero-network-overhead, type-safe bridge, <5ms per call |

### AI / Inference

| Component | Technology | Purpose |
|---|---|---|
| Local LLM | **llama-cpp-rs** (bindings to llama.cpp) | GGUF model loading, GPU offload (CUDA/Vulkan/Metal), KV cache, context management |
| Cloud LLM fallback | **reqwest** + OpenAI-compatible API | Gemini, GPT, Claude, Groq, OpenRouter — same API shape |
| Embeddings (Rust) | **fastembed-rs** or **ort** | Fast in-process embeddings for real-time memory search |
| Embeddings (Python) | **sentence-transformers** | High-quality embeddings for RAG ingestion (batch, GPU-accelerated) |
| Vision preprocessing | **OpenCV** + **Pillow** (Python) | Feature extraction, OCR, metadata — before LLM sees the image |
| Vision LLM | **llama-cpp-rs** multimodal | Qwen2.5-VL, LLaVA — image understanding through same backend |

### Python Sidecar — Pre-Cognitive Layer

| Component | Technology | Purpose |
|---|---|---|
| Package manager | **uv** | Fast venv creation, lockfile-based installs, per-plugin isolation |
| Image processing | **OpenCV** + **Pillow** | Resize, crop, metadata, feature extraction, histogram analysis |
| OCR | **pytesseract** + **easyocr** | Text extraction from images, screenshots, scanned documents |
| PDF extraction | **PyMuPDF** (fitz) | Page-level text, table extraction, image extraction from PDFs |
| DOCX extraction | **python-docx** | Structured text, table, and metadata extraction |
| CSV/Excel | **pandas** | Schema detection, summary statistics, data profiling |
| Web extraction | **trafilatura** + **readability-lxml** | Article extraction, boilerplate removal, structured output |
| Code analysis | **tree-sitter** | AST-level parsing for 50+ languages, function extraction, dependency graphs |
| Audio preprocessing | **librosa** + **webrtcvad** | Noise reduction, silence trimming, VAD enhancement |
| Embeddings | **sentence-transformers** | Batch embedding generation for RAG ingestion |
| Chunking | **langchain-text-splitters** or custom | Token-aware recursive chunking with overlap |

### Voice Pipeline

| Component | Technology | Platform Support |
|---|---|---|
| Audio capture | **cpal** | WASAPI (Win), ALSA/PulseAudio/PipeWire (Linux), CoreAudio (macOS) |
| Voice Activity Detection | **silero-vad** (ONNX via `ort`) | Detect speech start/end, skip silence |
| Speech-to-Text | **whisper-rs** (bindings to whisper.cpp) | GPU-accelerated, multilingual, timestamps |
| Text-to-Speech | **piper-rs** or **piper-phonemize** | ONNX voices, low latency, offline, multiple voices/languages |
| Audio playback | **rodio** | Cross-platform output, volume control, streaming playback |
| Wake word (future) | **Porcupine** or custom ONNX | "Hey KRIA" always-listening trigger |

### Storage & Memory

| Component | Technology | Purpose |
|---|---|---|
| Structured data | **SQLite** via `rusqlite` | Conversations, user facts, preferences, tool audit log, decay scores |
| Vector index | **usearch** (embedded HNSW) | ANN search over embedding vectors, mmap'd, 1ms queries |
| KV cache | **DashMap** (in-memory) | Session state, circuit breaker counts, rate limiters — ephemeral |
| File config | **TOML** via `serde` | User settings, model paths, keybindings, server config |
| Model storage | Local filesystem | `~/.kria/models/` — GGUF, ONNX, piper voices |

### System Control

| Capability | Crate | Platforms |
|---|---|---|
| Process management | **sysinfo** | Win/Linux/macOS — CPU, RAM, disk, battery, process list |
| File operations | **std::fs** + **walkdir** + **globset** | Native filesystem, recursive search, pattern matching |
| Shell execution | **tokio::process** | Async subprocess with stdout/stderr capture |
| Package management | **std::process::Command** | Calls apt/dnf/pacman/winget/brew natively |
| Clipboard | **arboard** | Read/write clipboard, cross-platform |
| Notifications | **notify-rust** (Linux), Tauri notification plugin (all) | Native OS notifications |
| Screen capture | **xcap** | Screenshot for vision model input |
| Autostart | **auto-launch** | Register at login, cross-platform |
| Global hotkey | **global-hotkey** crate or Tauri plugin | Push-to-talk, wake assistant |
| Network | **reqwest** + **hickory-dns** | HTTP, DNS lookup, ping, public IP |

### Frontend (UI)

| Component | Technology | Why |
|---|---|---|
| Renderer | **System WebView** (Edge WebView2 / WebKitGTK / WebKit) | Zero-overhead UI, no Chromium bundled |
| UI framework | **SolidJS** or **Vanilla JS/TS** | Lightweight, reactive, no virtual DOM overhead |
| Styling | **Tailwind CSS** or hand-written CSS | Small bundle, utility-first |
| Build tool | **Vite** | Fast HMR during development, tiny production bundles |
| IPC | **Tauri invoke()** + **Tauri events** | Type-safe Rust↔JS communication, async, binary support |

### Security & Safety

| Component | Implementation | Purpose |
|---|---|---|
| Safety tiers | GREEN / YELLOW / RED / BLACK (Rust enums) | Permission levels for tool execution |
| HITL gateway | Native OS dialog (desktop) / WebSocket prompt (server) | User approval before destructive actions |
| Audit log | SQLite `audit_events` table | Immutable record of all tool executions |
| Rollback | Copy-before-write snapshots | Undo file operations |
| Input sanitization | Argument validation per tool | Prevent injection through LLM-generated args |
| Blacklist | Compiled regex set | Block `rm -rf /`, format commands, registry nukes |
| Sidecar sandbox | Python runs in isolated venv, no system access | Python cannot touch filesystem directly — Rust gates all I/O |

### Distribution & Updates

| Feature | Technology | Platforms |
|---|---|---|
| Windows installer | **NSIS** or **WiX** (Tauri built-in) | `.exe` / `.msi` |
| Linux packages | **AppImage** + **deb** + **rpm** (Tauri built-in) | Universal + distro-specific |
| macOS bundle | **DMG** with notarization (Tauri built-in) | `.dmg` / `.app` |
| Auto-updater | **Tauri updater plugin** | Differential updates, code-signed |
| CI/CD | **GitHub Actions** | Cross-compile for all 3 platforms per commit |

---

## The Rust↔Python Bridge (Mediator Layer)

### Architecture

```
┌────────────────────────────────────────────────────────────────────────┐
│                    RUST (Sovereign Core)                                │
│                                                                        │
│  ┌──────────────┐    ┌──────────────────┐    ┌─────────────────────┐  │
│  │  Agent Loop   │───▶│  SidecarBridge    │───▶│  tokio::process     │  │
│  │  (kria-core)  │    │  (kria-core/      │    │  Child Process      │  │
│  │               │    │   sidecar/)       │    │                     │  │
│  │  Tool calls   │    │                   │    │  stdin  → JSON-RPC  │  │
│  │  that need    │◀───│  • route_request() │◀───│  stdout ← JSON-RPC  │  │
│  │  pre-process  │    │  • health_check() │    │  stderr → log       │  │
│  │               │    │  • tier_config()  │    │                     │  │
│  └──────────────┘    └──────────────────┘    └──────────┬──────────┘  │
│                                                          │             │
└──────────────────────────────────────────────────────────┼─────────────┘
                                                           │
                                              stdio pipe (JSON-RPC 2.0)
                                                           │
┌──────────────────────────────────────────────────────────┼─────────────┐
│                   PYTHON (Sidecar Process)                │             │
│                                                          ▼             │
│  ┌───────────────────────────────────────────────────────────────────┐ │
│  │                      kria_bridge.py                                │ │
│  │  • JSON-RPC dispatcher (reads stdin, writes stdout)               │ │
│  │  • Module registry (discovers installed processors)               │ │
│  │  • Hardware-tier–aware quality settings                           │ │
│  │  • Graceful shutdown, heartbeat                                   │ │
│  └───────────────────────┬───────────────────────────────────────────┘ │
│                          │                                             │
│  ┌───────────┐  ┌───────────┐  ┌───────────┐  ┌──────────────────┐  │
│  │  image     │  │  document  │  │  code      │  │  embeddings      │  │
│  │ processor  │  │ processor  │  │ analyzer   │  │  engine          │  │
│  │           │  │           │  │           │  │                  │  │
│  │ OpenCV    │  │ PyMuPDF   │  │ tree-sit  │  │ sentence-trans   │  │
│  │ Pillow    │  │ python-   │  │ ast       │  │ numpy            │  │
│  │ pytesseract│ │ docx      │  │ jedi      │  │                  │  │
│  │ easyocr   │  │ pandas    │  │           │  │                  │  │
│  └───────────┘  └───────────┘  └───────────┘  └──────────────────┘  │
│                                                                       │
│  ┌───────────┐  ┌───────────┐  ┌────────────────────────────────┐   │
│  │  web       │  │  audio     │  │  skills/ (plugin drop-in dir)  │   │
│  │ extractor  │  │ preproc    │  │                                │   │
│  │           │  │           │  │  each skill = own pyproject +  │   │
│  │ trafilat- │  │ librosa   │  │  own venv via uv               │   │
│  │ ura       │  │ webrtcvad │  │                                │   │
│  │ readabil- │  │ noisered  │  │  skills/summarizer/            │   │
│  │ ity-lxml  │  │           │  │  skills/translator/            │   │
│  └───────────┘  └───────────┘  └────────────────────────────────┘   │
└───────────────────────────────────────────────────────────────────────┘
```

### IPC Protocol

Communication between Rust and Python uses **JSON-RPC 2.0 over stdio** pipes:

```json
// Rust → Python (request)
{"jsonrpc": "2.0", "id": 1, "method": "image.analyze", "params": {
  "file_path": "/tmp/kria_upload_a1b2c3.png",
  "operations": ["metadata", "ocr", "features"],
  "tier": "performance",
  "max_tokens": 2000
}}

// Python → Rust (response)
{"jsonrpc": "2.0", "id": 1, "result": {
  "metadata": {"width": 1920, "height": 1080, "format": "png", "size_kb": 342},
  "ocr_text": "Error: connection refused on port 5432\nHint: Is PostgreSQL running?",
  "features": {
    "dominant_colors": ["#1e1e1e", "#d4d4d4", "#569cd6"],
    "scene_type": "screenshot_terminal",
    "has_text": true,
    "text_density": 0.72
  },
  "summary": "Terminal screenshot showing a PostgreSQL connection error with dark theme IDE"
}}
```

**Why stdio over HTTP/gRPC:**
- Zero network overhead (no TCP handshake, no serialization framework)
- Works on all platforms without port conflicts
- Process lifecycle fully controlled by Rust (spawn, monitor, kill)
- No attack surface — no listening sockets on the machine

### Lifecycle Management

```
App Startup (Rust)
    │
    ├── 1. Detect hardware tier
    ├── 2. Locate Python (bundled or system)
    ├── 3. Verify venv exists, install deps if needed (via uv)
    ├── 4. Spawn Python sidecar as child process
    ├── 5. Send health_check → wait for "ready"
    ├── 6. Send tier_config(tier="performance", features=[...])
    └── 7. Sidecar is now warm — ready for requests

Runtime
    │
    ├── Requests sent over stdin, responses read from stdout
    ├── Heartbeat every 30s — if missed, restart sidecar
    ├── If sidecar crashes → log, restart, retry pending request
    └── Backpressure: max 4 concurrent requests (queued beyond that)

App Shutdown
    │
    ├── Send shutdown RPC → Python cleans up temp files
    ├── Wait 3s for graceful exit
    └── SIGKILL if still alive
```

### Hardware-Tier–Aware Processing

The sidecar adapts its processing depth based on the detected hardware tier:

| Tier | Image Processing | Document Processing | Embedding Strategy | Code Analysis |
|------|-----------------|--------------------|--------------------|---------------|
| **Lite** | Resize to 512px max, basic metadata only | First 5 pages, plain text only | Hash-based similarity (no GPU) | Regex-based, no AST |
| **Standard** | Resize to 1024px, OCR (pytesseract), basic features | Full document, tables as text | CPU embeddings (MiniLM via ort) | tree-sitter AST, top-level |
| **Performance** | Full resolution, OCR (easyocr GPU), feature vectors | Full document + table structure + images | GPU embeddings (batch 64) | Full AST + dependency graph |
| **High** | Full resolution, multi-model OCR, scene description | Deep extraction + cross-reference | GPU embeddings (batch 128) | Full AST + semantic analysis |

---

## Pre-Cognitive Processing Pipeline

### The Problem: LLM Pressure

Raw data wastes LLM context tokens. A 2MB screenshot sent as base64 to a vision model consumes thousands of tokens on pixel data that could be summarized in 50 words. A 100-page PDF pasted as text overflows any context window. The Pre-Cognitive Pipeline solves this by extracting structured, token-efficient context *before* the LLM ever sees the data.

### Pipeline Architecture

```
     Raw Input                    Pre-Cognitive Layer              Optimized Context
  ┌──────────┐              ┌─────────────────────────┐         ┌──────────────────┐
  │ 2MB PNG  │──── Rust ───▶│  Python: image.analyze   │──JSON──▶│ "Terminal showing │
  │ screenshot│   intercept │  • resize to 1024px      │         │  PostgreSQL error │
  └──────────┘              │  • OCR → extract text    │         │  on port 5432.   │
                            │  • scene classification  │         │  Dark theme IDE." │
                            │  • feature extraction    │         │  + 342 bytes OCR  │
                            └─────────────────────────┘         └──────────────────┘
                                                                  ~150 tokens vs
                                                                  ~8000 raw tokens

  ┌──────────┐              ┌─────────────────────────┐         ┌──────────────────┐
  │ 80-page  │──── Rust ───▶│  Python: document.extract │──JSON──▶│ { title, authors, │
  │ PDF      │   intercept │  • PyMuPDF page-by-page  │         │   sections: [...], │
  └──────────┘              │  • table extraction      │         │   key_findings,   │
                            │  • image detection       │         │   tables: [...],  │
                            │  • section segmentation  │         │   page_count: 80 }│
                            └─────────────────────────┘         └──────────────────┘
                                                                  Structured JSON,
                                                                  token-budgeted

  ┌──────────┐              ┌─────────────────────────┐         ┌──────────────────┐
  │ GitHub   │──── Rust ───▶│  Python: code.analyze     │──JSON──▶│ { language: "rust",│
  │ repo     │   intercept │  • tree-sitter AST       │         │   modules: [...], │
  └──────────┘              │  • dependency graph      │         │   entry_points,   │
                            │  • function signatures   │         │   dependencies,   │
                            │  • import resolution     │         │   loc: 12400 }    │
                            └─────────────────────────┘         └──────────────────┘
                                                                  Semantic structure,
                                                                  not raw source
```

### Integration with Agent Loop

```
User: "Summarize this PDF and tell me the key findings"
                    │
                    ▼
         Agent Loop (Rust)
                    │
      ┌─────────────┼──────────────┐
      │             │              │
      ▼             ▼              ▼
 Intent Router   Tool Exec    Policy Check
 → AGENT_LOOP   → read_file   → GREEN
                    │
                    ▼
          File is a PDF (detected by extension + magic bytes)
                    │
                    ▼
          SidecarBridge::route_request("document.extract", {
              file_path: "/home/user/report.pdf",
              operations: ["text", "tables", "sections", "summary"],
              tier: "performance",
              max_tokens: 4000
          })
                    │
               [Python sidecar processes PDF]
                    │
                    ▼
          Clean JSON context returned:
          {
            title: "Q4 Financial Report",
            pages: 80,
            sections: [{heading: "Executive Summary", content: "..."}],
            tables: [{caption: "Revenue by Region", data: [...]}],
            key_terms: ["revenue", "EBITDA", "margin"]
          }
                    │
                    ▼
          Agent injects structured context into LLM prompt
          (uses TokenBudget to fit within context window)
                    │
                    ▼
          LLM generates summary with specific section citations
```

---

## Skill-Plugin Ecosystem

### Architecture

Every skill is a self-contained Python package with its own `pyproject.toml`, dependencies, and virtual environment. Skills are discovered at startup and registered as callable modules in the sidecar bridge.

```
~/.kria/skills/
├── summarizer/
│   ├── pyproject.toml           # name, version, deps, entry_point
│   ├── skill.json               # KRIA manifest: name, description, methods, tier
│   ├── .venv/                   # Isolated venv (created by uv)
│   └── src/
│       └── summarizer/
│           ├── __init__.py
│           └── handler.py       # def process(request) → response
│
├── translator/
│   ├── pyproject.toml
│   ├── skill.json
│   ├── .venv/
│   └── src/
│       └── translator/
│           ├── __init__.py
│           └── handler.py
│
└── code-reviewer/
    ├── pyproject.toml
    ├── skill.json               # {"methods": ["review_pr", "suggest_fixes"]}
    ├── .venv/
    └── src/
        └── code_reviewer/
            ├── __init__.py
            └── handler.py
```

### Skill Manifest (`skill.json`)

```json
{
  "name": "pdf-deep-analyzer",
  "version": "1.0.0",
  "description": "Deep PDF analysis with table extraction and citation mapping",
  "author": "community",
  "min_tier": "standard",
  "methods": [
    {
      "name": "analyze_pdf",
      "description": "Extract structured content from PDF documents",
      "parameters": {
        "file_path": {"type": "string", "required": true},
        "extract_tables": {"type": "boolean", "default": true},
        "extract_images": {"type": "boolean", "default": false},
        "max_pages": {"type": "integer", "default": 100}
      },
      "returns": "Structured JSON with text, tables, metadata, sections"
    }
  ],
  "python_requires": ">=3.11",
  "dependencies": ["pymupdf>=1.24", "camelot-py>=0.11"],
  "safety_tier": "GREEN"
}
```

### Skill Isolation

```
┌──────────────────────────────────────────────────────────────────┐
│  Rust Core                                                        │
│                                                                    │
│  SkillRegistry                                                     │
│  ├── discover_skills("~/.kria/skills/")                           │
│  │   └── for each skill dir:                                      │
│  │       ├── read skill.json → validate manifest                  │
│  │       ├── check min_tier vs current hardware                   │
│  │       ├── verify .venv exists, else run: uv sync               │
│  │       └── register methods in ToolRegistry (prefixed: skill_)  │
│  │                                                                 │
│  ├── invoke_skill("pdf-deep-analyzer", "analyze_pdf", params)     │
│  │   └── SidecarBridge::route_request()                           │
│  │       └── Python dispatcher loads skill module → calls handler │
│  │                                                                 │
│  └── Each skill's venv is fully isolated:                         │
│      ├── Skill A depends on PyMuPDF 1.24                          │
│      ├── Skill B depends on PyMuPDF 1.23                          │
│      └── No conflicts — separate .venv/ directories               │
└──────────────────────────────────────────────────────────────────┘
```

### EventBus for Plugin Communication

```
┌─────────────────────────────────────────────────────────────────┐
│                        EventBus (Rust)                           │
│                                                                   │
│  Channels (tokio::broadcast):                                     │
│  ├── file.uploaded     → triggers: image/document/code processor │
│  ├── message.received  → triggers: intent router, context builder│
│  ├── tool.completed    → triggers: audit logger, UI update       │
│  ├── sidecar.result    → triggers: agent loop continuation       │
│  ├── skill.installed   → triggers: registry refresh              │
│  ├── hardware.changed  → triggers: tier recalculation            │
│  └── voice.transcribed → triggers: send_message pipeline         │
│                                                                   │
│  Subscribers can be:                                              │
│  ├── Rust modules (agent loop, memory store, audit logger)       │
│  ├── Python sidecar (receives events for background processing)  │
│  └── Frontend (via Tauri events → JavaScript listeners)          │
│                                                                   │
│  Example flow:                                                    │
│  1. User uploads image → Tauri IPC → Rust                        │
│  2. Rust emits file.uploaded { path, mime_type, size }           │
│  3. EventBus routes to ImagePreprocessor subscriber              │
│  4. ImagePreprocessor calls SidecarBridge::image.analyze()       │
│  5. Python returns structured context                            │
│  6. Rust emits sidecar.result { request_id, context }            │
│  7. Agent loop picks up result → injects into LLM prompt         │
└─────────────────────────────────────────────────────────────────┘
```

---

## Detailed Architecture

### 1. Agent Engine

```
┌────────────────────────────────────────────────────────────────┐
│                        Agent Engine                             │
│                                                                 │
│  User Input (text, transcribed voice, or uploaded file)         │
│       │                                                         │
│       ▼                                                         │
│  ┌─────────┐    ┌──────────────────────────────────────────┐   │
│  │ Router  │───▶│ Intent Classification                     │   │
│  │         │    │ • DIRECT_TOOL  (single action)            │   │
│  │         │    │ • AGENT_LOOP   (multi-step reasoning)     │   │
│  │         │    │ • CONVERSATION (no tools needed)          │   │
│  │         │    │ • PREPROCESS   (file needs pre-digestion) │   │
│  └─────────┘    └──────────────┬───────────────────────────┘   │
│                                │                                │
│       ┌──────────────┬─────────┼──────────────┬──────────┐     │
│       │              │         │              │          │     │
│       ▼              ▼         ▼              ▼          │     │
│  DIRECT_TOOL    AGENT_LOOP  CONVERSATION  PREPROCESS    │     │
│  • Lite set     • Full set  • No tools    • Dispatch to │     │
│  • 1 iteration  • ReAct ≤10 • Direct LLM   sidecar     │     │
│  • No thinking  • CoT                    • Wait for     │     │
│       │              │                      context      │     │
│       └──────┬───────┘                    • Feed to LLM │     │
│              │                                │          │     │
│              │                ┌───────────────┘          │     │
│              │                │                          │     │
│              ▼                ▼                          │     │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │                    Tool Executor                          │  │
│  │  1. Parse tool call from LLM output                      │  │
│  │  2. Safety policy check (GREEN/YELLOW/RED/BLACK)         │  │
│  │  3. HITL approval if RED                                  │  │
│  │  4. Route: native Rust tool OR Python sidecar tool       │  │
│  │  5. Execute (30s timeout, isolated)                       │  │
│  │  6. If file/binary result → SidecarBridge pre-process    │  │
│  │  7. Truncate result for context window                    │  │
│  │  8. Feed result back to LLM                               │  │
│  └──────────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────┘
```

### 2. LLM Engine

```
┌────────────────────────────────────────────────────────────────┐
│                        LLM Engine                               │
│                                                                 │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │                   Model Router                            │  │
│  │                                                           │  │
│  │  Startup:                                                 │  │
│  │    sysinfo::detect() → available RAM, VRAM, CPU cores     │  │
│  │    → auto-select model quantization:                      │  │
│  │                                                           │  │
│  │    VRAM ≥ 8GB  → Q4_K_M (7B, 15 GPU layers)             │  │
│  │    VRAM ≥ 4GB  → Q4_K_M (3B, full GPU offload)           │  │
│  │    VRAM < 4GB  → Q2_K  (3B, CPU-only)                    │  │
│  │    RAM  < 4GB  → cloud fallback only                      │  │
│  │                                                           │  │
│  │  Runtime routing:                                         │  │
│  │    has_image? → vision model (Qwen2.5-VL)                │  │
│  │    complex?   → larger model or cloud                     │  │
│  │    simple?    → smallest loaded model                     │  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                 │
│  ┌────────────────────┐  ┌─────────────────────────────────┐  │
│  │   Local Inference   │  │       Cloud Fallback            │  │
│  │                     │  │                                 │  │
│  │  llama-cpp-rs       │  │  reqwest → OpenAI-compatible    │  │
│  │  • GGUF loading     │  │  • Gemini / GPT / Claude       │  │
│  │  • CUDA / Vulkan    │  │  • Groq / OpenRouter           │  │
│  │    / Metal GPU      │  │  • Rate limiting                │  │
│  │  • KV cache mgmt    │  │  • API key management           │  │
│  │  • Context window   │  │  • Automatic fallback on        │  │
│  │    overflow trim    │  │    local model failure           │  │
│  │  • Streaming tokens │  │  • Streaming tokens             │  │
│  └────────────────────┘  └─────────────────────────────────┘  │
│                                                                 │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │                  Model Manager                            │  │
│  │  • Download models from HuggingFace (resumable)           │  │
│  │  • SHA256 verification after download                     │  │
│  │  • List / delete / switch models at runtime               │  │
│  │  • Auto-detect new GGUF files in models directory         │  │
│  └──────────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────┘
```

### 3. Voice Pipeline

```
┌────────────────────────────────────────────────────────────────┐
│                      Voice Pipeline                             │
│                                                                 │
│  ┌─────────┐    ┌──────────┐    ┌──────┐    ┌──────────────┐  │
│  │   cpal   │───▶│ silero   │───▶│ VAD  │───▶│  whisper-rs  │  │
│  │ capture  │    │   VAD    │    │ gate │    │    STT       │  │
│  │ (16kHz)  │    │ (ONNX)   │    │      │    │ (GPU accel)  │  │
│  └─────────┘    └──────────┘    └──────┘    └──────┬───────┘  │
│                                                      │          │
│                                          transcribed text       │
│                                                      │          │
│                                                      ▼          │
│                                               Agent Engine      │
│                                                      │          │
│                                              response text      │
│                                                      │          │
│                                                      ▼          │
│  ┌──────────┐    ┌───────────┐    ┌────────────────────────┐   │
│  │  rodio    │◀──│  audio    │◀───│     piper TTS           │   │
│  │ playback  │   │  buffer   │    │ • 22kHz ONNX voice      │   │
│  │ (speaker) │   │ (stream)  │    │ • multiple voices       │   │
│  └──────────┘    └───────────┘    │ • streaming synthesis   │   │
│                                    └────────────────────────┘   │
│                                                                 │
│  Modes:                                                         │
│  • Push-to-talk (global hotkey)                                 │
│  • Wake word ("Hey KRIA") — always listening (future)           │
│  • Continuous conversation — VAD auto-segments                  │
│  • Text-only — voice engine disabled, zero resource use         │
└────────────────────────────────────────────────────────────────┘
```

### 4. Memory System

```
┌────────────────────────────────────────────────────────────────┐
│                      Memory System                              │
│                                                                 │
│   ~/.kria/kria.db (SQLite)           ~/.kria/vectors.usearch    │
│  ┌────────────────────────┐         ┌───────────────────────┐  │
│  │ conversations           │         │  usearch HNSW index   │  │
│  │ ├─ id, session_id       │         │  • 384-dim float32    │  │
│  │ ├─ role, content        │         │  • mmap'd from disk   │  │
│  │ ├─ timestamp            │         │  • ~1ms ANN search    │  │
│  │ └─ token_count          │         │  • auto-persisted     │  │
│  │                         │         └───────────┬───────────┘  │
│  │ memory_facts            │                     │              │
│  │ ├─ id, text             │◀── fact_id FK ──────┘              │
│  │ ├─ category             │                                    │
│  │ ├─ source (user/inferred)│                                   │
│  │ ├─ created_at           │                                    │
│  │ ├─ last_accessed        │                                    │
│  │ ├─ access_count         │                                    │
│  │ └─ decay_score (computed)│                                   │
│  │                         │                                    │
│  │ memory_links            │                                    │
│  │ ├─ fact_a_id            │  Relational links between facts:  │
│  │ ├─ fact_b_id            │  "prefers dark theme" ↔            │
│  │ ├─ relation_type        │  "uses Sublime Text" ↔             │
│  │ └─ strength (0.0-1.0)   │  "is a developer"                 │
│  │                         │                                    │
│  │ preferences             │                                    │
│  │ ├─ key                  │                                    │
│  │ └─ value (JSON)         │                                    │
│  │                         │                                    │
│  │ audit_log               │  Immutable record of all tool     │
│  │ ├─ timestamp            │  executions + HITL decisions       │
│  │ ├─ tool_name, args      │                                    │
│  │ ├─ safety_tier          │                                    │
│  │ ├─ approved_by          │                                    │
│  │ └─ result_summary       │                                    │
│  └─────────────────────────┘                                    │
│                                                                 │
│  Retrieval query:                                               │
│   1. embed(user_message) → query vector (Rust: fastembed-rs)    │
│   2. usearch.search(query, k=20) → candidate fact_ids          │
│   3. SELECT * FROM memory_facts                                 │
│      LEFT JOIN memory_links ON ...                              │
│      WHERE id IN (candidates) AND decay_score > 0.1            │
│      ORDER BY (similarity * 0.5)                                │
│            + (recency * 0.25)                                   │
│            + (frequency * 0.15)                                 │
│            + (link_strength * 0.1)                              │
│      LIMIT 5                                                    │
│                                                                 │
│  RAG Ingestion (via Python sidecar):                            │
│   1. User uploads document → Rust intercepts                    │
│   2. SidecarBridge::document.extract(file) → structured text   │
│   3. SidecarBridge::embeddings.chunk_and_embed(text)            │
│      → {chunks: [...], vectors: [[...], ...]}                   │
│   4. Rust stores chunks + vectors in SQLite + usearch           │
│   5. Ownership and lifecycle managed entirely by Rust           │
└────────────────────────────────────────────────────────────────┘
```

### 5. Tool System

```
┌────────────────────────────────────────────────────────────────┐
│                       Tool Registry                             │
│                                                                 │
│  CATEGORY          TOOLS                          SAFETY TIER   │
│  ─────────────────────────────────────────────────────────────  │
│                                                                 │
│  System Info       get_cpu_usage                   GREEN        │
│                    get_memory_info                  GREEN        │
│                    get_disk_space                   GREEN        │
│                    get_battery_status               GREEN        │
│                    get_network_status               GREEN        │
│                    get_time                         GREEN        │
│                    get_public_ip                    GREEN        │
│                    get_environment_variable         GREEN        │
│                                                                 │
│  App Control       open_application                GREEN        │
│                    close_application               YELLOW       │
│                    list_running_apps               GREEN        │
│                    check_app_installed             GREEN        │
│                                                                 │
│  App Lifecycle     search_package                  GREEN        │
│                    install_application             RED          │
│                    uninstall_application           RED          │
│                    check_updates_available         GREEN        │
│                    snap_install/remove/search      RED/GREEN    │
│                    flatpak_install/remove/search   RED/GREEN    │
│                                                                 │
│  File Ops          read_file                       GREEN        │
│                    list_directory                  GREEN        │
│                    search_files                    GREEN        │
│                    write_file                      YELLOW       │
│                    create_directory                YELLOW       │
│                    rename_file                     YELLOW       │
│                    clear_file                      YELLOW       │
│                    delete_file                     RED          │
│                    delete_directory                RED          │
│                    move_file                       RED          │
│                                                                 │
│  Internet          web_search                      GREEN        │
│                    deep_search                     GREEN        │
│                    fetch_webpage                   GREEN        │
│                    download_file                   YELLOW       │
│                    get_weather                     GREEN        │
│                    get_news                        GREEN        │
│                    rss_feed_read                   GREEN        │
│                                                                 │
│  Communication     send_notification               GREEN        │
│                    get_clipboard / set_clipboard    GREEN/YELLOW │
│                    schedule_reminder               YELLOW       │
│                                                                 │
│  Knowledge         remember_fact                   YELLOW       │
│                    recall_fact                     GREEN        │
│                    search_knowledge                GREEN        │
│                                                                 │
│  Shell             execute_shell                   RED          │
│                                                                 │
│  Power             lock_screen                     YELLOW       │
│                    shutdown_system                 RED          │
│                    reboot_system                   RED          │
│                                                                 │
│  Math              calculate                       GREEN        │
│                    unit_convert                    GREEN        │
│                                                                 │
│  Interaction       ask_user                        GREEN        │
│                                                                 │
│  ── FUTURE / SCALABILITY ──────────────────────────────────    │
│                                                                 │
│  Automation        create_macro                    YELLOW       │
│                    run_macro                       RED          │
│                    create_scheduled_task           YELLOW       │
│                    create_workflow                 YELLOW       │
│                                                                 │
│  Screen/Vision     screenshot                      GREEN        │
│                    screen_ocr                      GREEN        │
│                    describe_screen                 GREEN        │
│                                                                 │
│  Email             compose_email                   YELLOW       │
│                    read_inbox                      GREEN        │
│                                                                 │
│  Calendar          create_event                    YELLOW       │
│                    list_events                     GREEN        │
│                                                                 │
│  Smart Home        control_device (IoT)            YELLOW       │
│                    list_devices                    GREEN        │
│                                                                 │
│  Plugins           load_plugin                     YELLOW       │
│                    unload_plugin                   YELLOW       │
│                    list_plugins                    GREEN        │
│                                                                 │
│  ── PYTHON SIDECAR TOOLS ──────────────────────────────────    │
│  (Routed via SidecarBridge, executed in Python process)         │
│                                                                 │
│  Pre-Cognitive     image_analyze                   GREEN        │
│                    image_ocr                       GREEN        │
│                    document_extract                GREEN        │
│                    document_summarize              GREEN        │
│                    code_analyze_ast                GREEN        │
│                    code_dependency_graph           GREEN        │
│                    web_extract_article             GREEN        │
│                    audio_preprocess                GREEN        │
│                    embeddings_generate             GREEN        │
│                    rag_ingest_document             YELLOW       │
│                    rag_query                       GREEN        │
│                                                                 │
│  Skills            skill_* (dynamically registered) per-skill  │
│  (Plugin dir)      from ~/.kria/skills/*/skill.json            │
└────────────────────────────────────────────────────────────────┘
```

### 6. Communication Layer (Desktop vs Server)

```
┌──────────── DESKTOP MODE ─────────────┐  ┌──────── SERVER MODE ──────────────┐
│                                        │  │                                    │
│  WebView                               │  │  Browser / Mobile / Remote App     │
│    │                                   │  │    │                               │
│    │ Tauri invoke("tool_name", args)   │  │    │ HTTP POST /api/v1/chat        │
│    │ Tauri event listen/emit           │  │    │ WebSocket wss://host/ws       │
│    │                                   │  │    │                               │
│    ▼                                   │  │    ▼                               │
│  Tauri IPC (zero-copy, <1μs)           │  │  Axum router                      │
│    │                                   │  │    │                               │
│    ▼                                   │  │    │  + Auth middleware (JWT)       │
│  kria-core                             │  │    │  + Rate limiting              │
│                                        │  │    │  + Multi-user sessions        │
│  HITL:                                 │  │    ▼                               │
│    Native OS dialog                    │  │  kria-core                         │
│    (rfd crate → system modal)          │  │                                    │
│                                        │  │  HITL:                             │
│  Notifications:                        │  │    WebSocket push to client        │
│    OS notification center              │  │    Client sends approve/deny       │
│                                        │  │                                    │
│  Voice:                                │  │  Voice:                            │
│    Local mic → whisper-rs → LLM        │  │    Client streams audio via WS     │
│    LLM → piper TTS → local speaker     │  │    Server runs STT/TTS             │
│                                        │  │    Returns audio stream via WS     │
└────────────────────────────────────────┘  └────────────────────────────────────┘
```

---

## Directory Structure

```
kria/
├── Cargo.toml                         # Workspace definition
├── config/
│   └── default.toml                   # Default configuration
│
├── crates/
│   ├── kria-core/                     # Sovereign Core — Rust shared library (80% of logic)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                 # Public API
│   │       ├── agent/
│   │       │   ├── mod.rs
│   │       │   ├── router.rs          # Intent classification (+ PREPROCESS intent)
│   │       │   ├── loop_engine.rs     # ReAct loop (calls sidecar for pre-processing)
│   │       │   ├── planner.rs         # Multi-step plan generation
│   │       │   ├── prompts.rs         # System prompt builder
│   │       │   ├── response_parser.rs # Tool call extraction
│   │       │   └── interaction.rs     # Session/turn recording
│   │       ├── llm/
│   │       │   ├── mod.rs
│   │       │   ├── local.rs           # llama-cpp-rs backend
│   │       │   ├── cloud.rs           # OpenAI-compatible client
│   │       │   ├── model_router.rs    # Auto-select model (tier-aware)
│   │       │   └── model_manager.rs   # Download, verify, switch
│   │       ├── sidecar/               # ← NEW: Rust↔Python bridge
│   │       │   ├── mod.rs
│   │       │   ├── bridge.rs          # SidecarBridge: spawn, IPC, lifecycle
│   │       │   ├── protocol.rs        # JSON-RPC 2.0 request/response types
│   │       │   ├── health.rs          # Heartbeat, crash recovery, restart
│   │       │   └── tier_config.rs     # Hardware-tier → processing quality mapping
│   │       ├── voice/
│   │       │   ├── mod.rs
│   │       │   ├── capture.rs         # cpal microphone
│   │       │   ├── vad.rs             # silero-vad
│   │       │   ├── stt.rs             # whisper-rs
│   │       │   ├── tts.rs             # piper
│   │       │   └── playback.rs        # rodio
│   │       ├── tools/
│   │       │   ├── mod.rs
│   │       │   ├── registry.rs        # Tool registration + schema (incl. sidecar tools)
│   │       │   ├── file_ops.rs
│   │       │   ├── app_lifecycle.rs
│   │       │   ├── system_info.rs
│   │       │   ├── internet.rs
│   │       │   ├── shell.rs
│   │       │   ├── interaction.rs     # ask_user, clipboard, screenshot
│   │       │   └── precognitive.rs    # ← NEW: tools that delegate to Python sidecar
│   │       ├── memory/
│   │       │   ├── mod.rs
│   │       │   ├── store.rs           # SQLite operations
│   │       │   ├── vectors.rs         # usearch index
│   │       │   ├── retrieval.rs       # Hybrid search + scoring
│   │       │   └── decay.rs           # Memory aging
│   │       ├── safety/
│   │       │   ├── mod.rs
│   │       │   ├── policy.rs          # GREEN/YELLOW/RED/BLACK
│   │       │   ├── hitl.rs            # Approval gateway (trait)
│   │       │   ├── audit.rs           # Logging
│   │       │   ├── rollback.rs        # Undo snapshots
│   │       │   └── blacklist.rs       # Hardcoded blocks
│   │       ├── preprocessing/         # Lightweight Rust-native preprocessing
│   │       │   ├── mod.rs
│   │       │   ├── token_budget.rs    # Context window management
│   │       │   ├── image.rs           # Basic image info (Rust-native, fast path)
│   │       │   ├── document.rs        # Text file reading (Rust-native, fast path)
│   │       │   ├── code.rs            # Basic code info (Rust-native, fast path)
│   │       │   └── web.rs             # Basic HTML extraction (Rust-native, fast path)
│   │       ├── platform/
│   │       │   ├── mod.rs
│   │       │   ├── detect.rs          # OS, arch, pkg manager, GPU
│   │       │   └── paths.rs           # Home dir, config dir, data dir
│   │       ├── automation/
│   │       │   ├── mod.rs
│   │       │   ├── scheduler.rs       # Cron-like task scheduler
│   │       │   ├── workflow.rs        # Chained tool sequences
│   │       │   └── macro_recorder.rs  # Record + replay actions
│   │       ├── infra/
│   │       │   ├── mod.rs
│   │       │   └── event_bus.rs       # ← NEW: tokio::broadcast EventBus
│   │       └── plugin/
│   │           ├── mod.rs
│   │           ├── runtime.rs         # Manifest discovery + loading
│   │           └── skill_registry.rs  # ← NEW: discover + register skills from Python
│   │
│   ├── kria-desktop/                  # Tauri v2 entry point
│   │   ├── Cargo.toml
│   │   ├── tauri.conf.json            # Window, tray, permissions, sidecar bundle
│   │   ├── capabilities/              # Tauri v2 permission model
│   │   ├── icons/
│   │   └── src/
│   │       ├── main.rs                # Tauri bootstrap (spawns sidecar)
│   │       ├── commands.rs            # IPC handlers (invoke targets)
│   │       ├── tray.rs                # System tray menu
│   │       └── hitl_desktop.rs        # Native dialog HITL impl
│   │
│   └── kria-server/                   # Axum server entry point
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs                # Axum bootstrap (spawns sidecar)
│           ├── routes.rs              # REST API endpoints
│           ├── ws.rs                  # WebSocket handler
│           ├── auth.rs                # JWT authentication
│           └── hitl_server.rs         # WebSocket HITL impl
│
├── kria-modules/                      # ← NEW: Python sidecar package
│   ├── pyproject.toml                 # Python project config (uv-managed)
│   ├── uv.lock                        # Lockfile for reproducible installs
│   ├── README.md
│   └── src/
│       └── kria_modules/
│           ├── __init__.py
│           ├── bridge.py              # JSON-RPC stdio dispatcher (entry point)
│           ├── config.py              # Hardware tier config, quality presets
│           ├── processors/
│           │   ├── __init__.py
│           │   ├── image.py           # OpenCV + Pillow + pytesseract + easyocr
│           │   ├── document.py        # PyMuPDF + python-docx + pandas
│           │   ├── code.py            # tree-sitter + ast module
│           │   ├── web.py             # trafilatura + readability-lxml
│           │   ├── audio.py           # librosa + webrtcvad + noisereduce
│           │   └── embeddings.py      # sentence-transformers, chunking
│           ├── utils/
│           │   ├── __init__.py
│           │   ├── token_budget.py    # tiktoken-based accurate token counting
│           │   └── sanitizer.py       # Input validation, path traversal guards
│           └── skills/
│               ├── __init__.py
│               └── loader.py          # Discover + import skills from ~/.kria/skills/
│
├── ui/                                # Shared frontend
│   ├── package.json
│   ├── vite.config.ts
│   ├── index.html
│   └── src/
│       ├── main.ts
│       ├── chat/                      # Chat interface
│       ├── settings/                  # Settings panel
│       ├── hitl/                      # Approval dialogs
│       └── dashboard/                 # System monitoring
│
├── models/                            # Git-ignored, downloaded at runtime
│   ├── llm/
│   ├── stt/
│   ├── tts/
│   └── embeddings/
│
├── scripts/
│   ├── setup.sh                       # Full setup: Rust deps + Python venv + models
│   ├── setup_python.sh                # ← NEW: Python sidecar setup via uv
│   └── download_models.py
│
└── .github/
    └── workflows/
        └── release.yml                # Cross-platform CI/CD (bundles Python sidecar)
```

---

## Feature Roadmap

### Has (Phase 1 — Core)
- [x] ReAct agent loop with tool calling
- [x] Local LLM inference (GGUF, GPU offload)
- [x] Cloud LLM fallback (OpenAI, Gemini, Groq)
- [x] Adaptive model selection based on hardware
- [x] Voice: push-to-talk capture → STT → TTS → playback
- [x] File operations (read, write, search, create, delete, move, clear)
- [x] App lifecycle (search, install, uninstall, check installed)
- [x] System info (CPU, RAM, disk, battery, network)
- [x] Shell execution with safety gates
- [x] Web search, deep search, weather, news
- [x] Memory: SQLite + usearch hybrid retrieval with decay
- [x] Safety: GREEN/YELLOW/RED/BLACK tiers
- [x] HITL: native dialog (desktop) / WebSocket (server)
- [x] Audit log for all tool executions
- [x] Cross-platform: Windows + Linux + macOS
- [x] Auto-updater with differential updates
- [x] System tray with quick actions
- [x] Settings panel (model selection, voice, keybindings)
- [x] Server mode from same codebase
- [x] Python sidecar with JSON-RPC bridge
- [x] Pre-cognitive processing: image, document, code, web, audio
- [x] Hardware-tier–aware processing depth
- [x] Skill-plugin ecosystem with isolated venvs
- [x] EventBus for decoupled module communication

### Can Have (Phase 2 — Enhancement)
- [ ] Wake word detection ("Hey KRIA")
- [ ] Continuous conversation mode (VAD auto-segment)
- [ ] Vision: screenshot → multimodal LLM → describe/act
- [ ] Screen OCR for reading visible text
- [ ] Macro recorder (record + replay action sequences)
- [ ] Workflow engine (chained tool sequences)
- [ ] Scheduled tasks (cron-like)
- [ ] Multiple voice profiles and languages
- [ ] Conversation branching (fork a chat)
- [ ] Export conversations (markdown, PDF)
- [ ] Plugin system (WASM sandboxed extensions)
- [ ] Email integration (read inbox, compose drafts)
- [ ] Calendar integration (create events, reminders)
- [ ] Smart home / IoT control
- [ ] Multi-monitor aware screen control
- [ ] File watcher (monitor directories for changes)
- [ ] Clipboard history with semantic search
- [ ] RAG: ingest documents → chunk → embed → query (via Python sidecar)
- [ ] Multi-user server mode with per-user memory
- [ ] Mobile companion app (Flutter/React Native → same Axum API)
- [ ] Community skill marketplace (discover + install skills from URL)

---

## Resource Scaling

```
LOW-END (4GB RAM, no GPU) — "Lite" Tier
├── Model: Phi-3.5 Mini Q2_K (1.5GB RAM)
├── STT: whisper-tiny (CPU, ~500ms/utterance)
├── TTS: piper low-quality voice
├── Python sidecar: ~60MB (minimal deps, aggressive compression)
│   ├── OCR: pytesseract only (no easyocr — too heavy)
│   ├── Documents: plain text only, first 5 pages
│   ├── Embeddings: disabled (hash-based Rust fallback)
│   └── Code analysis: regex-only (no tree-sitter)
├── Tauri shell: ~30MB
├── SQLite + usearch: ~10MB
├── Total: ~1.7GB ← leaves 2.3GB for OS
└── Fallback: cloud LLM for complex queries

MID-RANGE (8GB RAM, 4GB VRAM) — "Standard" Tier
├── Model: Qwen2.5 3B Q4_K_M (2GB VRAM, full GPU)
├── STT: whisper-small (GPU, ~200ms/utterance)
├── TTS: piper high-quality voice
├── Python sidecar: ~150MB (full processors, CPU embeddings)
│   ├── OCR: pytesseract + basic easyocr
│   ├── Documents: full extraction, tables as text
│   ├── Embeddings: MiniLM CPU (real-time query via Rust ort)
│   └── Code analysis: tree-sitter AST, top-level functions
├── Total: ~2.3GB VRAM + ~350MB RAM
└── Fast local inference + decent pre-processing

HIGH-END (16GB+ RAM, 8GB+ VRAM) — "Performance"/"High" Tier
├── Model: Qwen2.5 7B Q4_K_M (5GB VRAM, 15 layers)
├── Secondary: Phi-4 Mini for fast routing
├── Vision: Qwen2.5-VL-7B (swapped in on demand)
├── STT: whisper-medium (GPU, ~100ms/utterance)
├── TTS: piper high-quality, multiple voices
├── Python sidecar: ~300MB (full processors, GPU embeddings)
│   ├── OCR: easyocr GPU + pytesseract
│   ├── Documents: deep extraction + table structure + images
│   ├── Embeddings: GPU-accelerated batch encoding
│   └── Code analysis: full AST + dependency graph + semantic
├── Total: ~6GB VRAM + ~600MB RAM
└── Full capabilities, minimal latency

SERVER (24GB+ VRAM)
├── Model: 13B+ or multiple 7B concurrent
├── Multi-user sessions (each with own sidecar worker pool)
├── Python sidecar: multi-worker mode (1 worker per concurrent user)
├── Whisper-large for best transcription
├── All features enabled
└── Horizontal: load balancer → N instances
```

---

This is the complete Sovereign-Orchestrator architecture. One Rust workspace, one Python sidecar, two entry points, pre-cognitive processing before the LLM. Runs on anything from a 4GB laptop to a multi-GPU server.