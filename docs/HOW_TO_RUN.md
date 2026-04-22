# K.R.I.A. — How to Run

> Rust / Tauri v2 / SolidJS desktop application with an optional standalone server mode.

---

## Table of Contents

1. [TL;DR — Just Run It](#tldr)
2. [Prerequisites](#prerequisites)
3. [Step-by-Step Setup](#step-by-step-setup)
4. [Running the Desktop App](#desktop-app)
5. [Do I Need to Run llama-server Manually?](#do-i-need-to-run-llama-server-manually)
6. [Standalone Server (Headless)](#standalone-server)
7. [LLM Backend Modes](#llm-backend-modes)
8. [Configuration](#configuration)
9. [Production Build](#production-build)
10. [FAQ](#faq)

---

<a id="tldr"></a>
## TL;DR — Just Run It

```bash
# One-time setup (installs system deps, Rust, Node, Tauri CLI):
bash scripts/setup.sh

# Install frontend dependencies:
cd ui && npm install && cd ..

# Run the app:
cargo tauri dev --features nvidia
```

That's it. **You do NOT need to start `llama-server` yourself.** KRIA's built-in Hardware Orchestrator automatically spawns and manages `llama-server` in the background — detecting your GPU, picking optimal parameters, and adjusting GPU layers dynamically.

The only prerequisites:
1. `llama-server` is on your `$PATH` (verify: `which llama-server`)
2. Model files exist in `models/llm/` (the Qwen `.gguf` and `mmproj-F16.gguf` are pre-configured)

---

## Prerequisites

### System Dependencies (Ubuntu/Debian)

```bash
sudo apt update && sudo apt install -y \
  build-essential pkg-config \
  libssl-dev \
  libasound2-dev \
  libwebkit2gtk-4.1-dev \
  libgtk-3-dev \
  librsvg2-dev \
  patchelf
```

### Rust Toolchain

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

### Node.js (for the frontend)

```bash
# Using fnm (recommended)
curl -fsSL https://fnm.vercel.app/install | bash

# Or via apt
sudo apt install nodejs npm
```

### Tauri CLI

```bash
cargo install tauri-cli --version "^2" --locked
```

### llama.cpp (for local LLM inference)

```bash
# Build from source with CUDA support
git clone https://github.com/ggerganov/llama.cpp && cd llama.cpp
cmake -B build -DGGML_CUDA=ON && cmake --build build --target llama-server -j

# Install to PATH
sudo cp build/bin/llama-server /usr/local/bin/

# Verify it works
llama-server --version
```

> **Shortcut:** Run `bash scripts/setup.sh` to install all prerequisites automatically.

---

## Step-by-Step Setup

### 1. Clone and enter the repo

```bash
cd /media/obaid/SSD/KRIA   # or wherever you cloned it
```

### 2. Install frontend dependencies

```bash
cd ui && npm install && cd ..
```

### 3. Ensure model files are in place

The default config expects these files in `models/llm/`:

| File | Purpose |
|---|---|
| `Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf` | Main LLM model (chat + vision) |
| `mmproj-F16.gguf` | Vision projector for image understanding |

Check they exist:
```bash
ls models/llm/
# Should show: Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf  mmproj-F16.gguf  ...
```

### 4. Verify llama-server is on your PATH

```bash
which llama-server
# Should print: /usr/local/bin/llama-server (or similar)
```

If not found, either install it (see Prerequisites) or set the full path in `config/default.toml`:
```toml
[orchestrator]
llama_server_binary = "/home/you/Downloads/llama.cpp/build/bin/llama-server"
```

### 5. Run

```bash
cargo tauri dev --features nvidia
```

---

<a id="desktop-app"></a>
## Running the Desktop App

### Development mode

```bash
cargo tauri dev --features nvidia
```

This single command does everything:

```
  cargo tauri dev --features nvidia
          │
          ├─ 1. Starts Vite dev server on http://localhost:1420
          │     (hot-reloads SolidJS/CSS changes instantly)
          │
          ├─ 2. Compiles the Rust backend (kria-core + kria-desktop)
          │
          ├─ 3. Opens the Tauri window (renders the Vite UI)
          │
          └─ 4. init_runtime() runs inside the Rust backend:
                ├─ Detects your GPU (NVIDIA → Cuda, Apple → Metal, else → CpuOnly)
                ├─ Reads initial VRAM telemetry
                ├─ Calculates optimal GPU layers (ngl) and context window
                ├─ Spawns llama-server with those parameters  ← automatic!
                ├─ Starts GPU watchdog (monitors VRAM every 2 seconds)
                └─ Wires the dynamic llama-server URL into the model router
```

**You do NOT open a second terminal to run llama-server. The app handles it.**

### Without NVIDIA feature

If you don't have an NVIDIA GPU, or don't want NVML bindings:

```bash
cargo tauri dev
```

The orchestrator still works — it falls back from NVML → `nvidia-smi` CLI → RAM-only monitoring.

### What `--features nvidia` does

It enables the `nvml-wrapper` crate for direct NVIDIA Management Library access. This gives faster, more accurate VRAM telemetry than shelling out to `nvidia-smi`. It is optional — the system works without it, just with slightly less precise GPU monitoring.

---

<a id="do-i-need-to-run-llama-server-manually"></a>
## Do I Need to Run llama-server Manually?

**Short answer: No.** Here's a decision table:

| Scenario | Run llama-server yourself? | What to do |
|---|---|---|
| Orchestrator enabled (default) | **No** | Just run `cargo tauri dev --features nvidia` |
| Orchestrator disabled (`enabled = false`) | **Yes** | Start `llama-server` in a separate terminal, then run `cargo tauri dev` |
| Using cloud LLM (Gemini, etc.) | **No** | Set `routing_mode = "gemini"` in config, no local server needed |

### When the Orchestrator is enabled (default)

The app automatically:
1. Finds `llama-server` on your `$PATH`
2. Finds the model `.gguf` from `[[llm.models]]` config
3. Spawns `llama-server` with optimal `--n-gpu-layers`, `--ctx-size`, `--port 0`
4. Monitors VRAM and re-spawns with adjusted parameters if GPU pressure changes

You do NOT need to:
- Open a second terminal
- Manually pick `--n-gpu-layers`
- Worry about port conflicts (it uses ephemeral ports)
- Restart llama-server when VRAM changes

### When you DO need to run it manually

Only if you **disable** the orchestrator:

```toml
# In config/default.toml or ~/.kria/config.toml:
[orchestrator]
enabled = false
```

Then start the server yourself before launching the app:

```bash
# Terminal 1: start llama-server
llama-server \
  -m models/llm/Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf \
  --mmproj models/llm/mmproj-F16.gguf \
  --host 127.0.0.1 --port 8080 \
  --n-gpu-layers 18 --ctx-size 2048

# Terminal 2: start the app
cargo tauri dev --features nvidia
```

The app connects to `http://127.0.0.1:8080/v1` as configured in `[llm] local_api_url`.

### If the orchestrator fails to start

The app still launches — you'll just get an error when you try to chat. Check the terminal logs for:
```
orchestrator: no model path configured — skipping startup
```
→ Fix: ensure `[[llm.models]]` is defined in `config/default.toml` with a valid `file` field.

```
orchestrator: failed to start (non-fatal): failed to spawn llama-server: No such file
```
→ Fix: install `llama-server` and ensure it's on your `$PATH` (`which llama-server`).

---

<a id="standalone-server"></a>
## Standalone Server (Headless)

API-only mode — no GUI, useful for remote access or integrations:

```bash
cargo run -p kria-server
# Listens on 127.0.0.1:3001 (configured in [server])
```

Test it:
```bash
curl http://127.0.0.1:3001/api/health
```

---

<a id="llm-backend-modes"></a>
## LLM Backend Modes

KRIA supports three ways to connect to an LLM:

### Mode 1: Local + Orchestrator (recommended)

**Config:**
```toml
[llm]
routing_mode = "local"

[orchestrator]
enabled = true           # This is the default
```

**What happens:** KRIA auto-spawns and manages `llama-server`. GPU layers are dynamically adjusted based on VRAM pressure. No manual server needed.

### Mode 2: Local + Manual Server

**Config:**
```toml
[llm]
routing_mode = "local"
local_api_url = "http://127.0.0.1:8080/v1"

[orchestrator]
enabled = false
```

**What happens:** You run `llama-server` yourself in a separate terminal. KRIA connects to it at the configured URL. No GPU monitoring or dynamic adjustment.

### Mode 3: Cloud LLM

**Config:**
```toml
[llm]
routing_mode = "gemini"   # or "external"

[orchestrator]
enabled = false
```

**Setup:**
```bash
export KRIA_CLOUD_API_KEY="your-api-key"
cargo tauri dev
```

**What happens:** All inference goes to the cloud. No local server, no orchestrator.

---

## Configuration

### Config file locations

| File | Purpose | Priority |
|---|---|---|
| `config/default.toml` | Project defaults (checked into git) | Lowest |
| `~/.kria/config.toml` | User overrides (auto-created by settings UI) | Highest |
| Environment variables | `KRIA_CLOUD_API_KEY`, `KRIA_LLM_MODE`, `KRIA_TIER` | Override both |

To customize, either edit `config/default.toml` directly or create a user override:

```bash
mkdir -p ~/.kria
cp config/default.toml ~/.kria/config.toml
nano ~/.kria/config.toml
```

### Key config sections

| Section | What it controls |
|---|---|
| `[llm]` | Model mode (`local`/`gemini`/`external`), API URL, context |
| `[[llm.models]]` | Model files, context size, capabilities, vision projector |
| `[orchestrator]` | Enable/disable, VRAM thresholds, llama-server binary path |
| `[orchestrator.model_profile]` | Per-model VRAM budget: layers, per-layer MB, KV cache |
| `[voice]` | STT/TTS models, VAD, mic settings |
| `[memory]` | Max facts, decay settings |
| `[safety]` | HITL approval, audit, rollback |
| `[server]` | Host & port for standalone server (default 3001) |
| `[telegram]` | Bot token, allowed chats, auto-start |
| `[ui]` | Theme, font size |
| `[search]` | Search engine, news feeds |
| `[hardware]` | Tier override, max context tokens, GPU layers, threads |
| `[agent]` | Autonomy profile, confidence thresholds, max tool rounds |

### Orchestrator config explained

```toml
[orchestrator]
enabled = true                    # Set false to manage llama-server yourself
poll_interval_secs = 2            # How often to check VRAM (seconds)
yield_threshold_mb = 512          # Free VRAM below this → start offloading layers to CPU
emergency_threshold_mb = 128      # Free VRAM below this → immediate kill + CPU fallback
recover_threshold_mb = 2048       # Free VRAM above this → try adding layers back to GPU
cooldown_secs = 60                # Min seconds between swaps (prevents thrashing)
max_transitions_per_hour = 6      # Hard cap on swaps per hour
min_ngl_delta = 3                 # Minimum layer change to trigger a swap
safety_margin_mb = 256            # VRAM buffer reserved to prevent OOM
llama_server_binary = "llama-server"  # Binary name or full path
flash_attention = true            # Pass --flash-attn to llama-server
mlock = true                      # Lock model weights in RAM
batch_size = 256                  # Batch size for llama-server

[orchestrator.model_profile]
total_layers = 35                 # Qwen2.5-VL-7B has 35 transformer layers
per_layer_vram_mb = 128           # ~128 MB per GPU layer
base_vram_overhead_mb = 200       # CUDA context + embeddings overhead
kv_per_1k_ctx_mb = 100            # KV cache per 1024 context tokens
min_context = 2048                # Never go below this context window
max_context = 8192                # Maximum context window
has_vision_projector = true       # Model supports images (via mmproj)
```

### Model entries

Models are defined in `[[llm.models]]` (array of tables). The orchestrator uses the **first** entry:

```toml
[[llm.models]]
name = "qwen2.5-vl-7b"
file = "Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf"    # Must match filename in models/llm/
display_name = "Qwen 2.5 VL 7B (Q4_K_M)"
context_window = 8192
max_tokens = 4096
vram_estimate_gb = 5.0
capabilities = ["chat", "vision"]
mmproj_file = "mmproj-F16.gguf"                  # Vision projector (optional)
```

---

## Production Build

### Using the build script

```bash
bash scripts/build-release.sh

# With NVIDIA GPU telemetry:
bash scripts/build-release.sh --features nvidia
```

### Manually

```bash
cd ui && npm run build && cd ..
cd crates/kria-desktop && cargo tauri build --features nvidia
```

Output: a self-contained native app in `target/release/bundle/`:
- **Linux:** `.deb` and `.AppImage`
- **macOS:** `.dmg`
- **Windows:** `.msi` / `.exe`

The production binary **bundles the entire frontend** — no Vite, no Node.js, no separate web server. Just the executable + `llama-server` on PATH.

---

## FAQ

### 1. Do I need two terminals (one for llama-server, one for the app)?

**No.** With the default config (`orchestrator.enabled = true`), a single command does everything:

```bash
cargo tauri dev --features nvidia
```

This starts Vite (frontend), compiles and runs the Rust backend, and the backend automatically spawns `llama-server` with optimal GPU settings. One terminal, one command.

### 2. What if I don't have an NVIDIA GPU?

Run without the feature flag:
```bash
cargo tauri dev
```

The orchestrator detects `CpuOnly` and spawns `llama-server` with `--n-gpu-layers 0` (pure CPU inference). Slower, but works.

On macOS with Apple Silicon, it detects `Metal` and uses all GPU layers (unified memory).

### 3. Does the app hot-reload when I change code?

| What changed | Reloads? | How |
|---|---|---|
| Frontend (SolidJS/CSS) | Yes, instantly | Vite HMR |
| Rust backend | Yes, recompiles | Tauri CLI watches and restarts |
| Config files | No | Restart the app |
| Feature flags | No | Stop and re-run with the flag |

### 4. Where are the logs?

**Terminal:** The terminal running `cargo tauri dev` shows live logs.

**Log files:** `~/.kria/logs/kria.log.YYYY-MM-DD` (JSON, daily rotation).

```bash
# View latest logs
cat ~/.kria/logs/kria.log.$(date +%F) | jq .

# Filter errors
cat ~/.kria/logs/kria.log.$(date +%F) | jq 'select(.level == "ERROR")'

# Verbose orchestrator logging
RUST_LOG="kria_core::llm::orchestrator=debug" cargo tauri dev --features nvidia
```

**Browser DevTools:** Right-click in the Tauri window → Inspect Element → Console.

### 5. How do I run the tests?

```bash
# All workspace tests
cargo test --workspace

# Specific crate
cargo test -p kria-core

# Orchestrator tests only
cargo test -p kria-core --lib -- llm::orchestrator

# Frontend tests
npm --prefix ui run test:run
```

### 6. How is the project structured?

```
KRIA/
├── crates/
│   ├── kria-core/          # Shared library (LLM, memory, safety, tools, orchestrator)
│   │   └── src/llm/orchestrator/
│   │       ├── mod.rs             # GPU detection, orchestrator startup
│   │       ├── server_manager.rs  # llama-server process lifecycle
│   │       ├── telemetry.rs       # VRAM/RAM telemetry (NVML, CLI, fallback)
│   │       ├── strategy.rs        # Layer offload calculator
│   │       └── gpu_watchdog.rs    # Real-time VRAM monitoring loop
│   ├── kria-desktop/       # Tauri v2 desktop app
│   └── kria-server/        # Headless HTTP/WS server
├── ui/                     # SolidJS + Vite frontend
├── config/                 # Default configuration
│   └── default.toml        # ← edit this for model/orchestrator settings
├── models/                 # Model files (gitignored)
│   └── llm/                # .gguf model files go here
├── scripts/                # Setup, build, utility scripts
└── docs/                   # Documentation
```

### 7. How do I check for compiler errors without running?

```bash
cargo check --workspace          # Fast type-check
cargo clippy --workspace         # Lints + warnings
```

### 8. How do I reset app state?

```bash
rm -rf ~/.kria/             # Remove all user data (config, logs, database)
                            # App recreates defaults on next launch

rm ~/.kria/config.toml      # Reset only config
rm -rf ~/.kria/logs/        # Clear only logs
```

### 9. How do I switch between local and cloud LLM?

```toml
# Local with orchestrator (default — recommended)
[llm]
routing_mode = "local"
[orchestrator]
enabled = true

# Local without orchestrator (manual llama-server)
[llm]
routing_mode = "local"
local_api_url = "http://127.0.0.1:8080/v1"
[orchestrator]
enabled = false

# Cloud (no local server needed)
[llm]
routing_mode = "gemini"
# Set key: export KRIA_CLOUD_API_KEY="..."
[orchestrator]
enabled = false
```

Restart the app after changing config.

### 10. Port conflicts — what ports does KRIA use?

| Port | Used by | When |
|---|---|---|
| `1420` | Vite dev server | Development only |
| `3001` | Standalone HTTP server | When running `kria-server` |
| _ephemeral_ | llama-server (orchestrator) | Auto-assigned, no conflicts |
| `8080` | llama-server (manual mode) | Only when orchestrator disabled |

In production, only the ephemeral llama-server port is used — Vite and standalone server ports are not needed.

### 11. How do I debug the orchestrator?

```bash
RUST_LOG="kria_core::llm::orchestrator=debug" cargo tauri dev --features nvidia
```

Expected startup logs:
```
INFO orchestrator: detected GPU backend backend=Cuda
INFO orchestrator: initial parameters ngl=35 ctx=4096 degradation=Full
INFO server_manager: spawning llama-server ngl=35 ctx=4096
INFO server_manager: discovered ephemeral port port=43567
INFO server_manager: llama-server is ready
INFO orchestrator: started and attached to model router
```

Check status at runtime (browser DevTools console):
```js
await window.__TAURI__.core.invoke("get_orchestrator_status")
```

### 12. Troubleshooting

| Problem | Cause | Fix |
|---|---|---|
| Blank window | Vite dev server didn't start (port 1420 in use) | Kill stale process: `fuser -k 1420/tcp`, then re-run |
| `orchestrator: no model path configured` | No `[[llm.models]]` in config | Check `config/default.toml` has valid model entries |
| `failed to spawn llama-server: No such file` | `llama-server` not on PATH | Run `which llama-server` — install or set full path in config |
| Model doesn't load | `.gguf` file not in `models/llm/` | Place model files there |
| No VRAM telemetry | NVML unavailable | Build with `--features nvidia` and install NVIDIA drivers |
| Excessive swapping | VRAM thresholds too sensitive | Increase `cooldown_secs`, decrease `max_transitions_per_hour` |
| `models = []` in `~/.kria/config.toml` | Stale user config | Delete `~/.kria/config.toml` so defaults apply, or add models manually |

### 13. Full reset

```bash
# Kill running instance
pkill kria-desktop 2>/dev/null

# Clear Rust build cache (forces full recompile — takes ~2-3 min)
cargo clean

# Clear UI cache
rm -rf ui/dist ui/node_modules/.vite

# Clear stale user config (so fresh defaults load)
rm -f ~/.kria/config.toml

# Rebuild
cargo tauri dev --features nvidia
```
# K.R.I.A. — How to Run

> Rust / Tauri v2 / SolidJS desktop application with an optional standalone server mode.

---

## Table of Contents

- [Prerequisites](#prerequisites)
- [Quick Start (One Command)](#quick-start)
- [Running — Desktop App (Tauri)](#desktop-app)
- [Running — Standalone Server](#standalone-server)
- [LLM Backend Setup](#llm-backend)
- [Hardware Orchestrator](#hardware-orchestrator)
- [Configuration](#configuration)
- [Production Build](#production-build)
- [FAQ](#faq)

---

## Prerequisites

### System dependencies (Linux)

```bash
sudo apt update && sudo apt install -y \
  build-essential pkg-config \
  libssl-dev \
  libasound2-dev \
  libwebkit2gtk-4.1-dev \
  libgtk-3-dev \
  librsvg2-dev \
  patchelf
```

### Rust toolchain

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

### Node.js (for the UI)

```bash
# Using nvm (recommended)
curl -fsSL https://fnm.vercel.app/install | bash

# Or via your package manager
sudo apt install nodejs npm
```

### Tauri CLI

```bash
cargo install tauri-cli --version "^2" --locked
```

> **Tip:** Run `bash scripts/setup.sh` to install _all_ prerequisites automatically (Linux/macOS). On Windows use `powershell -ExecutionPolicy Bypass -File scripts/setup.ps1`.

---

## Quick Start

From the project root:

```bash
# Install everything + build + run
bash scripts/setup.sh

# Or manually:
cd ui && npm install && cd ..
cd crates/kria-desktop && cargo tauri dev
```

That single `cargo tauri dev` command starts **both** the Vite frontend dev server (port 1420) **and** the Rust backend in one process. No separate terminals needed.

---

## Desktop App

### Development mode

```bash
cd crates/kria-desktop
cargo tauri dev
```

#### With NVIDIA GPU telemetry (NVML)

If you have an NVIDIA GPU and want real-time VRAM monitoring via NVML:

```bash
cd crates/kria-desktop
cargo tauri dev --features nvidia
```

This enables the `nvml-wrapper` crate for precise VRAM telemetry. Without it, the orchestrator falls back to `nvidia-smi` CLI polling or RAM-only monitoring.

This will:
1. Start the **Vite dev server** on `http://localhost:1420` (via `beforeDevCommand`)
2. Compile the **Rust backend** (`kria-desktop` crate)
3. Open the **Tauri window** pointing at the Vite dev server

### What happens under the hood

```
┌──────────────────────────────────────────────────────────┐
│  cargo tauri dev [--features nvidia]                     │
│                                                          │
│  ┌───────────────┐        ┌───────────────────────────┐  │
│  │  Vite (1420)  │◄──────►│  Tauri WebView            │  │
│  │  SolidJS UI   │  HMR   │  renders the frontend      │  │
│  └───────────────┘        └──────────┬────────────────┘  │
│                                      │ IPC                │
│                           ┌──────────▼────────────────┐  │
│                           │  Rust Backend              │  │
│                           │  (commands.rs)             │  │
│                           │  ├─ Orchestrator           │  │
│                           │  │  ├─ GPU Watchdog        │  │
│                           │  │  ├─ VRAM Telemetry      │  │
│                           │  │  └─ LlamaServerManager  │  │
│                           │  ├─ ModelRouter → LLM      │  │
│                           │  ├─ MemoryStore            │  │
│                           │  └─ SafetyGateway          │  │
│                           └───────────────────────────┘  │
└──────────────────────────────────────────────────────────┘
```

---

## Standalone Server

Headless / API-only mode — no GUI, useful for remote access or integrations:

```bash
cargo run -p kria-server
# Listens on 127.0.0.1:3001 (configured in config/default.toml → [server])
```

Test it:

```bash
curl http://127.0.0.1:3001/api/health
```

---

## LLM Backend

KRIA supports two modes for local LLM inference:

### Option A: Hardware Orchestrator (recommended)

When `[orchestrator] enabled = true` (the default), KRIA **automatically spawns and manages** a `llama-server` process. No manual server startup needed.

The orchestrator:
- Detects your GPU (NVIDIA CUDA, Apple Metal, or CPU-only)
- Calculates optimal GPU layer offloading based on available VRAM
- Spawns `llama-server` with the right parameters
- Monitors VRAM in real time and dynamically swaps GPU layers under pressure
- Cancels in-flight LLM streams during swaps to avoid corruption

**Requirements:**
1. `llama-server` must be on your `$PATH` (or set `orchestrator.llama_server_binary` in config)
2. At least one model `.gguf` file must be configured in `[[llm.local_models]]`

```bash
# Install llama.cpp (build from source or download a release)
git clone https://github.com/ggerganov/llama.cpp && cd llama.cpp
cmake -B build -DGGML_CUDA=ON && cmake --build build --target llama-server -j
sudo cp build/bin/llama-server /usr/local/bin/
```

Then just run KRIA — the orchestrator handles the rest:

```bash
cd crates/kria-desktop
cargo tauri dev --features nvidia   # with NVML telemetry
# or
cargo tauri dev                     # without NVML (falls back to nvidia-smi / RAM)
```

### Option B: Manual llama-server (orchestrator disabled)

Set `enabled = false` under `[orchestrator]` in your config, then start the server yourself:

```bash
llama-server \
  -m models/llm/microsoft_Phi-4-mini-instruct-Q4_K_M.gguf \
  --host 127.0.0.1 \
  --port 8080 \
  --ctx-size 4096
```

The app connects to `http://127.0.0.1:8080/v1` as configured in `[llm] local_host` / `local_port`.

Without either option, the app still starts — chat messages will return a helpful error telling you to start the server or set a cloud API key.

### Using a cloud LLM instead

Edit `~/.kria/config.toml`:

```toml
[llm]
mode = "gemini"          # or "external"
cloud_api_key = "YOUR_KEY_HERE"
```

Or set the environment variable:

```bash
export KRIA_CLOUD_API_KEY="YOUR_KEY_HERE"
```

With a cloud LLM, the orchestrator is not used (no local server to manage).

---

## Hardware Orchestrator

The orchestrator is a background daemon inside KRIA that manages `llama-server` lifecycle and dynamically adjusts GPU layer offloading based on real-time VRAM/RAM telemetry.

### How it works

```
┌─ Orchestrator ─────────────────────────────────┐
│                                                 │
│  GPU Watchdog (polls every 2s)                  │
│    │                                            │
│    ├─ VRAM > recover_threshold → add layers     │
│    ├─ VRAM < yield_threshold  → remove layers   │
│    └─ VRAM < emergency_threshold → immediate    │
│                                                 │
│  Telemetry: NVML → nvidia-smi CLI → RAM         │
│  (cascading fallback)                           │
│                                                 │
│  LlamaServerManager                             │
│    ├─ spawn(ngl, ctx, vision)                   │
│    ├─ graceful_stop() + re-spawn on swap        │
│    └─ cancel_streams() during transitions       │
│                                                 │
│  Strategy Calculator                            │
│    ├─ DegradationLevel: Full → ReducedContext   │
│    │   → PartialOffload → HeavyOffload → CPU    │
│    └─ Calculates optimal ngl + ctx from VRAM    │
└─────────────────────────────────────────────────┘
```

### GPU backends

| Platform | Backend | Telemetry | Dynamic offloading |
| -------- | ------- | --------- | ------------------ |
| Linux/Windows + NVIDIA | `Cuda` | NVML or nvidia-smi | Full VRAM-based |
| macOS (Apple Silicon) | `Metal` | RAM-based | Static (all layers on GPU) |
| No discrete GPU | `CpuOnly` | RAM-based | N/A (all CPU) |

### NVIDIA feature flag

The `nvidia` feature enables the `nvml-wrapper` crate for direct NVML API access (faster, more accurate than CLI):

```bash
# Build with NVML support
cargo tauri dev --features nvidia
cargo tauri build --features nvidia

# Without it, falls back to nvidia-smi CLI → RAM monitoring
cargo tauri dev
```

### Orchestrator events (frontend)

The orchestrator emits Tauri events that the UI listens to:

| Event | Payload | UI effect |
| ----- | ------- | --------- |
| `orchestrator:swap_started` | `{from_ngl, to_ngl, emergency}` | Shows swap overlay, disables input |
| `orchestrator:swap_completed` | `{new_ngl, new_context, duration_ms}` | Hides overlay, re-enables input |
| `orchestrator:degradation_changed` | `{level}` | Shows degradation pill (e.g. "ReducedContext") |
| `orchestrator:vram_pressure` | `{free_vram_mb}` | Logged to console |
| `orchestrator:stream_interrupted` | `{}` | In-flight stream cancelled |

### Checking orchestrator status

From the frontend:
```ts
const status = await invoke("get_orchestrator_status");
// Returns: { enabled, backend, current_ngl, current_context, degradation, server_healthy, api_url }
```

Or check the health dashboard — the orchestrator registers `"orchestrator"` and `"llama-server"` in the health registry.

---

## Configuration

```bash
# Create user config directory
mkdir -p ~/.kria

# Copy the default config
cp config/default.toml ~/.kria/config.toml

# Edit to your needs
nano ~/.kria/config.toml
```

Key sections in `config/default.toml`:

| Section                       | What it controls                                                        |
| ----------------------------- | ----------------------------------------------------------------------- |
| `[llm]`                       | Model mode (`local`/`gemini`/`external`), port, context                 |
| `[[llm.local_models]]`        | Model file names, context size, GPU layers, capabilities                |
| `[voice]`                     | STT/TTS models, sample rate, VAD threshold                              |
| `[memory]`                    | Max facts, decay settings                                               |
| `[safety]`                    | HITL approval requirements, audit, rollback                              |
| `[server]`                    | Host & port for the standalone server (default 3001)                     |
| `[telegram]`                  | Telegram bot token, allowed chats, auto-start                           |
| `[ui]`                        | Theme, font size                                                        |
| `[search]`                    | Search engine, news feed URLs                                           |
| `[hardware]`                  | Tier override, max context tokens, GPU layers, threads                  |
| `[agent]`                     | Autonomy profile, confidence thresholds, max tool rounds                |
| `[orchestrator]`              | Enable/disable, VRAM thresholds, llama-server binary, flash attention   |
| `[orchestrator.model_profile]`| Per-model VRAM budget: layers, per-layer MB, KV cache, context limits   |

---

## Production Build

### Using the script (recommended)

```bash
bash scripts/build-release.sh

# With NVIDIA GPU telemetry support:
bash scripts/build-release.sh --features nvidia
```

This builds the frontend, compiles the Rust backend in release mode, and produces platform-native bundles:

- **Linux:** `.deb` and `.AppImage` in `target/release/bundle/`
- **macOS:** `.dmg` in `target/release/bundle/`
- **Windows:** `powershell -File scripts/build-release.ps1` → `.msi` / `.exe`

### Manually

```bash
cd ui && npm run build && cd ..
cd crates/kria-desktop && cargo tauri build
```

The output binary is a **single self-contained executable** — the frontend is bundled inside. No separate web server needed in production.

---

## FAQ

### 1. In production, do I need to run the frontend and backend separately?

**No.** The production build bundles everything into a **single binary**. When you run `cargo tauri build`, it:
1. Compiles the SolidJS frontend into static files (`ui/dist/`)
2. Embeds those files directly into the Rust binary
3. Produces a native app (`.AppImage`, `.deb`, `.dmg`, `.msi`)

The resulting executable contains both the UI and the Rust backend — no Node.js, no Vite, no separate processes. Just double-click and run.

With the **Hardware Orchestrator enabled** (default), even the `llama-server` process is managed automatically — KRIA spawns it, monitors VRAM, and adjusts GPU layers on the fly. The only prerequisite is that `llama-server` is on your `$PATH`.

If the orchestrator is **disabled**, you need to start `llama-server` manually:

```bash
# Example: start both in one script (orchestrator disabled)
./llama-server -m models/llm/microsoft_Phi-4-mini-instruct-Q4_K_M.gguf \
  --host 127.0.0.1 --port 8080 --ctx-size 4096 &

./KRIA   # the built binary
```

If you use a cloud LLM (Gemini, etc.), the single binary is truly all you need — no local server, no orchestrator.

---

### 2. Does it pick up code changes automatically during development?

**Partially — it depends on what you changed:**

| What changed         | Hot-reloaded? | What to do                            |
| -------------------- | ------------- | ------------------------------------- |
| **Frontend** (SolidJS/CSS in `ui/src/`) | ✅ Yes | Vite HMR updates instantly in the window |
| **Rust backend** (`crates/*/src/`)      | ✅ Yes (recompile) | Tauri CLI detects the change, recompiles, and restarts the app automatically |
| **Tauri config** (`tauri.conf.json`)    | ❌ No  | Stop `cargo tauri dev` (Ctrl+C) and re-run it |
| **Config files** (`config/default.toml`, `~/.kria/config.toml`) | ❌ No | Restart the app — config is read once at startup |
| **Cargo.toml** (dependency changes)     | ❌ No  | Stop, run `cargo update` if needed, then `cargo tauri dev` again |
| **Feature flags** (e.g. `--features nvidia`) | ❌ No | Stop and re-run with the flag |

**In short:** For day-to-day frontend and Rust code changes, just save the file — the running `cargo tauri dev` process handles it. For config/dependency changes, restart.

---

### 3. Where can I see logs?

Logs are written to **two places**:

#### Terminal (stdout)
The terminal where you ran `cargo tauri dev` shows live compact logs:

```
INFO kria_desktop::commands: KRIA runtime initialized
INFO kria_desktop::commands: user prompt received chars=42
INFO kria_desktop::commands: KRIA runtime initialized — agent loop active
WARN kria_core::sidecar::bridge: sidecar pre-check: kria_modules not importable: ...
```

#### Log files (JSON, daily rotation)

```
~/.kria/logs/kria.log.YYYY-MM-DD
```

These are JSON-formatted and rotated daily. Use `jq` to browse:

```bash
# Latest logs
cat ~/.kria/logs/kria.log.$(date +%F) | jq .

# Filter by level
cat ~/.kria/logs/kria.log.$(date +%F) | jq 'select(.level == "ERROR")'

# Follow live
tail -f ~/.kria/logs/kria.log.$(date +%F) | jq .
```

#### Controlling log verbosity

KRIA uses an essential-noise default profile when `RUST_LOG` is not set, and now includes essential pipeline traces in terminal (prompt intake, LLM request/response summary, tool calls/results, final output path). To inspect the full prompt→result pipeline with extra detail, enable the dedicated debug trace mode:

```bash
# Full pipeline trace (debug-only, sanitized + truncated payload previews)
KRIA_PIPELINE_DEBUG=1 cargo tauri dev
```

Set `RUST_LOG` for manual overrides:

```bash
# Verbose — show everything
RUST_LOG=debug cargo tauri dev

# Quiet — errors only
RUST_LOG=error cargo tauri dev

# Fine-grained
RUST_LOG="kria_core=debug,kria_desktop=info" cargo tauri dev

# Pipeline trace target + selected modules
KRIA_PIPELINE_DEBUG=1 RUST_LOG="kria_pipeline=debug,kria_desktop=info,kria_core=info" cargo tauri dev
```

Default levels (when `RUST_LOG` is not set): `info` globally with noisy subprocess targets clamped (`llama-server`, `mcp_stderr`, `sidecar_stderr` at `warn`) and `kria_pipeline=info` (`KRIA_PIPELINE_DEBUG=1` upgrades pipeline traces to debug detail).

#### Browser DevTools (frontend)

Right-click in the Tauri window → **Inspect Element** → **Console** tab for frontend `console.log` output (SolidJS, Tauri IPC events, etc.).

---

### 4. How do I run the tests?

```bash
# All tests across the workspace
cargo test --workspace

# Tests for a specific crate
cargo test -p kria-core
cargo test -p kria-desktop
cargo test -p kria-server

# A specific test by name
cargo test test_memory_store
```

---

### 5. How do I add a new Tauri command (IPC endpoint)?

1. **Define the command** in `crates/kria-desktop/src/commands.rs`:
   ```rust
   #[tauri::command]
   pub async fn my_command(
       input: String,
       state: State<'_, AppState>,
   ) -> Result<serde_json::Value, String> {
       // your logic
       Ok(serde_json::json!({"result": "ok"}))
   }
   ```

2. **Register it** in `crates/kria-desktop/src/main.rs` inside `invoke_handler`:
   ```rust
   .invoke_handler(tauri::generate_handler![
       commands::send_message,
       commands::my_command,  // add here
       // ...
   ])
   ```

3. **Call from the frontend** in `ui/src/`:
   ```ts
   import { invoke } from "@tauri-apps/api/core";
   const result = await invoke("my_command", { input: "hello" });
   ```

---

### 6. How is the project structured?

```
KRIA/
├── crates/
│   ├── kria-core/       # Shared library — LLM, memory, safety, tools, config
│   │   └── src/llm/orchestrator/   # Hardware Orchestrator modules
│   │       ├── mod.rs              #   Top-level orchestrator (GPU detection, startup)
│   │       ├── server_manager.rs   #   llama-server process lifecycle
│   │       ├── telemetry.rs        #   VRAM/RAM telemetry (NVML, CLI, fallback)
│   │       ├── strategy.rs         #   Layer offload calculator + degradation levels
│   │       └── gpu_watchdog.rs     #   Real-time VRAM monitoring state machine
│   ├── kria-desktop/    # Tauri v2 desktop app (depends on kria-core)
│   └── kria-server/     # Axum HTTP/WS server (depends on kria-core)
├── ui/                  # SolidJS + Vite frontend
├── config/              # Default configuration files
├── models/              # LLM/STT/TTS model files (gitignored)
├── scripts/             # Setup, build, and utility scripts
└── docs/                # Documentation
```

The three Rust crates form a dependency tree: both `kria-desktop` and `kria-server` depend on `kria-core`. Shared logic (LLM routing, memory, safety, orchestrator) lives in `kria-core`.

---

### 7. How do I check for compiler errors without running the app?

```bash
# Type-check the whole workspace (fast, no codegen)
cargo check --workspace

# With all warnings shown
cargo check --workspace 2>&1 | head -50

# Full build (compiles but doesn't run)
cargo build --workspace
```

---

### 8. How do I reset the app state / data?

```bash
# Remove all user data (config, logs, database)
rm -rf ~/.kria/

# The app will recreate defaults on next launch

# To only clear logs:
rm -rf ~/.kria/logs/

# To only reset config:
rm ~/.kria/config.toml
```

Or use the uninstall script for a thorough cleanup:

```bash
bash scripts/uninstall.sh --config   # remove config + data only
bash scripts/uninstall.sh --all      # remove everything including toolchains
```

---

### 9. How do I switch between local and cloud LLM?

Edit `~/.kria/config.toml`:

```toml
# Local with orchestrator (recommended — auto-manages llama-server)
[llm]
mode = "local"

[orchestrator]
enabled = true

# Local without orchestrator (manual llama-server)
[llm]
mode = "local"
local_port = 8080

[orchestrator]
enabled = false

# Cloud (no local server needed, orchestrator unused)
[llm]
mode = "gemini"
# Set key via env: export KRIA_CLOUD_API_KEY="..."

[orchestrator]
enabled = false
```

Then restart the app. The model router in `kria-core` will automatically route chat requests to the configured backend.

---

### 10. How do I debug Rust backend code?

1. **Add `tracing` calls** to any function:
   ```rust
   tracing::debug!("processing request: {:?}", input);
   ```

2. **Run with verbose logging:**
   ```bash
   RUST_LOG=debug cargo tauri dev
   ```

3. **Use a debugger** (VS Code + CodeLLDB extension):
   - Set a breakpoint in `commands.rs`
   - Run the "Debug Tauri" launch config (or `cargo build -p kria-desktop && ./target/debug/kria-desktop`)

4. **Inspect Tauri IPC events** in the browser DevTools console — all `app.emit()` events appear there.

---

### 11. Port conflicts — what ports does KRIA use?

| Port   | Used by                     | Configurable in              |
| ------ | --------------------------- | ---------------------------- |
| `1420` | Vite dev server (dev only)  | `ui/vite.config.ts`          |
| `3001` | Standalone HTTP server      | `config/default.toml` → `[server]` |
| `8080` | llama.cpp LLM server (manual mode) | `config/default.toml` → `[llm]`    |
| _ephemeral_ | llama-server (orchestrator mode) | Auto-assigned, discovered at startup |

In production builds, ports 1420 and 3001 are **not used** — the Tauri app is self-contained.

When the orchestrator is enabled, it spawns `llama-server` on an ephemeral port and wires the dynamic URL into the model router automatically. The `[llm] local_port = 8080` setting is only used as a fallback when the orchestrator is disabled.

---

### 12. How do I debug the Hardware Orchestrator?

**Check if it started:**
```bash
# Look for orchestrator log lines in the terminal
RUST_LOG="kria_core::llm::orchestrator=debug" cargo tauri dev --features nvidia
```

You should see:
```
INFO orchestrator: detected GPU backend backend=Cuda
INFO orchestrator: initial parameters ngl=35 ctx=4096 degradation=Full
INFO orchestrator: started and attached to model router
```

**Check orchestrator status at runtime** (browser DevTools console):
```js
await window.__TAURI__.core.invoke("get_orchestrator_status")
// → { enabled: true, backend: "Cuda", current_ngl: 35, current_context: 4096, ... }
```

**Common issues:**

| Symptom | Cause | Fix |
| ------- | ----- | --- |
| `orchestrator: no model path configured` | No `[[llm.local_models]]` in config | Add a model entry to `config/default.toml` or `~/.kria/config.toml` |
| `orchestrator: failed to start (non-fatal)` | `llama-server` not on `$PATH` | Install llama.cpp and ensure `llama-server` is accessible |
| Orchestrator starts but model doesn't load | `.gguf` file not at expected path | Place model files in `~/.kria/models/llm/` |
| No VRAM telemetry (falls back to RAM) | NVML not available | Build with `--features nvidia` and ensure NVIDIA drivers are installed |

---

### 13. Troubleshooting — full reset

```bash
# 1. Kill any running instance
pkill kria-desktop 2>/dev/null; sleep 1

# 2. Clear Rust build cache (forces full recompile)
cd /media/obaid/SSD/KRIA
cargo clean

# 3. Clear UI build cache
rm -rf ui/dist ui/node_modules/.vite

# 4. Rebuild everything
cd crates/kria-desktop && cargo tauri dev
```

Or if you just want a **quick restart** without cleaning (usually enough):
```bash
# Stop current dev server (Ctrl+C in the terminal running cargo tauri dev), then:
cd /media/obaid/SSD/KRIA/crates/kria-desktop && cargo tauri dev
```

`cargo clean` is only needed when:
- Icons/assets changed and the binary isn't picking them up
- Build is in a broken state
- Rust code changes aren't being detected

It takes ~2-3 min to recompile everything from scratch after `cargo clean`.

It takes ~2–3 min to recompile everything from scratch after `cargo clean`.