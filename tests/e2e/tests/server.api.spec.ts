/**
 * API integration tests for the KRIA server.
 *
 * Verifies the contract between the frontend and the Axum backend
 * including happy paths, edge cases, and broken response handling.
 * Runs without a browser — pure HTTP.
 */
import { test, expect } from "@playwright/test";
import { KriaApiClient } from "../pages/api-client";

let api: KriaApiClient;

test.beforeEach(async ({ request }) => {
  api = new KriaApiClient(request);
});

// ── Health ────────────────────────────────────────────────────────

test.describe("GET /api/health", () => {
  test("returns healthy status with version", async () => {
    const res = await api.getHealth();
    expect(res.ok()).toBeTruthy();

    const body = await res.json();
    expect(body.status).toBe("healthy");
    expect(body).toHaveProperty("version");
  });
});

// ── Chat ──────────────────────────────────────────────────────────

test.describe("POST /api/chat", () => {
  test("happy path — accepts valid message", async () => {
    const res = await api.sendChat("Hello KRIA");
    expect(res.ok()).toBeTruthy();

    const body = await res.json();
    expect(body.status).toBe("received");
    expect(body.message).toBe("Hello KRIA");
    expect(body.session_id).toBeTruthy();
  });

  test("preserves explicit session_id", async () => {
    const res = await api.sendChat("test", "session-42");
    const body = await res.json();
    expect(body.session_id).toBe("session-42");
  });

  test("edge case — rejects empty body", async ({ request }) => {
    const res = await request.post("/api/chat", {
      data: {},
      headers: { "Content-Type": "application/json" },
    });
    // Axum rejects missing required fields
    expect(res.status()).toBe(422);
  });

  test("edge case — rejects non-JSON content type", async ({ request }) => {
    const res = await request.post("/api/chat", {
      data: "not json",
      headers: { "Content-Type": "text/plain" },
    });
    expect(res.ok()).toBeFalsy();
  });

  test("broken API — nonexistent endpoint returns 404", async ({ request }) => {
    const res = await request.get("/api/does-not-exist");
    expect(res.status()).toBe(404);
  });
});

// ── Sessions ──────────────────────────────────────────────────────

test.describe("GET /api/sessions", () => {
  test("returns an empty array", async () => {
    const res = await api.listSessions();
    expect(res.ok()).toBeTruthy();

    const body = await res.json();
    expect(Array.isArray(body)).toBeTruthy();
    expect(body).toHaveLength(0);
  });
});

// ── Models ────────────────────────────────────────────────────────

test.describe("GET /api/models", () => {
  test("returns object with models key", async () => {
    const res = await api.listModels();
    expect(res.ok()).toBeTruthy();

    const body = await res.json();
    expect(body).toHaveProperty("models");
    expect(Array.isArray(body.models)).toBeTruthy();
  });
});

// ── Settings ──────────────────────────────────────────────────────

test.describe("Settings API", () => {
  test("GET returns full configuration structure", async () => {
    const res = await api.getSettings();
    expect(res.ok()).toBeTruthy();

    const body = await res.json();
    for (const section of ["llm", "voice", "memory", "safety", "server", "ui"]) {
      expect(body).toHaveProperty(section);
    }
  });

  test("POST returns updated status", async () => {
    const res = await api.updateSettings({ ui: { theme: "light" } });
    expect(res.ok()).toBeTruthy();

    const body = await res.json();
    expect(body.status).toBe("updated");
  });

  test("idempotent — settings are unchanged after test POST", async () => {
    // POST an update
    await api.updateSettings({ ui: { theme: "light" } });

    // GET should still return server defaults (placeholder doesn't persist)
    const res = await api.getSettings();
    const body = await res.json();
    expect(body.ui.theme).toBe("dark"); // default from KriaConfig
  });
});
