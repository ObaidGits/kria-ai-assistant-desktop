# KRIA — Testing Guide

## Overview

The test suite is organized into four layers that run automatically on every push/PR via GitHub Actions:

| Layer | Framework | Location | What it covers |
|-------|-----------|----------|----------------|
| **Rust unit/feature tests** | `cargo test` | `crates/kria-core/tests/` | Config loading, safety engine, memory store, vectors, embeddings |
| **Rust integration tests** | `cargo test` | `crates/kria-server/tests/` | REST API endpoints, WebSocket protocol, CORS, error handling |
| **Phase-B domain tests** | `cargo test` | `crates/kria-core/tests/test_*.rs` | All 175 AI-assistant prompts across 25 sections |
| **Playwright API tests** | Playwright (TS) | `tests/e2e/tests/*.api.spec.ts` | HTTP contract validation, WS integration |
| **Playwright E2E tests** | Playwright (TS) | `tests/e2e/tests/*.e2e.spec.ts` | Full browser user journeys (chat, settings, voice) |

---

## Phase-B Test Files (AI Assistant Coverage)

| File | Prompt IDs covered | Notes |
|------|--------------------|-------|
| `tests/common/mod.rs` | — | Shared helpers: MockLlmServer, SandboxDir, env guards |
| `test_smoke_system.rs` | SYS-01..10, PWR-01..08, CFG-01..09, PROC-01..08 | §1 System, §2 Power, §3 Config, §5 Process |
| `test_files.rs` | FS-01..FS-19 | §4 File System (sandbox-only writes) |
| `test_internet.rs` | NET-01..11, DOC-01..07 | §6 Network/Web, §7 Documents |
| `test_vision.rs` | VIS-01..06 | §8 Vision & Image |
| `test_interaction_desktop.rs` | DT-01..10, COM-01..03 | §9 Interaction/Desktop, §10 Notifications |
| `test_dev_packages.rs` | SH-01..06, PKG-01..08, SCHED-01..03, GIT-01..10 | §11 Shell, §12 Packages, §13 Scheduling, §15 Git |
| `test_memory_knowledge.rs` | MEM-01..11, AUTO-01..03 | §14 Memory/Knowledge, §16 Proactive |
| `test_gworkspace_mcp.rs` | I18N-01..03, GW-01..21, COLAB-01..06, MCP-FS-01..03 | §17 i18n, §18 GWorkspace, §19 Colab, §20 MCP |
| `test_safety_routing.rs` | SAFE-01..10, ROUTE-01..10 | §22 Safety/Policy, §23 Routing/Disambiguation |
| `test_multistep.rs` | CHAIN-01..07, SIDE-01..05 | §21 Multi-Step, §25 Sidecar |
| `quality_hallucination_tests.rs` | SYS-01, SYS-02, NET-01, FS-01, GW-01, CRITICAL-1..3, HALLUC-01..03 | Gated: `KRIA_REAL_LLM=1` |
| `voice_live_tests.rs` | VOICE-01..06 | Gated: `KRIA_VOICE_LIVE=1`, `--test-threads=1` |
| `dangerous_live_tests.rs` | DANGER-T1..T3 | Tier-1/2 always run; Tier-3 `#[ignore]` + `KRIA_DANGEROUS=1` |

### Prompt-ID → Test Name Traceability Matrix (selected entries)

| Prompt ID | Test name |
|-----------|-----------|
| SYS-01 | `test_smoke_system::functional_sys01_get_cpu_usage` |
| SYS-05 | `test_smoke_system::policy_sys05_shutdown_is_red` |
| FS-09 | `test_files::functional_fs09_delete_file_removes_it` |
| NET-02 | `test_internet::functional_net02_fetch_webpage_rust_lang` |
| VIS-03 | `test_vision::functional_vis03_screenshot_creates_file` |
| DT-05 | `test_interaction_desktop::functional_dt05_clipboard_roundtrip` |
| GIT-08 | `test_dev_packages::policy_git08_git_push_is_red` |
| MEM-01 | `test_memory_knowledge::functional_mem01_mem02_remember_recall_roundtrip` |
| GW-04 | `test_gworkspace_mcp::policy_gw04_gmail_send_is_red` |
| SAFE-01 | `test_safety_routing::policy_safe01_write_to_blocked_paths_is_red` |
| SAFE-05 | `test_safety_routing::policy_safe05_catastrophic_bash_is_red` |
| CHAIN-02 | `test_multistep::functional_chain02_read_transform_write` |
| CRITICAL-1 | `quality_hallucination_tests::quality_critical_system_stats_uses_tool` |
| VOICE-01 | `voice_live_tests::voice01_hey_ria_wake_word_detected` |

---

## Quick Start

### Run all Rust tests (standard, safe — no destructive ops)

```bash
cargo test --workspace
```

### Run Phase-B domain tests only

```bash
cargo test -p kria-core --test test_smoke_system
cargo test -p kria-core --test test_files
cargo test -p kria-core --test test_internet
cargo test -p kria-core --test test_vision
cargo test -p kria-core --test test_interaction_desktop
cargo test -p kria-core --test test_dev_packages
cargo test -p kria-core --test test_memory_knowledge
cargo test -p kria-core --test test_gworkspace_mcp
cargo test -p kria-core --test test_safety_routing
cargo test -p kria-core --test test_multistep
cargo test -p kria-core --test dangerous_live_tests
```

### Run a specific test file (original Phase-A files)

```bash
cargo test -p kria-core --test config_tests
cargo test -p kria-core --test safety_tests
cargo test -p kria-core --test memory_tests
cargo test -p kria-server --test integration_api
cargo test -p kria-server --test integration_ws
```

### Run quality / hallucination tests (requires local Phi-4-mini at localhost:8080)

```bash
KRIA_REAL_LLM=1 cargo test -p kria-core --test quality_hallucination_tests
# Quality report is written to: target/quality-report.json
```

### Run live voice tests

```bash
KRIA_VOICE_LIVE=1 cargo test -p kria-core --test voice_live_tests -- --test-threads=1
```

### Run dangerous Tier-3 tests (CAUTION — destructive)

```bash
KRIA_DANGEROUS=1 cargo test -p kria-core --test dangerous_live_tests -- --ignored
```

### Run Playwright tests

```bash
# Install dependencies (one-time)
cd tests/e2e
npm install
npx playwright install --with-deps

# Start the KRIA server first (separate terminal)
cargo run -p kria-server

# Run API-only tests (no browser needed)
npx playwright test --project=api-integration

# Run browser E2E tests (needs UI dev server running too)
cd ../../ui && npm run dev &    # start frontend on :5173
cd ../tests/e2e
npx playwright test --project=e2e-chromium

# Run everything
npx playwright test

# Interactive UI mode
npx playwright test --ui

# View HTML report after a run
npx playwright show-report
```

---

## Run Modes Summary

| Mode | Command | What runs |
|------|---------|-----------|
| **Standard** | `cargo test --workspace` | All non-gated tests; no real LLM, no live voice, no destructive ops |
| **Real LLM** | `KRIA_REAL_LLM=1 cargo test --test quality_hallucination_tests` | Quality/hallucination golden set via local Phi-4-mini |
| **Live Voice** | `KRIA_VOICE_LIVE=1 cargo test --test voice_live_tests -- --test-threads=1` | Wake-word, barge-in, VAD, PTT, emergency stop |
| **Dangerous** | `KRIA_DANGEROUS=1 cargo test --test dangerous_live_tests -- --ignored` | Real shutdown / Gmail send / git push to main |

---

## Architecture

### Page Object Model (POM)

All Playwright tests use page objects in `tests/e2e/pages/`:

| Page Object | File | Purpose |
|-------------|------|---------|
| `KriaApiClient` | `pages/api-client.ts` | HTTP client wrapper for all REST endpoints |
| `ChatPage` | `pages/chat-page.ts` | Chat UI interaction (send message, toggle voice, etc.) |
| `SettingsPage` | `pages/settings-page.ts` | Settings modal (open, modify, close) |

### Idempotency

All tests are fully idempotent:

- **Rust integration tests** spin up an ephemeral Axum server on a random port per test — no shared state.
- **Rust feature tests** use in-memory SQLite (`:memory:`) or `tempfile` — nothing persisted.
- **Phase-B tests** use `SandboxDir` (auto-deleted RAII dir at `target/test-sandbox/<uuid>/`) for all writes.
- **Playwright API tests** POST to placeholder endpoints that don't persist changes.
- **Playwright E2E tests** operate on a fresh page per test.

---

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `KRIA_BASE_URL` | `http://127.0.0.1:8088` | Base URL for Playwright API tests |
| `KRIA_WS_URL` | `ws://127.0.0.1:8088/ws` | WebSocket URL for WS tests |
| `KRIA_UI_URL` | `http://localhost:5173` | Frontend URL for browser E2E tests |
| `KRIA_REAL_LLM` | unset | Set to `1` to enable quality/hallucination tests |
| `KRIA_VOICE_LIVE` | unset | Set to `1` to enable live voice tests |
| `KRIA_DANGEROUS` | unset | Set to `1` to enable Tier-3 destructive tests (use `--ignored`) |

---

## CI Pipeline

The GitHub Actions workflow (`.github/workflows/ci.yml`) runs on every push to `main`/`develop` and on PRs:

1. **rust-check** — `cargo fmt`, `cargo clippy`, `cargo build`
2. **rust-test** — `cargo test --workspace` (depends on rust-check)
3. **e2e-api** — Starts server, runs Playwright API tests (depends on rust-test)
4. **e2e-browser** — Starts server + UI, runs Playwright browser tests (depends on rust-test)

Test reports are uploaded as GitHub Actions artifacts on every run.


| Layer | Framework | Location | What it covers |
|-------|-----------|----------|----------------|
| **Rust unit/feature tests** | `cargo test` | `crates/kria-core/tests/` | Config loading, safety engine, memory store, vectors, embeddings |
| **Rust integration tests** | `cargo test` | `crates/kria-server/tests/` | REST API endpoints, WebSocket protocol, CORS, error handling |
| **Playwright API tests** | Playwright (TS) | `tests/e2e/tests/*.api.spec.ts` | HTTP contract validation, WS integration |
| **Playwright E2E tests** | Playwright (TS) | `tests/e2e/tests/*.e2e.spec.ts` | Full browser user journeys (chat, settings, voice) |

---

## Quick Start

### Run all Rust tests

```bash
cargo test --workspace
```

### Run a specific test file

```bash
# Config tests only
cargo test -p kria-core --test config_tests

# Safety tests only
cargo test -p kria-core --test safety_tests

# Memory tests only
cargo test -p kria-core --test memory_tests

# Server API integration tests
cargo test -p kria-server --test integration_api

# WebSocket integration tests
cargo test -p kria-server --test integration_ws
```

### Run Playwright tests

```bash
# Install dependencies (one-time)
cd tests/e2e
npm install
npx playwright install --with-deps

# Start the KRIA server first (separate terminal)
cargo run -p kria-server

# Run API-only tests (no browser needed)
npx playwright test --project=api-integration

# Run browser E2E tests (needs UI dev server running too)
cd ../../ui && npm run dev &    # start frontend on :5173
cd ../tests/e2e
npx playwright test --project=e2e-chromium

# Run everything
npx playwright test

# Interactive UI mode
npx playwright test --ui

# View HTML report after a run
npx playwright show-report
```

---

## Architecture

### Page Object Model (POM)

All Playwright tests use page objects in `tests/e2e/pages/`:

| Page Object | File | Purpose |
|-------------|------|---------|
| `KriaApiClient` | `pages/api-client.ts` | HTTP client wrapper for all REST endpoints |
| `ChatPage` | `pages/chat-page.ts` | Chat UI interaction (send message, toggle voice, etc.) |
| `SettingsPage` | `pages/settings-page.ts` | Settings modal (open, modify, close) |

### Idempotency

All tests are fully idempotent:

- **Rust integration tests** spin up an ephemeral Axum server on a random port per test — no shared state.
- **Rust feature tests** use in-memory SQLite (`:memory:`) or `tempfile` — nothing persisted.
- **Playwright API tests** POST to placeholder endpoints that don't persist changes.
- **Playwright E2E tests** operate on a fresh page per test.

---

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `KRIA_BASE_URL` | `http://127.0.0.1:8088` | Base URL for Playwright API tests |
| `KRIA_WS_URL` | `ws://127.0.0.1:8088/ws` | WebSocket URL for WS tests |
| `KRIA_UI_URL` | `http://localhost:5173` | Frontend URL for browser E2E tests |

---

## CI Pipeline

The GitHub Actions workflow (`.github/workflows/ci.yml`) runs on every push to `main`/`develop` and on PRs:

1. **rust-check** — `cargo fmt`, `cargo clippy`, `cargo build`
2. **rust-test** — `cargo test --workspace` (depends on rust-check)
3. **e2e-api** — Starts server, runs Playwright API tests (depends on rust-test)
4. **e2e-browser** — Starts server + UI, runs Playwright browser tests (depends on rust-test)

Test reports are uploaded as GitHub Actions artifacts on every run.
