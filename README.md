# K.R.I.A. — Kernel-Responsive Intelligent Agent

> A locally-hosted, voice-controlled, complete AI Assistant with root-level OS control, internet connectivity, document intelligence, and workflow automation.

**Developer:** Obaidullah Zeeshan — BTech CS Final Year Project

---

## What is K.R.I.A.?

K.R.I.A. is a **complete AI Assistant** that runs entirely on your laptop. It goes far beyond a simple chatbot — it listens for voice commands, reasons about complex tasks using a local LLM, browses the internet for real-time information, reads and analyzes documents, manages your applications, automates repetitive workflows, and executes actions on your operating system.

**No cloud. No subscriptions. No data leaves your machine (unless you ask it to search the web).**

## Key Features

### 🧠 Intelligent Agent
- **Agentic Reasoning** — Qwen3-8B MoE via llama.cpp with ReAct planning for multi-step tasks
- **Voice-First Interaction** — Custom wake word ("Hey KRIA"), sub-500ms response for simple commands
- **65+ Built-in Tools** — Organized across 12 capability domains
- **Four-Tier Safety System** — GREEN/YELLOW/RED/BLACK risk classification with human-in-the-loop approval

### 🌐 Internet Connectivity
- **Web Search** — DuckDuckGo-powered search with result extraction (no API key required)
- **Content Extraction** — Fetch and extract text from any web page via trafilatura
- **Real-Time Data** — Weather, news headlines, stock prices, IP info — all from free APIs
- **Download Manager** — Download files with progress tracking and size limits
- **RSS Feeds** — Read and aggregate RSS/Atom feeds

### 📄 Document Intelligence
- **Document Parsing** — Read PDF, DOCX, XLSX, CSV, images (OCR), Markdown, HTML
- **Document Summarization** — LLM-powered summarization of any document
- **Format Conversion** — Convert between formats (MD→PDF, DOCX→PDF, XLSX→CSV, etc.)
- **Document RAG** — Ingest documents into a local knowledge base for Q&A
- **Smart File Organization** — Rule-based automatic file sorting

### 💻 OS-Level Control
- **Service Management** — List, start, stop, restart system services (systemctl / sc.exe)
- **Scheduled Tasks** — Create, list, and manage cron jobs / Windows Task Scheduler entries
- **Environment Management** — Read/set environment variables, edit shell profiles
- **Disk Management** — Find large files, detect duplicates, calculate directory sizes
- **Network Diagnostics** — Ping, DNS lookup, traceroute, speed test, WiFi management
- **Power Control** — Shutdown, reboot, sleep, hibernate, lock screen

### 📦 Application Management
- **Cross-Platform Package Manager** — Unified interface across apt, dnf, winget, brew
- **Install/Uninstall/Update** — Install apps with a single voice command
- **Package Search** — Search repositories for available packages

### ⚙️ Automation Engine
- **YAML Workflows** — Define multi-step automation routines with variables and conditions
- **Event Triggers** — React to file changes, app launches, WiFi connections, battery level
- **Scheduled Tasks** — Cron-like scheduling with APScheduler
- **Macro Recorder** — Record and replay sequences of tool calls

### 🔔 Communication Hub
- **Desktop Notifications** — Native notifications on Linux (notify-send) and Windows (toast)
- **Email Composer** — Draft emails and open in your default email client
- **Clipboard Intelligence** — Read, write, history, and transform clipboard content
- **Timed Reminders** — Set reminders that trigger as desktop notifications

### 📚 Knowledge Base
- **Persistent Facts** — "Remember that my project deadline is April 30"
- **Document Q&A** — Ask questions about ingested documents via RAG
- **Code Snippets** — Save and retrieve code/text snippets
- **User Preference Learning** — Learns your patterns for proactive suggestions

### 🔌 Plugin Architecture
- **Extensible** — Community-contributed plugins for new capabilities
- **Sandboxed** — Plugins cannot bypass safety system
- **Simple API** — `plugin.yml` manifest + Python entry point

### 🛡️ Safety & Privacy
- **Fully Local** — All models run on-device (16GB RAM + NVIDIA RTX GPU)
- **Four-Tier Safety** — Dangerous actions blocked until human approval
- **Rollback System** — Automatic backups before destructive operations
- **Audit Logging** — Every action logged to tamper-proof database
- **Internet Transparency** — Every outgoing request logged and auditable
- **Docker-Portable** — One-command deployment with GPU passthrough

## Quick Start

```bash
# Clone
git clone https://github.com/obaidullah-zeeshan/kria.git
cd kria

# Setup (creates virtualenv, installs deps, generates .env)
bash scripts/setup.sh        # Linux
# .\scripts\setup.ps1        # Windows PowerShell

# Download models (first time only, ~7 GB, ~10 minutes)
python3 scripts/download_models.py

# Launch
docker compose up -d

# Dashboard
open http://localhost:3000
```

See [HOW_TO_RUN.md](docs/HOW_TO_RUN.md) for detailed instructions (Docker and non-Docker methods).

## Hardware Requirements

| Component | Minimum | Recommended |
|---|---|---|
| RAM | 16 GB | 32 GB |
| GPU | NVIDIA RTX with 6 GB VRAM | 8+ GB VRAM |
| Storage | 20 GB free | NVMe SSD |
| OS | Windows 11 / Ubuntu 22.04+ | WSL2 or native Linux |
| Internet | Optional (for web features) | Broadband for real-time data |

## Architecture

See the [System Design Document](docs/SYSTEM_DESIGN_DOCUMENT.md) for the full technical specification.

## Documentation

| Document | Description |
|---|---|
| [System Design Document](docs/SYSTEM_DESIGN_DOCUMENT.md) | Complete system architecture — 14 modules, technology choices, and rationale |
| [Implementation Guide](docs/IMPLEMENTATION_GUIDE.md) | 18-phase implementation plan with code examples for all 65+ tools |
| [Safety Specification](docs/SAFETY_SPECIFICATION.md) | Guardrail system, risk classification, HITL protocol, internet safety |
| [Project Structure](docs/PROJECT_STRUCTURE.md) | Directory layout and module responsibilities |
| [How to Run](docs/HOW_TO_RUN.md) | Setup and running instructions (Docker & manual) |
| [Speech Recognition](docs/SPEECH_RECOGNITION.md) | Speech pipeline deep-dive |
| [Queries & Decisions](docs/QUERIES.md) | Design queries, FAQ, and decision log |

## Tool Catalog (65+ Tools)

| Category | Count | Examples |
|---|---|---|
| App Control | 6 | open/close/list apps, focus window |
| File Operations | 10 | read/write/search/move/copy/delete files |
| Document Intelligence | 6 | parse PDF/DOCX/XLSX/CSV, summarize, convert |
| System Info | 7 | CPU, RAM, disk, network, battery, GPU, uptime |
| System Config | 8 | volume, brightness, WiFi, services, firewall |
| Process Management | 5 | kill, priority, process details |
| Network Management | 5 | ping, DNS, traceroute, public IP, speed test |
| Code Execution | 3 | Python, Bash, PowerShell (sandboxed) |
| Web & Internet | 8 | search, fetch, download, weather, news, RSS |
| Communication | 4 | notifications, email draft, clipboard, reminders |
| Knowledge & Memory | 5 | remember/recall facts, document RAG, snippets |
| Automation | 4 | scheduled tasks, workflows, macros |

## License

Apache 2.0
