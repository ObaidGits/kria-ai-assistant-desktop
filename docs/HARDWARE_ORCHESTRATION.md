# KRIA Hardware Orchestration

> Dynamic GPU/CPU layer offloading for local LLM inference — zero-downtime model migration based on real-time VRAM telemetry.

---

## Table of Contents

1. [Overview](#overview)
2. [Architecture](#architecture)
3. [Module Reference](#module-reference)
   - [Orchestrator (mod.rs)](#orchestrator-modrs)
   - [Telemetry (telemetry.rs)](#telemetry-telemetryrs)
   - [Strategy (strategy.rs)](#strategy-strategyrs)
   - [GPU Watchdog (gpu_watchdog.rs)](#gpu-watchdog-gpu_watchdogrs)
   - [Server Manager (server_manager.rs)](#server-manager-server_managerrs)
4. [GPU Backend Detection](#gpu-backend-detection)
5. [Layer Strategy Algorithm](#layer-strategy-algorithm)
6. [Watchdog State Machine](#watchdog-state-machine)
7. [Server Lifecycle Management](#server-lifecycle-management)
8. [Event System & Frontend Integration](#event-system--frontend-integration)
9. [Configuration Reference](#configuration-reference)
10. [Cross-Platform Behavior](#cross-platform-behavior)
11. [Security & Safety Design](#security--safety-design)
12. [Testing](#testing)
13. [Troubleshooting](#troubleshooting)

---

## Overview

The Hardware Orchestrator is a daemon embedded inside `kria-core` that dynamically monitors VRAM/RAM telemetry and migrates LLM computation between GPU and CPU. It manages the full lifecycle of `llama-server` — spawning, health-checking, swapping, and killing — to keep inference running optimally as system resources fluctuate (e.g., when a game or creative application competes for GPU memory).

### Key Capabilities

| Capability | Description |
|---|---|
| **Dynamic layer offloading** | Adjusts `--n-gpu-layers` in real time based on available VRAM |
| **Zero-downtime swaps** | Kills old server, waits for VRAM release, spawns new server with updated params |
| **Hysteresis-based watchdog** | Prevents thrashing with sustained-pressure detection and cooldown periods |
| **Emergency path** | Bypasses hysteresis for critically low VRAM — immediate SIGKILL + CPU fallback |
| **Recovery scaling** | Automatically scales back up when VRAM becomes available again |
| **Cross-platform** | NVML on Linux/Windows, RAM-based on macOS Metal, CPU-only fallback |
| **Ephemeral ports** | Uses `--port 0` for conflict-free server binding |
| **Stream cancellation** | Non-blocking `CancellationToken` aborts in-flight LLM streams during swap |
| **Frontend integration** | Swap progress overlay and degradation pill in the chat UI |

### Design Principles

- **Lock-free state reads**: `AtomicU8` for server state — any task can check health without contention
- **Non-blocking stream abort**: `CancellationToken` from `tokio-util` — streams `select!` on it
- **Rate limiting**: Max 6 transitions/hour, minimum |Δngl| ≥ 3 to prevent micro-adjustments
- **Fail-safe**: If a swap fails, the system falls back to CPU-only inference (ngl=0)
- **Non-fatal startup**: If the orchestrator fails to start, KRIA continues without it

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         KRIA Desktop (Tauri)                            │
│                                                                         │
│  init_runtime()                                                         │
│    │                                                                    │
│    ├─ Orchestrator::start()                                             │
│    │    ├─ GpuBackend::detect()        → Cuda | Metal | CpuOnly        │
│    │    ├─ create_cuda_telemetry()     → NvmlTelemetry | CliTelemetry  │
│    │    │                                 | RamTelemetry               │
│    │    ├─ calculate_target_params()   → TargetParams { ngl, ctx, … }  │
│    │    ├─ LlamaServerManager::spawn() → ephemeral port + health check │
│    │    └─ GpuWatchdog::run()          → telemetry polling loop        │
│    │                                                                    │
│    └─ model_router.attach_server_manager(orch.server_manager)          │
│         └─ LocalBackend now uses orchestrator's dynamic API URL        │
│                                                                         │
├─────────────────────────────────────────────────────────────────────────┤
│                          Event Flow                                     │
│                                                                         │
│  GpuWatchdog ──publish──▶ EventBus ──subscribe──▶ Tauri event emitter  │
│                               │                         │               │
│                               │    KriaEvent::           │               │
│                               │      VramPressure        │               │
│                               │      LlmSwapStarted     ▼               │
│                               │      LlmSwapCompleted   Frontend        │
│                               │      LlmDegradationChanged  │           │
│                               │      LlmStreamInterrupted   │           │
│                               │                              ▼           │
│                               │                     isSwapping()         │
│                               │                     degradationLevel()   │
│                               │                     swap-overlay UI      │
│                               │                     degradation-pill     │
└─────────────────────────────────────────────────────────────────────────┘
```

### Data Flow Summary

1. **Startup**: `Orchestrator::start()` detects GPU, reads initial telemetry, calculates optimal `(ngl, context, vision)`, spawns `llama-server`, starts the watchdog loop.
2. **Steady state**: Watchdog polls telemetry every `poll_interval_secs` (default 2s). If resources are stable, nothing happens.
3. **Pressure detected**: Free VRAM drops below `yield_threshold_mb` → watchdog enters **Yielding** state, waits for sustained breach (5s).
4. **Swap triggered**: Recalculates optimal params, publishes `LlmSwapStarted`, cancels streams, kills old server, waits for VRAM release (CUDA ghost prevention), spawns new server, publishes `LlmSwapCompleted`, enters **Cooldown**.
5. **Recovery**: Free VRAM rises above `recover_threshold_mb` → watchdog calculates if scaling up is worthwhile (Δngl ≥ `min_ngl_delta`), performs graceful swap upward.

---

## Module Reference

All orchestrator code lives under `crates/kria-core/src/llm/orchestrator/`.

```
orchestrator/
├── mod.rs              # Top-level Orchestrator struct, GpuBackend enum, OrchestratorSnapshot
├── telemetry.rs        # GpuTelemetry trait + NvmlTelemetry, CliTelemetry, RamTelemetry
├── strategy.rs         # Layer strategy calculator, DegradationLevel, TargetParams
├── gpu_watchdog.rs     # Telemetry polling loop, hysteresis state machine
└── server_manager.rs   # llama-server process lifecycle, port discovery, health checks
```

### Orchestrator (`mod.rs`)

The top-level entry point that wires all sub-modules together.

#### `GpuBackend` enum

```rust
pub enum GpuBackend {
    Cuda,     // NVIDIA GPU (Linux/Windows) — full VRAM-based orchestration
    Metal,    // Apple Silicon (macOS) — unified memory, RAM-based telemetry
    CpuOnly,  // No discrete GPU — CPU-only inference
}
```

Detection priority:
1. **macOS** → always `Metal` (unified memory architecture)
2. **NVML init succeeds** → `Cuda` (requires `nvidia` feature flag)
3. **`nvidia-smi` CLI works** → `Cuda` (CLI fallback without NVML bindings)
4. **Otherwise** → `CpuOnly`

#### `OrchestratorSnapshot`

Read-only view of current state, returned by `Orchestrator::snapshot()` and exposed via the `get_orchestrator_status` Tauri command:

```rust
pub struct OrchestratorSnapshot {
    pub backend: GpuBackend,       // Detected GPU backend
    pub current_ngl: u32,          // Active GPU layers
    pub current_context: u32,      // Active context window
    pub degradation: DegradationLevel,  // Current quality level
    pub server_healthy: bool,      // Is llama-server responding?
}
```

#### `Orchestrator` struct

| Method | Description |
|---|---|
| `start(config, model_path, mmproj_path, event_bus, health)` | Async constructor — detects GPU, spawns server, starts watchdog |
| `snapshot()` | Returns current `OrchestratorSnapshot` |
| `api_url()` | Current `llama-server` API URL (ephemeral port) |
| `ensure_ready(reason)` | Preflight runtime warm-up — starts/restarts server on demand before a turn |
| `restart(reason)` | Bounded restart with cooldown + fallback spawn path |
| `release_if_idle(reason)` | Gracefully releases local runtime (and GPU VRAM) during safe idle windows |
| `shutdown()` | Graceful shutdown — aborts watchdog, kills server |

---

### Telemetry (`telemetry.rs`)

Provides real-time GPU/RAM metrics through the `GpuTelemetry` trait.

#### `TelemetrySnapshot`

```rust
pub struct TelemetrySnapshot {
    pub free_vram_mb: u64,      // Free VRAM (CUDA) or free RAM (Metal/CpuOnly)
    pub total_vram_mb: u64,     // Total VRAM or total RAM
    pub gpu_util_pct: Option<u32>,  // GPU utilization 0-100%, if available
}
```

#### Telemetry Providers

| Provider | Platform | Feature Gate | Data Source | Fidelity |
|---|---|---|---|---|
| `NvmlTelemetry` | Linux, Windows | `nvidia` | NVML C library bindings | High — direct VRAM query, GPU utilization |
| `CliTelemetry` | Linux, Windows | None | `nvidia-smi` CLI subprocess | Medium — subprocess overhead, ~100ms per query |
| `RamTelemetry` | All | None | `sysinfo` crate | Low — RAM only, no GPU metrics |

#### Factory: `create_cuda_telemetry()`

Cascading fallback chain:
1. Try `NvmlTelemetry::try_new(0)` — requires `nvidia` feature + working NVML driver
2. Try `CliTelemetry::try_new()` — requires `nvidia-smi` on `$PATH`
3. Fall back to `RamTelemetry::new()` — always works, uses system RAM as proxy

All providers return **zero/dummy values on failure** rather than propagating errors. The watchdog handles degraded telemetry gracefully.

#### Thread Safety

- `NvmlTelemetry`: `Nvml` handle is `Send + Sync`; reads are lock-free
- `CliTelemetry`: Uses `tokio::task::spawn_blocking` for the subprocess call to avoid blocking the tokio runtime
- `RamTelemetry`: Wraps `sysinfo::System` in `std::sync::Mutex` because `sysinfo` is not `Send`

---

### Strategy (`strategy.rs`)

Pure-function VRAM budget calculator. No I/O, no async — deterministic mapping from `(ModelProfile, free_vram_mb, safety_margin, backend)` → `TargetParams`.

#### `DegradationLevel` enum

Ordered from best to worst quality:

| Level | Description | Criteria |
|---|---|---|
| `Full` | All layers on GPU, full context, vision enabled | `ngl == total_layers && context >= max_context` |
| `ReducedContext` | All layers on GPU but context is reduced | `ngl == total_layers && context < max_context` |
| `PartialOffload` | Some layers offloaded to CPU | `ngl >= total_layers/2` |
| `HeavyOffload` | Heavy CPU offload, reduced context | `ngl < total_layers/2` |
| `CpuOnly` | Full CPU inference (ngl=0) | `ngl == 0` |

#### `TargetParams`

```rust
pub struct TargetParams {
    pub ngl: u32,              // Number of GPU layers to offload
    pub context: u32,          // Context window size in tokens
    pub enable_vision: bool,   // Whether to load mmproj
    pub degradation: DegradationLevel,
}
```

#### CUDA Budget Algorithm

```
calculate_target_params(profile, free_vram_mb, safety_margin_mb, backend):

  1. available = free_vram_mb - safety_margin_mb
  2. if available < base_vram_overhead_mb → CpuOnly (ngl=0, min_context)
  3. budget_after_base = available - base_vram_overhead_mb
  4. max_layers = budget_after_base / per_layer_vram_mb
  5. ngl = min(max_layers, total_layers)
  6. vram_used_by_layers = ngl × per_layer_vram_mb
  7. remaining = budget_after_base - vram_used_by_layers
  8. context = clamp(remaining × 1024 / kv_per_1k_ctx_mb, min_context, max_context)
  9. enable_vision = has_vision_projector AND ngl ≥ 15
```

**Vision threshold**: Vision (mmproj) is disabled when `ngl < 15` because there isn't enough GPU capacity to meaningfully accelerate the vision encoder.

#### Metal Algorithm

Apple Silicon uses unified memory, so all layers are always fully offloaded:

```
calculate_metal_params(profile, free_ram_mb):

  1. ngl = total_layers  (always full offload)
  2. usable = free_ram_mb - 2048  (reserve 2GB for system)
  3. context = clamp(usable × 1024 / kv_per_1k_ctx_mb, min_context, max_context)
  4. enable_vision = has_vision_projector  (always on if model supports it)
```

#### Worked Example (RTX 4050, 6GB VRAM)

Given the default `ModelProfile` for Qwen2.5-VL-7B:

| Parameter | Value |
|---|---|
| `total_layers` | 35 |
| `per_layer_vram_mb` | 128 MB |
| `base_vram_overhead_mb` | 200 MB |
| `kv_per_1k_ctx_mb` | 100 MB |
| `safety_margin_mb` | 256 MB |

**Scenario A — 5.5GB free VRAM (desktop idle)**
```
available = 5632 - 256 = 5376 MB
budget = 5376 - 200 = 5176 MB
layers = 5176 / 128 = 40 → clamped to 35 (all layers)
remaining = 5176 - 4480 = 696 MB → context = 696 × 1024 / 100 = 7127 tokens
Result: ngl=35, ctx=7127, vision=on, degradation=ReducedContext
```

**Scenario B — 3GB free VRAM (game running)**
```
available = 3072 - 256 = 2816 MB
budget = 2816 - 200 = 2616 MB
layers = 2616 / 128 = 20
remaining = 2616 - 2560 = 56 MB → context = 573 → clamped to 2048 (min)
Result: ngl=20, ctx=2048, vision=on (20 ≥ 15), degradation=PartialOffload
```

**Scenario C — 300MB free VRAM (heavy GPU load)**
```
available = 300 - 256 = 44 MB
44 < 200 (base overhead) → CpuOnly
Result: ngl=0, ctx=2048, vision=off, degradation=CpuOnly
```

---

### GPU Watchdog (`gpu_watchdog.rs`)

Telemetry polling loop with a hysteresis-based state machine that decides when to trigger server swaps.

#### State Machine

```
                      ┌─────────────────────────┐
                      │                         │
                      ▼                         │
                 ┌────────┐   pressure    ┌──────────┐
    ───start───▶ │  Idle  │─────────────▶ │ Yielding │
                 └────────┘               └──────────┘
                   ▲    │                    │     │
                   │    │ free > recover     │     │ sustained > 5s
                   │    │   └─ try_recover() │     │   AND rate ok
                   │    │                    │     │
                   │    │   pressure gone    │     ▼
                   │    │◀──────────────────┘  ┌──────────┐
                   │    │                      │ Swapping │
                   │    │                      └──────────┘
                   │    │                          │
                   │    │     cooldown expired     │ swap done
                   │    └──────────────────────┐   │
                   │                           │   ▼
                   │                      ┌──────────┐
                   └──────────────────────│ Cooldown │
                                          └──────────┘

         Emergency path (any state except Cooldown):
         free < emergency_threshold → bypass hysteresis → Swapping → Cooldown
```

#### States

| State | Description | Duration |
|---|---|---|
| **Idle** | Normal operation — polling telemetry every `poll_interval_secs` | Until pressure detected |
| **Yielding** | VRAM pressure detected, waiting for sustained breach | 5 seconds |
| **Cooldown** | Post-swap stabilization period, no swaps allowed | `cooldown_secs` (default 60s) |

#### Safety Mechanisms

| Mechanism | Config Key | Default | Purpose |
|---|---|---|---|
| **Hysteresis** | (hardcoded 5s) | 5s | Prevents swap on transient VRAM spikes |
| **Cooldown** | `cooldown_secs` | 60s | Prevents rapid back-and-forth swapping |
| **Rate limit** | `max_transitions_per_hour` | 6 | Hard cap on hourly swap count |
| **Min delta** | `min_ngl_delta` | 3 | Prevents micro-adjustments (|Δngl| < 3 → skip) |
| **Emergency bypass** | `emergency_threshold_mb` | 128 MB | Bypasses all hysteresis for critical VRAM |

#### Swap Execution Flow

```
handle_swap(free_vram_mb, emergency):

  1. Read current params: (old_ngl, old_ctx)
  2. Calculate new: calculate_target_params(profile, free_vram_mb, ...)
  3. Check min delta: |old_ngl - new_ngl| < min_ngl_delta → skip (unless emergency)
  4. Publish LlmSwapStarted event
  5. Cancel in-flight streams (CancellationToken)
  6. Publish LlmStreamInterrupted event
  7. Kill old server (emergency → immediate kill, yield → graceful stop)
  8. Wait for VRAM release (CUDA only — poll until free VRAM rises, 5s timeout)
  9. Spawn new server with new params
  10. Publish LlmSwapCompleted + LlmDegradationChanged events
  11. On spawn failure → emergency CPU-only fallback (ngl=0, min_context)
```

#### Recovery Flow

When free VRAM rises above `recover_threshold_mb`, the watchdog proactively scales up:

```
try_recover(free_vram_mb):

  1. Read current params
  2. Calculate optimal params from current VRAM
  3. Check if improvement is meaningful: ngl_gain ≥ min_ngl_delta OR ctx improved
  4. If yes → graceful swap upward (same event flow as handle_swap)
  5. If no → do nothing
```

---

### Server Manager (`server_manager.rs`)

Manages a single `llama-server` child process with atomic state tracking.

#### Key Design Decisions

| Decision | Rationale | Vulnerability Addressed |
|---|---|---|
| `AtomicU8` for state | Lock-free reads from any task | V7: RwLock deadlock prevention |
| `CancellationToken` | Non-blocking stream abort | V13: Stream interruption during swap |
| `--port 0` (ephemeral) | No port conflicts across swaps | V5: Port collision, V14: Bind failure |
| `kill_on_drop(true)` | Child process cleaned up if manager drops | Zombie process prevention |
| Stderr port parsing | Discover actual bound port | Supports ephemeral port allocation |

#### Server States

```rust
const STATE_STOPPED:  u8 = 0;  // No server running
const STATE_STARTING: u8 = 1;  // Spawning + waiting for port/health
const STATE_READY:    u8 = 2;  // Healthy, accepting requests
const STATE_SWAPPING: u8 = 3;  // Mid-swap (old killed, new spawning)
const STATE_ERROR:    u8 = 4;  // Spawn or health-check failure
```

State transitions are atomic (`Ordering::Acquire` / `Release`) — any task can read state without holding a lock.

#### `LlamaServerManager` API

| Method | Description |
|---|---|
| `new(config, model_path, mmproj_path)` | Constructor — no I/O |
| `spawn(ngl, context, enable_vision, event_bus)` | Start `llama-server` with params, discover port, health-check |
| `state()` → `u8` | Lock-free state read |
| `is_healthy()` → `bool` | True if `STATE_READY` |
| `is_swapping()` → `bool` | True if `STATE_SWAPPING` |
| `current_params()` → `(u32, u32)` | Current `(ngl, context)` |
| `api_url()` → `String` | Current `http://127.0.0.1:{port}/v1` |
| `cancel_token()` → `CancellationToken` | Token for stream cancellation via `select!` |
| `cancel_streams()` | Cancel all in-flight streams |
| `graceful_stop()` | SIGTERM → wait 10s → kill |
| `kill()` | Immediate SIGKILL (emergency path) |

#### Spawn Sequence

```
spawn(ngl, context, enable_vision):

  1. Set state → STARTING
  2. Build command:
     llama-server --model <path> --port 0 --ctx-size <ctx>
                  --n-gpu-layers <ngl> --batch-size <batch>
                  [--flash-attn] [--mlock] [--mmproj <path>]
  3. Spawn child process (kill_on_drop=true)
  4. Parse stderr for "listening on ...:<port>" (60s timeout)
  5. Spawn background task to forward remaining stderr to tracing::debug
  6. Poll GET /health every 500ms until 200 OK (120s timeout)
  7. Store API URL, update AtomicU32 params, set state → READY
```

#### Port Discovery

The server manager parses `llama-server`'s stderr for the listening port:

```
main: server is listening on http://127.0.0.1:43567
                                               ^^^^^
                                          Extracted port
```

Pattern matching: looks for lines containing `"listening"`, extracts the last `:<digits>` segment.

#### Graceful Stop vs Kill

| Method | Signal | Wait | Use Case |
|---|---|---|---|
| `graceful_stop()` | SIGTERM (via `start_kill()`) | 10s then SIGKILL | Yield swaps, recovery |
| `kill()` | SIGKILL (via `kill()`) | None | Emergency path |

Both methods:
1. Set state → `SWAPPING`
2. Cancel all streams via `CancellationToken`
3. Abort the stderr reader task
4. Set state → `STOPPED`

---

## GPU Backend Detection

Detection runs at orchestrator startup and is **platform-aware**:

```
GpuBackend::detect():

  macOS         → Metal (always)
  Linux/Windows → Try NVML init (feature "nvidia")
                  → Try nvidia-smi CLI
                  → CpuOnly (no GPU found)
```

| Backend | Platform | Telemetry Source | Dynamic ngl? | Vision Support |
|---|---|---|---|---|
| `Cuda` | Linux, Windows | NVML or nvidia-smi | Yes — full VRAM-based | Yes (ngl ≥ 15) |
| `Metal` | macOS | RAM (sysinfo) | No — always full offload | Always on |
| `CpuOnly` | Any | RAM (sysinfo) | No — always ngl=0 | Off |

### Feature Flag: `nvidia`

```toml
# crates/kria-core/Cargo.toml
[features]
nvidia = ["nvml-wrapper"]

# crates/kria-desktop/Cargo.toml
[features]
nvidia = ["kria-core/nvidia"]
```

Build with NVML support:
```bash
cargo tauri dev --features nvidia     # Development
cargo tauri build --features nvidia   # Release
```

Without the feature, NVML bindings are excluded at compile time. The system still works via `nvidia-smi` CLI fallback.

---

## Event System & Frontend Integration

### Event Bus Events

The orchestrator publishes five event types through KRIA's `EventBus`:

| Event | When | Payload |
|---|---|---|
| `VramPressure` | Free VRAM drops below yield/emergency threshold | `{ free_vram_mb: u64 }` |
| `LlmSwapStarted` | Swap begins (before server kill) | `{ from_ngl, to_ngl, emergency }` |
| `LlmSwapCompleted` | New server is healthy after swap | `{ new_ngl, new_context, duration_ms }` |
| `LlmDegradationChanged` | Quality level changed | `{ level: String }` |
| `LlmStreamInterrupted` | In-flight stream cancelled by swap | (no payload) |

### Tauri Event Forwarding

A dedicated `tokio::spawn` task subscribes to the `EventBus` and forwards orchestrator events to the Tauri frontend:

| EventBus Event | Tauri Event Name | JSON Payload |
|---|---|---|
| `LlmSwapStarted` | `orchestrator:swap_started` | `{ from_ngl, to_ngl, emergency }` |
| `LlmSwapCompleted` | `orchestrator:swap_completed` | `{ new_ngl, new_context, duration_ms }` |
| `LlmDegradationChanged` | `orchestrator:degradation_changed` | `{ level }` |
| `VramPressure` | `orchestrator:vram_pressure` | `{ free_vram_mb }` |

### Frontend Signals (SolidJS)

```typescript
// ui/src/stores/app.ts
const [isSwapping, setIsSwapping] = createSignal(false);
const [degradationLevel, setDegradationLevel] = createSignal<string | null>(null);
```

| Signal | Updated By | Purpose |
|---|---|---|
| `isSwapping()` | `orchestrator:swap_started` → true, `orchestrator:swap_completed` → false | Disables input, shows overlay |
| `degradationLevel()` | `orchestrator:degradation_changed` | Shows quality pill when not "Full" |

### UI Components

#### Swap Overlay

Shown over the chat area while a swap is in progress (`isSwapping() === true`):

```tsx
<Show when={isSwapping()}>
  <div class="swap-overlay">
    <div class="swap-overlay-content">
      <span class="dot" /><span class="dot" /><span class="dot" />
      <span class="swap-label">Optimizing GPU layers…</span>
    </div>
  </div>
</Show>
```

Three animated dots with staggered pulse animation. The textarea and send button are disabled during swap.

#### Degradation Pill

Shown when the model is operating below full quality:

```tsx
<Show when={degradationLevel() && degradationLevel() !== "Full"}>
  <div class="degradation-pill">{degradationLevel()}</div>
</Show>
```

Displays the current `DegradationLevel` (e.g., "ReducedContext", "PartialOffload") in a warning-colored pill badge.

### Tauri Command: `get_orchestrator_status`

```typescript
// Invoke from frontend
const status = await invoke("get_orchestrator_status");
// Returns:
// { enabled: true, backend: "Cuda", current_ngl: 35, current_context: 8192,
//   degradation: "Full", server_healthy: true, api_url: "http://127.0.0.1:43567/v1" }
// or:
// { enabled: false }
```

---

## Configuration Reference

All orchestrator settings live under `[orchestrator]` in `config/default.toml`.

### Main Settings

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Master switch — when false, llama-server is not managed by the orchestrator |
| `poll_interval_secs` | u64 | `2` | How often the watchdog reads telemetry (seconds) |
| `yield_threshold_mb` | u64 | `512` | Free VRAM (MB) below which a yield swap triggers after sustained breach |
| `emergency_threshold_mb` | u64 | `128` | Free VRAM (MB) below which an emergency swap fires immediately |
| `recover_threshold_mb` | u64 | `2048` | Free VRAM (MB) above which the system tries to scale back up |
| `cooldown_secs` | u64 | `60` | Minimum seconds between non-emergency swaps |
| `max_transitions_per_hour` | u32 | `6` | Hard cap on swap transitions in a sliding 1-hour window |
| `min_ngl_delta` | u32 | `3` | Minimum |Δngl| to trigger a swap (prevents micro-adjustments) |
| `safety_margin_mb` | u64 | `256` | VRAM (MB) reserved as a buffer to prevent OOM |
| `llama_server_binary` | String | `"llama-server"` | Path or name of the llama-server binary |
| `flash_attention` | bool | `true` | Pass `--flash-attn` to llama-server |
| `mlock` | bool | `true` | Pass `--mlock` to llama-server (lock model weights in RAM) |
| `batch_size` | u32 | `256` | Pass `--batch-size` to llama-server |
| `graceful_stop_timeout_secs` | u64 | `5` | Max wait for graceful stop before kill escalation |
| `health_check_timeout_secs` | u64 | `120` | Spawn readiness timeout for llama-server health probe |
| `port_discovery_timeout_secs` | u64 | `60` | Max wait for ephemeral port discovery from server logs |
| `vram_release_timeout_secs` | u64 | `5` | Max wait to confirm VRAM release after stop/swap |
| `restart_cooldown_secs` | u64 | `10` | Minimum cooldown between restart attempts |
| `restart_backoff_ms` | u64 | `350` | Backoff before fallback spawn after restart failure |
| `idle_release_enabled` | bool | `true` | Enable idle-time runtime release to free GPU memory |
| `idle_release_after_secs` | u64 | `300` | Idle window before releasing local runtime |
| `idle_release_check_interval_secs` | u64 | `10` | Poll interval for idle-release checks |

### macOS-specific Thresholds

| Key | Type | Default | Description |
|---|---|---|---|
| `macos_yield_ram_mb` | u64 | `2048` | macOS: free RAM (MB) below which yield triggers |
| `macos_emergency_ram_mb` | u64 | `1024` | macOS: free RAM (MB) below which emergency triggers |
| `macos_recover_ram_mb` | u64 | `4096` | macOS: free RAM (MB) above which recovery is allowed |

### Model Profile (`[orchestrator.model_profile]`)

| Key | Type | Default | Description |
|---|---|---|---|
| `total_layers` | u32 | `35` | Total transformer layers in the model (Qwen2.5-VL-7B = 35) |
| `per_layer_vram_mb` | u32 | `128` | Approximate VRAM per offloaded layer (MB) |
| `base_vram_overhead_mb` | u32 | `200` | Base VRAM overhead for CUDA context + embeddings (MB) |
| `kv_per_1k_ctx_mb` | u32 | `100` | KV cache VRAM per 1024 context tokens (MB) |
| `min_context` | u32 | `2048` | Hard floor — context never goes below this |
| `max_context` | u32 | `8192` | Maximum context window |
| `has_vision_projector` | bool | `true` | Whether the model has a vision projector (mmproj) |

### Example: Custom Model Profile

```toml
[orchestrator.model_profile]
total_layers = 80        # Llama 3.1 70B
per_layer_vram_mb = 256
base_vram_overhead_mb = 500
kv_per_1k_ctx_mb = 200
min_context = 4096
max_context = 131072
has_vision_projector = false
```

---

## Cross-Platform Behavior

### Linux (Primary Target)

- **GPU detection**: NVML → nvidia-smi → CpuOnly
- **Telemetry**: NVML (sub-ms queries) or nvidia-smi (~100ms per query)
- **Dynamic offloading**: Full VRAM-based ngl adjustment
- **Server signals**: `start_kill()` sends SIGTERM; `kill()` sends SIGKILL

### Windows

- **GPU detection**: NVML → nvidia-smi → CpuOnly (identical to Linux)
- **Telemetry**: Same cascade
- **Dynamic offloading**: Full VRAM-based ngl adjustment
- **Server signals**: `start_kill()` calls `TerminateProcess`; `kill()` also terminates

### macOS (Apple Silicon)

- **GPU detection**: Always `Metal`
- **Telemetry**: `RamTelemetry` (sysinfo-based RAM monitoring)
- **Dynamic offloading**: **Static** — all layers always on GPU (unified memory). Only context window adjusts.
- **Thresholds**: Uses `macos_yield_ram_mb` / `macos_emergency_ram_mb` / `macos_recover_ram_mb` instead of VRAM thresholds
- **Vision**: Always enabled if model supports it

---

## Security & Safety Design

### VRAM Safety Margin

A `safety_margin_mb` (default 256 MB) is always reserved from the VRAM budget. This prevents the orchestrator from allocating so aggressively that `llama-server` triggers an OOM or the GPU driver crashes.

### Rate Limiting

- **Max transitions per hour**: A sliding-window counter caps swaps at `max_transitions_per_hour` (default 6). If exceeded, the watchdog enters Cooldown instead of swapping.
- **Min delta**: Swaps with |Δngl| < `min_ngl_delta` (default 3) are suppressed to prevent micro-adjustments from tiny VRAM fluctuations.
- **Cooldown**: After every swap, the watchdog enters a `cooldown_secs` period where no further swaps are allowed (except emergency).

### Emergency CPU Fallback

If a swap spawn fails (e.g., the new server crashes on startup), the system immediately falls back to CPU-only inference:

```rust
Err(e) => {
    tracing::error!(?e, "watchdog: failed to spawn new server after swap");
    let _ = self.server.spawn(0, min_context, false, event_bus).await;
}
```

This ensures KRIA never enters a state where no LLM backend is available.

### CUDA Ghost Prevention

After killing `llama-server`, NVIDIA drivers may not immediately release VRAM. The watchdog polls telemetry up to 5 seconds waiting for free VRAM to actually increase before spawning the new server:

```rust
async fn wait_for_vram_release(&self) {
    let timeout = Duration::from_secs(5);
    loop {
        if elapsed > timeout { break; }
        if snap.free_vram_mb > yield_threshold_mb { break; }
        sleep(200ms).await;
    }
}
```

### Process Isolation

- `kill_on_drop(true)` on the child process ensures cleanup if the manager panics
- Stderr is captured and forwarded to structured logging (`tracing::debug`)
- The child process runs in its own process group

### Non-Fatal Startup

If the orchestrator fails to start (e.g., llama-server binary not found), KRIA continues without it:

```rust
Err(e) => {
    tracing::error!("orchestrator: failed to start (non-fatal): {e}");
    health.update("orchestrator", ServiceStatus::Degraded, Some(format!("{e}")));
    None
}
```

---

## Testing

### Unit Tests (11 total, all passing)

#### Strategy Tests (`strategy.rs`)

| Test | Validates |
|---|---|
| `full_vram_gives_full_params` | 6GB VRAM → all 35 layers, vision on, degradation=Full |
| `low_vram_forces_cpu_only` | 300MB VRAM → ngl=0, min_context, vision off, CpuOnly |
| `moderate_vram_gives_partial_offload` | 3GB VRAM → ~20 layers, vision on (20 ≥ 15) |
| `vision_disabled_below_ngl_15` | 2GB VRAM → ~12 layers, vision off (12 < 15) |
| `metal_always_full_layers` | Metal backend → all layers regardless of free RAM |
| `cpu_only_backend` | CpuOnly backend → ngl=0, no vision, even with 8GB free |
| `context_floor_enforced` | Very low VRAM → context ≥ min_context (2048) |

#### Server Manager Tests (`server_manager.rs`)

| Test | Validates |
|---|---|
| `extract_port_from_standard_line` | Parses `"...http://127.0.0.1:43567"` → 43567 |
| `extract_port_from_plain_line` | Parses `"...0.0.0.0:8080"` → 8080 |
| `no_port_from_unrelated_line` | Non-listening log line → None |
| `state_transitions` | AtomicU8 state transitions: STOPPED → READY → SWAPPING |

### Running Tests

```bash
# Run all orchestrator tests
cargo test -p kria-core --lib -- llm::orchestrator

# Run with output
cargo test -p kria-core --lib -- llm::orchestrator --nocapture

# Run a specific test
cargo test -p kria-core --lib -- llm::orchestrator::strategy::tests::full_vram_gives_full_params
```

---

## Troubleshooting

### Orchestrator Not Starting

**Symptom**: Log shows `"orchestrator: failed to start (non-fatal)"`.

**Causes**:
1. `llama-server` binary not on `$PATH` — set `orchestrator.llama_server_binary` to the full path
2. No model configured — ensure `config.llm.models[0].file` points to a valid `.gguf` file
3. Model file missing — check that the `.gguf` exists in `models/llm/`

**Check**: `which llama-server` and verify the model path in `config/default.toml`.

### Excessive Swapping

**Symptom**: Log shows frequent swap messages, UI overlay flashes repeatedly.

**Fix**: Increase thresholds to reduce sensitivity:
```toml
[orchestrator]
cooldown_secs = 120              # Longer cooldown between swaps
max_transitions_per_hour = 3     # Fewer swaps per hour
min_ngl_delta = 5                # Larger minimum change to trigger swap
yield_threshold_mb = 768         # Higher threshold = less sensitive
```

### Stuck in CPU-Only Mode

**Symptom**: `degradation = "CpuOnly"` even though GPU has available VRAM.

**Causes**:
1. NVML not initialized — build with `--features nvidia`
2. `nvidia-smi` not on PATH — install NVIDIA drivers
3. `recover_threshold_mb` too high — the system needs that much free VRAM to scale back up

**Debug**: Query the status endpoint:
```typescript
const status = await invoke("get_orchestrator_status");
console.log(status);
// Check backend, current_ngl, degradation
```

### Port Conflicts

**Symptom**: `"error when starting dev server: Port 1420 is already in use"`.

The orchestrator uses ephemeral ports (`--port 0`) so it does not conflict with Vite's dev server port. This error is a Vite issue — the `beforeDevCommand` now runs `fuser -k 1420/tcp` to kill stale Vite processes before starting.

### Inspecting Orchestrator Logs

All orchestrator logging uses structured tracing with the `orchestrator:`, `watchdog:`, `server_manager:`, and `llama-server` prefixes:

```bash
# Run with debug logging for orchestrator modules
RUST_LOG=kria_core::llm::orchestrator=debug cargo tauri dev --features nvidia
```

Key log lines to look for:
```
orchestrator: detected GPU backend        → Shows Cuda/Metal/CpuOnly
orchestrator: initial parameters           → Initial ngl/ctx/degradation
server_manager: spawning llama-server      → Server command and params
server_manager: discovered ephemeral port  → Bound port number
server_manager: llama-server is ready      → Health check passed
watchdog: starting telemetry loop          → Watchdog is running
watchdog: VRAM pressure detected           → Yield state entered
watchdog: executing swap                   → Swap in progress
watchdog: swap completed                   → New server healthy
watchdog: recovery completed               → Scaled back up
```

### Resetting Orchestrator State

If the orchestrator is in a bad state, you can reset it by toggling the config:

```toml
[orchestrator]
enabled = false   # Disable, restart, then re-enable
```

Or restart the application — the orchestrator re-detects everything from scratch on startup.

---

## Dependencies

| Crate | Version | Purpose | Feature Gated |
|---|---|---|---|
| `nvml-wrapper` | 0.10 | NVIDIA Management Library bindings | `nvidia` feature |
| `tokio-util` | 0.7 | `CancellationToken` for stream abort | No |
| `sysinfo` | 0.32 | RAM monitoring (macOS/CpuOnly fallback) | No |
| `reqwest` | (workspace) | Health endpoint polling | No |
| `async-trait` | (workspace) | `GpuTelemetry` trait async methods | No |

---

## File Map

| File | Lines | Responsibility |
|---|---|---|
| `crates/kria-core/src/llm/orchestrator/mod.rs` | ~130 | Entry point, `GpuBackend`, `Orchestrator`, `OrchestratorSnapshot` |
| `crates/kria-core/src/llm/orchestrator/telemetry.rs` | ~230 | `GpuTelemetry` trait, NVML/CLI/RAM providers, factory |
| `crates/kria-core/src/llm/orchestrator/strategy.rs` | ~190 | VRAM budget calculator, `DegradationLevel`, `TargetParams`, 7 tests |
| `crates/kria-core/src/llm/orchestrator/gpu_watchdog.rs` | ~280 | Polling loop, state machine, swap/recover logic |
| `crates/kria-core/src/llm/orchestrator/server_manager.rs` | ~300 | Process lifecycle, port discovery, health checks, 4 tests |
| `crates/kria-core/src/config.rs` | (section) | `OrchestratorConfig`, `ModelProfile` structs + defaults |
| `crates/kria-core/src/infra/event_bus.rs` | (section) | 5 orchestrator event variants in `KriaEvent` |
| `crates/kria-desktop/src/commands.rs` | (section) | `init_runtime()` orchestrator startup, event forwarding, Tauri command |
| `crates/kria-desktop/src/main.rs` | (line) | `get_orchestrator_status` registered in invoke_handler |
| `config/default.toml` | (section) | `[orchestrator]` + `[orchestrator.model_profile]` defaults |
| `ui/src/stores/app.ts` | (lines) | `isSwapping`, `degradationLevel` signals + event listeners |
| `ui/src/components/ChatView.tsx` | (section) | Swap overlay, degradation pill, disabled input during swap |
| `ui/src/styles/global.css` | (section) | `.swap-overlay`, `.degradation-pill` styles + pulse animation |
