# KRIA — Testing Guide

## Overview

The test suite is organized into four layers that run automatically on every push/PR via GitHub Actions:

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
