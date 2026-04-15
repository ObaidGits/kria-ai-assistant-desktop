/**
 * WebSocket integration tests for the KRIA server.
 *
 * Exercises every WS message type using raw WebSocket connections.
 * Fully idempotent — no persistent state mutated.
 */
import { test, expect } from "@playwright/test";
import { WebSocket } from "ws";

const WS_URL = process.env.KRIA_WS_URL || "ws://127.0.0.1:8088/ws";

/** Open a WS connection and return it once the welcome message arrives. */
function connectWs(): Promise<{ ws: WebSocket; welcome: Record<string, unknown> }> {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(WS_URL);
    const timer = setTimeout(() => {
      ws.close();
      reject(new Error("WS connection timed out"));
    }, 5000);

    ws.once("message", (raw) => {
      clearTimeout(timer);
      const welcome = JSON.parse(raw.toString());
      resolve({ ws, welcome });
    });

    ws.once("error", (err) => {
      clearTimeout(timer);
      reject(err);
    });
  });
}

/** Send a JSON message and wait for the next response. */
function sendAndReceive(
  ws: WebSocket,
  msg: Record<string, unknown>,
): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("WS response timed out")), 5000);

    ws.once("message", (raw) => {
      clearTimeout(timer);
      resolve(JSON.parse(raw.toString()));
    });

    ws.send(JSON.stringify(msg));
  });
}

// ── Connection ────────────────────────────────────────────────────

test.describe("WebSocket connection", () => {
  test("sends welcome message on connect", async () => {
    const { ws, welcome } = await connectWs();
    expect(welcome.type).toBe("connected");
    expect(welcome).toHaveProperty("version");
    ws.close();
  });
});

// ── Chat flow ─────────────────────────────────────────────────────

test.describe("WebSocket chat", () => {
  test("chat message returns ack then done", async () => {
    const { ws } = await connectWs();

    // Send chat
    const ack = await sendAndReceive(ws, { type: "chat", message: "Hi KRIA" });
    expect(ack.type).toBe("ack");
    expect(ack.message).toBe("Hi KRIA");

    // Receive done
    const done: Record<string, unknown> = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("timeout")), 5000);
      ws.once("message", (raw) => {
        clearTimeout(timer);
        resolve(JSON.parse(raw.toString()));
      });
    });
    expect(done.type).toBe("done");
    expect((done.text as string)).toContain("Hi KRIA");

    ws.close();
  });
});

// ── HITL ──────────────────────────────────────────────────────────

test.describe("WebSocket HITL", () => {
  test("approve returns hitl_ack", async () => {
    const { ws } = await connectWs();
    const resp = await sendAndReceive(ws, { type: "approve", request_id: "r1" });
    expect(resp.type).toBe("hitl_ack");
    expect(resp.action).toBe("approve");
    ws.close();
  });

  test("deny returns hitl_ack", async () => {
    const { ws } = await connectWs();
    const resp = await sendAndReceive(ws, { type: "deny", request_id: "r2", reason: "unsafe" });
    expect(resp.type).toBe("hitl_ack");
    expect(resp.action).toBe("deny");
    ws.close();
  });
});

// ── Ping / Pong ───────────────────────────────────────────────────

test.describe("WebSocket ping", () => {
  test("ping returns pong", async () => {
    const { ws } = await connectWs();
    const resp = await sendAndReceive(ws, { type: "ping" });
    expect(resp.type).toBe("pong");
    ws.close();
  });
});

// ── Error handling ────────────────────────────────────────────────

test.describe("WebSocket error handling", () => {
  test("unknown type returns error", async () => {
    const { ws } = await connectWs();
    const resp = await sendAndReceive(ws, { type: "garbage" });
    expect(resp.type).toBe("error");
    expect((resp.message as string)).toContain("unknown message type");
    ws.close();
  });

  test("invalid JSON returns error", async () => {
    const { ws } = await connectWs();

    const resp: Record<string, unknown> = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("timeout")), 5000);
      ws.once("message", (raw) => {
        clearTimeout(timer);
        resolve(JSON.parse(raw.toString()));
      });
      ws.send("not valid json {{{");
    });

    expect(resp.type).toBe("error");
    expect((resp.message as string)).toContain("invalid JSON");
    ws.close();
  });
});
