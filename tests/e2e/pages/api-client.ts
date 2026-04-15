/**
 * API Client page object for KRIA REST API integration testing.
 *
 * Encapsulates all HTTP interactions so test files remain clean
 * and changes to endpoints only need updating in one place.
 */
import { APIRequestContext } from "@playwright/test";

export class KriaApiClient {
  constructor(private readonly request: APIRequestContext) {}

  // ── Health ──────────────────────────────────────────────────────

  async getHealth() {
    return this.request.get("/api/health");
  }

  // ── Chat ────────────────────────────────────────────────────────

  async sendChat(message: string, sessionId?: string) {
    return this.request.post("/api/chat", {
      data: { message, session_id: sessionId },
    });
  }

  async sendChatRaw(body: unknown) {
    return this.request.post("/api/chat", {
      data: body as Record<string, unknown>,
    });
  }

  // ── Sessions ────────────────────────────────────────────────────

  async listSessions() {
    return this.request.get("/api/sessions");
  }

  // ── Models ──────────────────────────────────────────────────────

  async listModels() {
    return this.request.get("/api/models");
  }

  // ── Settings ────────────────────────────────────────────────────

  async getSettings() {
    return this.request.get("/api/settings");
  }

  async updateSettings(settings: Record<string, unknown>) {
    return this.request.post("/api/settings", { data: settings });
  }
}
