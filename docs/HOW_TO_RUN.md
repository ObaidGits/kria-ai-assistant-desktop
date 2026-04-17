# K.R.I.A. — How to Run

> Rust / Tauri v2 / SolidJS desktop application with an optional standalone server mode.

---

## Table of Contents

- [Prerequisites](#prerequisites)
- [Quick Start (One Command)](#quick-start)
- [Running — Desktop App (Tauri)](#desktop-app)
- [Running — Standalone Server](#standalone-server)
- [LLM Backend Setup](#llm-backend)
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

This will:
1. Start the **Vite dev server** on `http://localhost:1420` (via `beforeDevCommand`)
2. Compile the **Rust backend** (`kria-desktop` crate)
3. Open the **Tauri window** pointing at the Vite dev server

### What happens under the hood

```
┌──────────────────────────────────────────────────────┐
│  cargo tauri dev                                     │
│                                                      │
│  ┌───────────────┐        ┌───────────────────────┐  │
│  │  Vite (1420)  │◄──────►│  Tauri WebView        │  │
│  │  SolidJS UI   │  HMR   │  renders the frontend  │  │
│  └───────────────┘        └──────────┬────────────┘  │
│                                      │ IPC            │
│                           ┌──────────▼────────────┐  │
│                           │  Rust Backend          │  │
│                           │  (commands.rs)         │  │
│                           │  ├─ ModelRouter → LLM  │  │
│                           │  ├─ MemoryStore        │  │
│                           │  └─ SafetyGateway      │  │
│                           └───────────────────────┘  │
└──────────────────────────────────────────────────────┘
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

The app expects a local **llama.cpp** server on port `8080` (configurable in `config/default.toml` → `[llm]`).

```bash
# Download & build llama.cpp, then run:
./llama-server \
  -m models/llm/microsoft_Phi-4-mini-instruct-Q4_K_M.gguf \
  --host 127.0.0.1 \
  --port 8080 \
  --ctx-size 4096

obaid@obaid-ubuntu:~/Downloads/llama.cpp$ ./build/bin/llama-server   -m models/Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf   --mmproj models/mmproj-F16.gguf   -ngl 0

obaid@obaid-ubuntu:~/Downloads/llama.cpp$ ./build/bin/llama-server -m models/microsoft_Phi-4-mini-instruct-Q4_K_M.gguf -c 8192 -ngl 0

```

Without it, the app still starts — chat messages will return a helpful error telling you to start the server or set a cloud API key.

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

| Section      | What it controls                                        |
| ------------ | ------------------------------------------------------- |
| `[llm]`      | Model mode (`local`/`gemini`/`external`), port, context |
| `[voice]`    | STT/TTS models, sample rate, VAD threshold              |
| `[memory]`   | Max facts, decay settings                               |
| `[safety]`   | HITL approval requirements, audit, rollback              |
| `[server]`   | Host & port for the standalone server (default 3001)     |
| `[ui]`       | Theme, font size                                        |

---

## Production Build

### Using the script (recommended)

```bash
bash scripts/build-release.sh
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

The **only** external process you may need is the **llama.cpp server** (if you're using a local LLM). You can script that alongside:

```bash
# Example: start both in one script
./llama-server -m models/llm/microsoft_Phi-4-mini-instruct-Q4_K_M.gguf \
  --host 127.0.0.1 --port 8080 --ctx-size 4096 &

./KRIA   # the built binary

# Or use cloud LLM mode and you need nothing else
```

If you use a cloud LLM (Gemini, etc.), the single binary is truly all you need.

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

**In short:** For day-to-day frontend and Rust code changes, just save the file — the running `cargo tauri dev` process handles it. For config/dependency changes, restart.

---

### 3. Where can I see logs?

Logs are written to **two places**:

#### Terminal (stdout)
The terminal where you ran `cargo tauri dev` shows live compact logs:

```
INFO kria_desktop::commands: KRIA runtime initialized
INFO kria_desktop::commands: User message: hello
INFO kria_desktop::commands: Routing to backend: phi-4-mini
INFO kria_desktop::commands: LLM response received (142 chars)
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

Set the `RUST_LOG` environment variable:

```bash
# Verbose — show everything
RUST_LOG=debug cargo tauri dev

# Quiet — errors only
RUST_LOG=error cargo tauri dev

# Fine-grained
RUST_LOG="kria_core=debug,kria_desktop=info" cargo tauri dev
```

Default levels (when `RUST_LOG` is not set): `info` globally, `debug` for `kria_core`, `kria_desktop`, and `kria_server`.

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
│   ├── kria-desktop/    # Tauri v2 desktop app (depends on kria-core)
│   └── kria-server/     # Axum HTTP/WS server (depends on kria-core)
├── ui/                  # SolidJS + Vite frontend
├── config/              # Default configuration files
├── models/              # LLM/STT/TTS model files (gitignored)
├── scripts/             # Setup, build, and utility scripts
└── docs/                # Documentation
```

The three Rust crates form a dependency tree: both `kria-desktop` and `kria-server` depend on `kria-core`. Shared logic (LLM routing, memory, safety) lives in `kria-core`.

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
# Local (llama.cpp required)
[llm]
mode = "local"
local_port = 8080

# Cloud (no local server needed)
[llm]
mode = "gemini"
# Set key via env: export KRIA_CLOUD_API_KEY="..."
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
| `8080` | llama.cpp LLM server        | `config/default.toml` → `[llm]`    |

In production builds, ports 1420 and 3001 are **not used** — the Tauri app is self-contained.

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

It takes ~2–3 min to recompile everything from scratch after `cargo clean`.