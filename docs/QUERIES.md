# K.R.I.A. — Frequently Asked Questions & Design Queries

> This document records all design queries raised during development and their detailed answers.
> It serves as a decision log and FAQ for anyone reviewing the project.

| Field | Detail |
|---|---|
| **Document Version** | 1.0.0 |
| **Date** | April 2026 |
| **Developer** | Obaidullah Zeeshan |

---

## Table of Contents

1. [Is the plan fully open source and free?](#q1--is-the-plan-fully-open-source-and-free)
2. [What features will the final software have?](#q2--what-features-will-the-final-software-have)
3. [Does KRIA support both Windows and Linux?](#q3--does-kria-support-both-windows-and-linux)
4. [Is the platform multilingual?](#q4--is-the-platform-multilingual)
5. [Is there a memory or learning system?](#q5--is-there-a-memory-or-learning-system)
6. [What data is being stored?](#q6--what-data-is-being-stored)
7. [Docker vs Direct — does it impact performance?](#q7--docker-vs-direct--does-it-impact-performance)
8. [Should OpenClaw be integrated?](#q8--should-openclaw-be-integrated)
9. [How can the project be enhanced further?](#q9--how-can-the-project-be-enhanced-further)
10. [Can multiple LLM models be used with dynamic switching?](#q10--can-multiple-llm-models-be-used-with-dynamic-switching)

---

## Q1 — Is the plan fully open source and free?

**Answer: Yes — 100%. Every component is open source and free.**

### Core AI Models

| Component | License | Cost |
|---|---|---|
| Qwen3-8B MoE (LLM brain) | Apache 2.0 | Free |
| Qwen3-0.6B (draft model) | Apache 2.0 | Free |
| Whisper.cpp (speech-to-text) | MIT | Free |
| Piper TTS (text-to-speech) | MIT | Free |
| OpenWakeWord (wake word) | Apache 2.0 | Free |
| Silero VAD (voice activity detection) | MIT | Free |
| nomic-embed-text (embeddings) | Apache 2.0 | Free |

### Inference Engines

| Component | License | Cost |
|---|---|---|
| llama.cpp (LLM server) | MIT | Free |
| whisper.cpp (STT server) | MIT | Free |

### Infrastructure

| Component | License | Cost |
|---|---|---|
| Python 3.12+ | PSF | Free |
| FastAPI | MIT | Free |
| Redis | BSD-3 | Free |
| SQLite | Public Domain | Free |
| ChromaDB | Apache 2.0 | Free |
| Docker | Apache 2.0 | Free |

### Internet / Web Tools — No API Keys Required

| Data Source | Method | Cost |
|---|---|---|
| Web Search | DuckDuckGo HTML scraping | Free, no API key |
| Weather | wttr.in JSON API | Free, no API key |
| News | RSS feeds (BBC, Reuters, etc.) | Free, no API key |
| IP/Geo Info | ipinfo.io (50k req/month) | Free tier |
| Content Extraction | trafilatura (Python library) | Free |
| RSS Feeds | feedparser (Python library) | Free |

### Document / File Libraries

| Library | License | Purpose |
|---|---|---|
| PyMuPDF | AGPL-3.0 | PDF parsing |
| python-docx | MIT | DOCX parsing |
| openpyxl | MIT | Excel parsing |
| pandas | BSD-3 | CSV/data analysis |
| pandoc | GPL-2.0 | Document conversion |
| watchdog | Apache 2.0 | File monitoring |
| Pillow | HPND | Image processing |
| pytesseract | Apache 2.0 | OCR (optional) |

> **Note on PyMuPDF:** Uses AGPL-3.0, which requires open-sourcing if you distribute the software commercially. Since K.R.I.A. runs locally as a BTech project (not distributed commercially), this is not an issue. If redistribution is needed later, swap to `pdfplumber` (MIT).

**Bottom line: Zero paid APIs. Zero subscriptions. Zero cloud dependency.**

---

## Q2 — What features will the final software have?

**Answer: K.R.I.A. v2.0 is a complete AI Assistant with 65+ tools across 12 domains.**

### 🧠 Intelligent Conversational AI
- Natural language chat with Qwen3-8B (128K context window)
- Multi-step reasoning via ReAct loop (Think → Act → Observe → Repeat)
- Speculative decoding for 2x faster responses
- Conversation memory across sessions (SQLite + ChromaDB)

### 🎙️ Voice Control
- "Hey KRIA" wake word — always listening, hands-free
- Real-time speech-to-text (GPU-accelerated Whisper)
- Natural-sounding voice responses (Piper neural TTS)
- Sub-500ms latency for simple commands

### 🌐 Internet Access
- Web search (DuckDuckGo — no API key)
- Fetch and extract content from any web page
- Download files with progress tracking and size limits
- Real-time weather, news headlines, public IP info
- RSS/Atom feed reading
- Ping, DNS lookup, traceroute, speed test, WiFi network listing

### 📄 Document Intelligence
- **Read:** PDF, DOCX, XLSX, CSV, Markdown, HTML, JSON, YAML, plain text
- **Summarize:** Any document via LLM
- **Convert:** Between formats (MD→PDF, DOCX→PDF, XLSX→CSV, etc.)
- **OCR:** Extract text from images/screenshots
- **RAG:** Ingest documents into knowledge base, ask questions about them later

### 📁 File Management
- Read, write, copy, move, rename, delete files and directories
- Search files by name/pattern
- Smart rule-based auto-organization (e.g., sort Downloads by file type)
- Directory monitoring — trigger actions when files change
- Find large files, detect duplicates, calculate directory sizes

### 💻 OS-Level System Control
- **System info:** CPU, RAM, disk, network, battery, GPU, uptime
- **Services:** List, start, stop, restart system services
- **Scheduled tasks:** Create/manage cron jobs or Windows Task Scheduler entries
- **Environment variables:** Read and set env vars, edit shell profiles
- **Power:** Shutdown, reboot, sleep, hibernate, lock screen
- **System config:** Volume, brightness, WiFi toggle, power plans

### 📦 Application Management
- Search package repositories (apt, dnf, winget, brew)
- Install applications with a single voice command
- Uninstall/update applications
- Check for available updates
- Open, close, focus running applications

### 💻 Code Execution
- Run Python, Bash, or PowerShell in a sandboxed environment
- Output captured and returned to the conversation
- Sandboxed with timeouts, resource limits, and network isolation

### ⚙️ Automation Engine
- YAML workflows — multi-step automated routines with variables and conditions
- Cron-like scheduling
- Event triggers — react to file changes, app launches, WiFi connections, low battery
- Macro recorder — record a sequence of actions, replay them later

### 🔔 Communication
- Desktop notifications — native Linux/Windows notifications
- Email composer — draft emails, open in email client (never auto-sends)
- Clipboard — read, write, history, transform clipboard content
- Timed reminders

### 📚 Knowledge Base & Learning
- Remember facts — recalled later across sessions
- Document Q&A — ask questions about previously ingested documents
- Code snippets — save and retrieve reusable code/text
- User preferences — learns patterns over time

### 🔌 Plugin System
- Install community-contributed plugins for new capabilities
- Simple format: `plugin.yml` + Python code
- Plugins are sandboxed — can't bypass safety system

### 🛡️ Safety System
- 4-tier risk classification: GREEN → YELLOW → RED → BLACK
- Human-in-the-loop: dangerous actions blocked until approval
- Rollback: automatic backups before destructive actions
- Audit log: every action logged to tamper-proof database
- Emergency stop: "KRIA, halt all"

### 🖥️ Web Dashboard
- Chat interface with real-time WebSocket
- HITL approval popups
- System monitor (CPU/RAM/GPU graphs)
- Audit log viewer, file explorer, workflow editor, plugin manager, settings

### 🐳 Docker Deployment
- One-command startup: `docker compose up -d`
- GPU passthrough for NVIDIA RTX
- Works on Linux, Windows (WSL2), and Docker Desktop

---

## Q3 — Does KRIA support both Windows and Linux?

**Answer: Yes — both are first-class platforms.**

### Cross-Platform Strategy

Every OS-specific tool has two code paths that branch automatically:

| Operation | Linux | Windows |
|---|---|---|
| Open app | `xdg-open` / subprocess | `os.startfile()` / pywin32 COM |
| List/kill processes | psutil (cross-platform) | psutil (cross-platform) |
| Service management | `systemctl` | `Get-Service` / `sc.exe` |
| Scheduled tasks | `crontab` | Task Scheduler COM API |
| Volume/brightness | `pactl` / `brightnessctl` | `pycaw` / WMI |
| Package install | `apt` / `dnf` / `pacman` | `winget` / `choco` |
| Notifications | `notify-send` (dbus) | `win10toast` / Toast API |
| Clipboard | `xclip` | PowerShell `Get-Clipboard` |
| Lock screen | `loginctl lock-session` | `rundll32 user32.dll,LockWorkStation` |
| Shutdown/reboot | `shutdown -h now` | `shutdown /s /t 0` |
| Environment vars | `.bashrc` / `.zshrc` | Registry / PowerShell `$PROFILE` |
| File watching | `inotify` / watchdog | watchdog (ReadDirectoryChangesW) |
| WiFi | `nmcli` / `iwconfig` | `netsh wlan` |

### Identical on Both Platforms (no branching needed)

- LLM reasoning → llama.cpp (cross-platform binary)
- Speech-to-text → whisper.cpp (cross-platform binary)
- TTS → Piper (cross-platform binary)
- File operations → `pathlib` (Python built-in)
- System info → `psutil` (works on both)
- Internet tools → `httpx` + `trafilatura` (pure Python)
- Document parsing → PyMuPDF, python-docx, openpyxl, pandas (pure Python)
- Database → SQLite, Redis, ChromaDB (cross-platform)
- Docker → Native Linux / Docker Desktop on Windows
- Web dashboard → React in browser (platform-independent)

### Deployment Methods

| Method | Linux | Windows |
|---|---|---|
| Docker (recommended) | ✅ Native Docker + nvidia-container-toolkit | ✅ Docker Desktop + WSL2 GPU |
| Manual (no Docker) | ✅ `bash scripts/setup.sh` | ✅ `.\scripts\setup.ps1` |

### The Bridge Architecture

The `kria-bridge` daemon runs on the host machine (outside Docker) and detects the OS at startup, using the correct system calls automatically. This is what enables OS-level tools to work identically regardless of platform.

**Both platforms get the same 65+ tools, same voice pipeline, same dashboard.**

---

## Q4 — Is the platform multilingual?

**Answer: Partially supported by the tech stack, but only English is configured in v1.0.**

### Current State by Component

| Component | Multilingual Capability | Configured In Docs |
|---|---|---|
| **STT (Whisper)** | Supports 99 languages (Hindi, Urdu, Arabic, French, etc.) | English only |
| **LLM (Qwen3-8B)** | Natively supports English, Chinese, Hindi, and 20+ languages | English only |
| **TTS (Piper)** | Voice models for 30+ languages | `en_US-lessac-high` only |
| **Wake Word** | "Hey KRIA" is language-independent (phonetic) | ✅ Works for all |

### How to Enable Multilingual

- **STT:** Change the `language` parameter in Whisper config (or set to `auto-detect`)
- **LLM:** No changes needed — Qwen3 handles Hindi/Urdu natively
- **TTS:** Download additional Piper voice models (e.g., `hi_IN-*` for Hindi) — ~65MB each, free
- **Switching:** Add a `set_language` tool or auto-detect via Whisper

### Supported Languages (if configured)

By the underlying models: English, Hindi, Urdu, Chinese, French, Spanish, Arabic, Japanese, Korean, German, Portuguese, Russian, Italian, Turkish, Vietnamese, Thai, Indonesian, and many more.

**Recommendation:** Adding Hindi voice support is very low effort and high impact for an Indian university BTech project.

---

## Q5 — Is there a memory or learning system?

**Answer: Yes — a 5-tier memory system plus user preference learning.**

### Memory Architecture

| Memory Type | Technology | What It Does |
|---|---|---|
| **Conversation Buffer** | In-memory sliding window (last 20 turns) | Remembers current conversation |
| **Persistent Memory** | SQLite + FTS5 (full-text search) | Searchable conversation history across sessions |
| **Semantic Memory** | ChromaDB vectors | Finds similar past conversations ("remember when I asked about...") |
| **Document Memory** | ChromaDB collection `kria_documents` | RAG — answers questions about ingested documents |
| **Tool Output Cache** | Redis with TTL | Caches recent tool results to avoid redundant calls |

### User Preference Learning

| What It Learns | How |
|---|---|
| Preferred apps | Tracks `open_application` call patterns |
| Common directories | Tracks file operation paths |
| Schedule patterns | Tracks usage times |
| Language style | Analyzes conversation tone |
| Tool preferences | Tracks which tools are used most |

### Fact Store

| Tool | Purpose |
|---|---|
| `remember_fact` | Store: "My project deadline is April 30" |
| `recall_fact` | Retrieve stored facts by keyword |
| `search_knowledge` | Semantic search across all knowledge |
| `list_remembered` | List all stored facts |

### What It Is NOT

The learning system is **pattern tracking and preference storage**, not model retraining. The LLM itself (Qwen3-8B) is not fine-tuned on user data — it uses stored context to personalize responses without modifying model weights.

---

## Q6 — What data is being stored?

**Answer: All data stays local. Here's a complete inventory.**

### SQLite Database (`data/kria.db`)

| Table | Data Stored |
|---|---|
| `audit_log` | Every tool call — timestamp, action, parameters, risk level, decision, result, network URLs |
| `conversations` | Chat history — user messages, KRIA responses, timestamps, session IDs |
| `user_preferences` | Stored facts, learned preferences, custom settings |

### Redis (in-memory, ephemeral)

| Key Pattern | Data | TTL |
|---|---|---|
| `cache:search:*` | Cached web search results | 1 hour |
| `cache:page:*` | Cached web page extracts | 24 hours |
| `cache:tool:*` | Cached tool outputs (system info, etc.) | 60 seconds |
| `pubsub:*` | Inter-service messages | Not persisted |

### ChromaDB (vector database)

| Collection | Data |
|---|---|
| `kria_conversations` | Conversation embeddings for semantic search |
| `kria_documents` | Ingested document chunks (PDFs, DOCX, etc.) for RAG |

### Filesystem

| Location | Data |
|---|---|
| `~/.kria/rollback/` | File backups before destructive operations (72-hour retention) |
| `~/.kria/workflows/` | User-created YAML workflow definitions |
| `~/.kria/plugins/` | Installed plugins |
| `~/.kria/snippets/` | Saved code/text snippets |
| `~/Downloads/kria/` | Downloaded files |
| Clipboard history | Last 20 entries (in-memory only, NOT persisted) |

### What Is NOT Stored (Privacy Guarantees)

- ❌ No telemetry or usage analytics sent externally
- ❌ No cloud backup of any data
- ❌ No file contents sent over the network (unless user explicitly requests web search/download)
- ❌ No credentials, SSH keys, or tokens stored by KRIA
- ❌ Sensitive environment variables are redacted in tool output

---

## Q7 — Docker vs Direct — does it impact performance?

**Answer: Docker adds essentially zero measurable overhead for the AI workload.**

### Docker Performance Overhead

| Component | Docker Overhead | Why |
|---|---|---|
| LLM (llama.cpp) | ~0% | NVIDIA Container Toolkit = direct GPU access |
| STT (whisper.cpp) | ~0% | GPU passthrough, no virtualization |
| TTS (Piper) | ~0% | CPU-bound, native performance in container |
| Redis | ~1-2% | Negligible network namespace overhead |
| ChromaDB | ~1-2% | Negligible |
| Core (FastAPI) | ~0% | Python runs identical in container |
| Audio I/O | ~5-10ms extra | PulseAudio/PipeWire socket mount |
| OS tools (bridge) | 0% | Bridge runs on host, NOT in Docker |

### Key Design Point

The `kria-bridge` daemon (which handles ALL OS interaction — app launching, file ops, system commands) runs directly on the host machine. Docker only contains the AI models and data services. So OS tool performance is identical either way.

### When to Use Which

| Scenario | Recommendation |
|---|---|
| Just want it to work | Docker — one command, all services managed |
| Squeezing every microsecond | Direct — saves ~5-10ms on audio routing |
| Don't have Docker installed | Direct — `SKIP_DOCKER=1 bash scripts/setup.sh` |
| Multiple machines / reproducibility | Docker — portable across setups |
| GPU has limited VRAM (<6GB) | Direct — slightly less memory overhead |

**Recommendation: Docker is recommended. The ~5-10ms audio overhead is invisible in practice.**

---

## Q8 — Should OpenClaw be integrated?

**Answer: No. K.R.I.A. and OpenClaw are competitors, not complements.**

### What OpenClaw Is

OpenClaw (formerly Clawdbot/Moltbot) is an open-source AI agent (Node.js) that executes OS tasks, browses the web, manages files, and automates workflows — accessed via messaging platforms (WhatsApp, Telegram, Discord). Created by Peter Steinberger in late 2025.

### The Overlap Problem

| Capability | K.R.I.A. Has It? | OpenClaw Adds Value? |
|---|---|---|
| File read/write/manage | ✅ Yes (10 tools) | ❌ Duplicate |
| Shell command execution | ✅ Yes (sandboxed) | ❌ Duplicate |
| Web browsing & search | ✅ Yes (web_search, fetch_webpage) | ❌ Duplicate |
| Workflow automation | ✅ Yes (YAML engine) | ❌ Duplicate |
| App management | ✅ Yes (package manager abstraction) | ❌ Duplicate |
| OS-level control | ✅ Yes (services, power, disk, network) | ❌ Duplicate |
| Agentic ReAct loop | ✅ Yes (custom implementation) | ❌ Duplicate |

### Why Integration Would Hurt

| Problem | Explanation |
|---|---|
| **Architecture conflict** | K.R.I.A. = Python + FastAPI. OpenClaw = Node.js. Two runtimes, two agent loops for the same tasks. |
| **Duplicate safety systems** | K.R.I.A. has 4-tier safety. OpenClaw has its own permissions. Conflicts create security gaps. |
| **Cloud dependency** | OpenClaw leans toward cloud LLM APIs (Claude/GPT). K.R.I.A. is fully local. |
| **Security vulnerabilities** | OpenClaw has had CVEs (CVE-2026-25253). Increases attack surface. |
| **BTech originality** | Evaluators will question whether you built K.R.I.A. or plugged in someone else's work. |
| **Unnecessary messaging** | OpenClaw's value is WhatsApp/Telegram/Discord. K.R.I.A. uses voice + dashboard. |

### What To Do Instead

If you want specific OpenClaw-like features (e.g., Telegram access), add them as lightweight K.R.I.A. plugins (~50 lines of Python each) without importing the entire OpenClaw stack.

---

## Q9 — How can the project be enhanced further?

**Answer: Here are the top enhancements ranked by impact and feasibility.**

### Priority 1 — Multi-Language Voice (Hindi)

- **Effort:** Very low — change Whisper config, download Hindi Piper voice (~65MB)
- **Impact:** Massive for Indian university BTech project
- **Everything already supports Hindi** — just needs configuration

### Priority 2 — Screen Vision ("What's on my screen?")

- **Model:** Qwen2.5-VL-3B (GGUF, ~2GB VRAM)
- **How:** screenshot tool → feed image to vision model → get description
- **Use cases:** "Read this error on my screen", "Summarize this chart"
- **Impact:** Evaluators will be blown away

### Priority 3 — Telegram Bot Interface

- **How:** `python-telegram-bot` library — ~100 lines of Python
- **Use cases:** Control KRIA from phone when away from laptop
- **Cost:** Free (Telegram Bot API)

### Priority 4 — Natural Language Workflow Creation

- **How:** Add a `create_workflow` tool — LLM generates YAML from plain English
- **Example:** "Every morning at 9am, check weather and open VS Code" → auto-generates workflow
- **Shows the agent can program itself**

### Priority 5 — Daily Briefing + Context Awareness

- **Context signals:** Time, day, battery, WiFi, running apps, idle time
- **Daily briefing:** Weather, disk space, reminders, new files
- **Makes KRIA feel like a real personal assistant**

### Other Valuable Enhancements

| Enhancement | Effort | Impact |
|---|---|---|
| System tray agent (`pystray`) | Medium | Native app feel |
| Git integration tools | Low | Developer-friendly |
| Universal search (files + docs + web) | Medium | Power feature |
| Dashboard analytics (usage graphs) | Medium | Visual appeal |
| Performance benchmarking dashboard | Medium | BTech evaluation gold |
| Safety demo mode for presentations | Very low | Presentation impact |

---

## Q10 — Can multiple LLM models be used with dynamic switching?

**Answer: Yes — this is called model routing / cascading inference and it's highly recommended.**

### The Concept: Task-Based Model Routing

```
User Command
    │
    ▼
┌──────────────┐
│ Intent Router │ ← Lightweight classifier (rules + small model)
└──────┬───────┘
       │
       ├── Simple ("open Chrome") ───→ 🟢 No model needed — direct tool call
       ├── Medium ("search web") ────→ 🟡 Qwen3-0.6B (~10ms)
       ├── Complex ("analyze PDF") ──→ 🔴 Qwen3-8B MoE (~180ms)
       └── Vision ("what's on screen")→ 🟣 Qwen2.5-VL-3B (on demand)
```

### Available Models (All Free, All Local)

| Model | VRAM | Purpose | Load Time |
|---|---|---|---|
| **Qwen3-0.6B** Q8_0 | ~0.8 GB | Simple tasks, routing, draft | ~1 second |
| **Qwen3-4B** Q4_K_M | ~2.8 GB | Medium tasks (optional tier) | ~2 seconds |
| **Qwen3-8B MoE** Q4_K_M | ~5.2 GB | Complex reasoning, coding | ~4 seconds |
| **Qwen2.5-VL-3B** Q4_K_M | ~2.5 GB | Vision tasks | ~2 seconds |
| **nomic-embed-text** GGUF | ~270 MB (CPU) | Embeddings for RAG | Already loaded |

Total download: ~10.7 GB (vs current 7.4 GB — only ~3 GB more)

### Performance Gains

| Command | Without Routing | With Routing | Speedup |
|---|---|---|---|
| "Open Chrome" | 8B model → ~400ms | Direct tool call → ~30ms | **13x faster** |
| "What time is it?" | 8B model → ~350ms | Direct tool call → ~5ms | **70x faster** |
| "Search for Python tutorials" | 8B model → ~500ms | 0.6B model → ~80ms | **6x faster** |
| "What's the weather?" | 8B model → ~450ms | 0.6B model → ~70ms | **6x faster** |
| Complex multi-step task | 8B model → ~2s | 8B model → ~2s | Same (correct model) |

**70-80% of daily commands are trivial or simple.** Routing them to smaller models (or no model) makes KRIA feel instant.

### How Models Share GPU (Time-Multiplexed)

Models are swapped in/out of VRAM as needed:
- **Cold swap:** 2-4 seconds (first load)
- **Warm swap:** <500ms (mmap caching keeps model in system RAM)
- **Idle state:** Small model (0.8 GB) stays loaded — minimal footprint
- **Heavy reasoning:** Large model loaded, Whisper unloaded temporarily

### Routing Logic

```
TRIVIAL patterns (no LLM needed):
  "open {app}", "close {app}", "what time", "lock screen",
  "battery", "volume up/down"
  → Direct tool dispatch

SIMPLE patterns (small model):
  "search for", "what's the weather", "remind me", "read file"
  → Qwen3-0.6B

Everything else:
  → Qwen3-8B MoE (full reasoning)

Fallback:
  If small model says "I need more reasoning power" → escalate to big model
```

### Verdict

**Yes, do it.** The simplest starting point: route pattern-matched trivial commands directly to tools with NO model, and use the existing Qwen3-8B for everything else. That alone gets 80% of the benefit with minimal code changes.

---

*Document maintained by Obaidullah Zeeshan — K.R.I.A. Project*
*Last updated: April 2026*
