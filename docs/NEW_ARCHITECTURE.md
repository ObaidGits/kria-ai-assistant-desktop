---

# K.R.I.A. — Final Architecture

## System Overview

```
One Rust workspace. Two entry points. Zero external services.

┌─────────────────────────────────────────────────────────────────────┐
│                        KRIA Workspace                               │
│                                                                     │
│  ┌─────────────┐  ┌──────────────┐  ┌───────────────────────────┐ │
│  │ kria-desktop │  │ kria-server  │  │        kria-core          │ │
│  │  (Tauri v2)  │  │  (Axum)      │  │     (shared library)      │ │
│  │              │  │              │  │                           │ │
│  │ • Window     │  │ • HTTP API   │  │ • Agent engine            │ │
│  │ • Tray icon  │  │ • WebSocket  │  │ • LLM inference           │ │
│  │ • IPC bridge │  │ • Auth layer │  │ • Voice pipeline          │ │
│  │ • Auto-start │  │ • Static UI  │  │ • Tool system             │ │
│  │ • Installer  │  │ • Multi-user │  │ • Memory & knowledge      │ │
│  └──────┬───────┘  └──────┬───────┘  │ • Safety & HITL           │ │
│         │                  │          │ • Plugin runtime          │ │
│         └──────────────────┴──────────┤                           │ │
│                 depends on            └───────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────┘
```

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

### AI / Inference

| Component | Technology | Purpose |
|---|---|---|
| Local LLM | **llama-cpp-rs** (bindings to llama.cpp) | GGUF model loading, GPU offload (CUDA/Vulkan/Metal), KV cache, context management |
| Cloud LLM fallback | **reqwest** + OpenAI-compatible API | Gemini, GPT, Claude, Groq, OpenRouter — same API shape |
| Embeddings | **fastembed-rs** or **candle** | Local sentence embeddings (all-MiniLM-L6-v2, 384-dim) for memory search |
| Vision (future) | **llama-cpp-rs** multimodal | Qwen2.5-VL, LLaVA — image understanding through same backend |

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

### Distribution & Updates

| Feature | Technology | Platforms |
|---|---|---|
| Windows installer | **NSIS** or **WiX** (Tauri built-in) | `.exe` / `.msi` |
| Linux packages | **AppImage** + **deb** + **rpm** (Tauri built-in) | Universal + distro-specific |
| macOS bundle | **DMG** with notarization (Tauri built-in) | `.dmg` / `.app` |
| Auto-updater | **Tauri updater plugin** | Differential updates, code-signed |
| CI/CD | **GitHub Actions** | Cross-compile for all 3 platforms per commit |

---

## Detailed Architecture

### 1. Agent Engine

```
┌────────────────────────────────────────────────────────────────┐
│                        Agent Engine                             │
│                                                                 │
│  User Input (text or transcribed voice)                         │
│       │                                                         │
│       ▼                                                         │
│  ┌─────────┐    ┌──────────────────────────────────────────┐   │
│  │ Router  │───▶│ Intent Classification                     │   │
│  │         │    │ • DIRECT_TOOL  (single action)            │   │
│  │         │    │ • AGENT_LOOP   (multi-step reasoning)     │   │
│  │         │    │ • CONVERSATION (no tools needed)          │   │
│  └─────────┘    └──────────────┬───────────────────────────┘   │
│                                │                                │
│       ┌────────────────────────┼────────────────────┐          │
│       │                        │                    │          │
│       ▼                        ▼                    ▼          │
│  DIRECT_TOOL              AGENT_LOOP           CONVERSATION    │
│  • Lite tool set          • Full tool set      • No tools      │
│  • 1 iteration            • ReAct loop (≤10)   • Direct LLM    │
│  • No thinking            • CoT reasoning      • response      │
│       │                        │                               │
│       └────────┬───────────────┘                               │
│                ▼                                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │                    Tool Executor                          │  │
│  │  1. Parse tool call from LLM output                      │  │
│  │  2. Safety policy check (GREEN/YELLOW/RED/BLACK)         │  │
│  │  3. HITL approval if RED                                  │  │
│  │  4. Execute tool function                                 │  │
│  │  5. Truncate result for context window                    │  │
│  │  6. Feed result back to LLM                               │  │
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
│   1. embed(user_message) → query vector                         │
│   2. usearch.search(query, k=20) → candidate fact_ids          │
│   3. SELECT * FROM memory_facts                                 │
│      LEFT JOIN memory_links ON ...                              │
│      WHERE id IN (candidates) AND decay_score > 0.1            │
│      ORDER BY (similarity * 0.5)                                │
│            + (recency * 0.25)                                   │
│            + (frequency * 0.15)                                 │
│            + (link_strength * 0.1)                              │
│      LIMIT 5                                                    │
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
│   ├── kria-core/                     # Shared library (80% of code)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                 # Public API
│   │       ├── agent/
│   │       │   ├── mod.rs
│   │       │   ├── router.rs          # Intent classification
│   │       │   ├── loop.rs            # ReAct loop
│   │       │   ├── prompts.rs         # System prompt builder
│   │       │   └── response_parser.rs # Tool call extraction
│   │       ├── llm/
│   │       │   ├── mod.rs
│   │       │   ├── local.rs           # llama-cpp-rs backend
│   │       │   ├── cloud.rs           # OpenAI-compatible client
│   │       │   ├── model_router.rs    # Auto-select model
│   │       │   └── model_manager.rs   # Download, verify, switch
│   │       ├── voice/
│   │       │   ├── mod.rs
│   │       │   ├── capture.rs         # cpal microphone
│   │       │   ├── vad.rs             # silero-vad
│   │       │   ├── stt.rs             # whisper-rs
│   │       │   ├── tts.rs             # piper
│   │       │   └── playback.rs        # rodio
│   │       ├── tools/
│   │       │   ├── mod.rs
│   │       │   ├── registry.rs        # Tool registration + schema
│   │       │   ├── file_ops.rs
│   │       │   ├── app_lifecycle.rs
│   │       │   ├── system_info.rs
│   │       │   ├── internet.rs
│   │       │   ├── shell.rs
│   │       │   └── interaction.rs     # ask_user
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
│   │       ├── platform/
│   │       │   ├── mod.rs
│   │       │   ├── detect.rs          # OS, arch, pkg manager
│   │       │   └── paths.rs           # Home dir, config dir, data dir
│   │       └── plugin/                # Future: dynamic tool loading
│   │           ├── mod.rs
│   │           └── runtime.rs         # WASM or dynamic lib plugins
│   │
│   ├── kria-desktop/                  # Tauri v2 entry point
│   │   ├── Cargo.toml
│   │   ├── tauri.conf.json            # Window, tray, permissions
│   │   ├── capabilities/              # Tauri v2 permission model
│   │   ├── icons/
│   │   └── src/
│   │       ├── main.rs                # Tauri bootstrap
│   │       ├── commands.rs            # IPC handlers (invoke targets)
│   │       ├── tray.rs                # System tray menu
│   │       └── hitl_desktop.rs        # Native dialog HITL impl
│   │
│   └── kria-server/                   # Axum server entry point
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs                # Axum bootstrap
│           ├── routes.rs              # REST API endpoints
│           ├── ws.rs                  # WebSocket handler
│           ├── auth.rs                # JWT authentication
│           └── hitl_server.rs         # WebSocket HITL impl
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
└── .github/
    └── workflows/
        └── release.yml                # Cross-platform CI/CD
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
- [ ] RAG: ingest documents → chunk → embed → query
- [ ] Multi-user server mode with per-user memory
- [ ] Mobile companion app (Flutter/React Native → same Axum API)

---

## Resource Scaling

```
LOW-END (4GB RAM, no GPU)
├── Model: Phi-3.5 Mini Q2_K (1.5GB RAM)
├── STT: whisper-tiny (CPU, ~500ms/utterance)
├── TTS: piper low-quality voice
├── Tauri shell: ~30MB
├── SQLite + usearch: ~10MB
├── Total: ~1.6GB ← leaves 2.4GB for OS
└── Fallback: cloud LLM for complex queries

MID-RANGE (8GB RAM, 4GB VRAM)
├── Model: Qwen2.5 3B Q4_K_M (2GB VRAM, full GPU)
├── STT: whisper-small (GPU, ~200ms/utterance)
├── TTS: piper high-quality voice
├── Total: ~2.1GB VRAM + ~200MB RAM
└── Fast local inference for everything

HIGH-END (16GB+ RAM, 8GB+ VRAM)
├── Model: Qwen2.5 7B Q4_K_M (5GB VRAM, 15 layers)
├── Secondary: Phi-4 Mini for fast routing
├── Vision: Qwen2.5-VL-7B (swapped in on demand)
├── STT: whisper-medium (GPU, ~100ms/utterance)
├── TTS: piper high-quality, multiple voices
├── Total: ~6GB VRAM + ~400MB RAM
└── Full capabilities, minimal latency

SERVER (24GB+ VRAM)
├── Model: 13B+ or multiple 7B concurrent
├── Multi-user sessions
├── Whisper-large for best transcription
├── All features enabled
└── Horizontal: load balancer → N instances
```

---

This is the complete architecture. One Rust workspace, two entry points, zero external services, runs on anything from a 4GB laptop to a multi-GPU server.