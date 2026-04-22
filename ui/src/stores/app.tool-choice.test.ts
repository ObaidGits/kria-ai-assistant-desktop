import { beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock, listenMock, listenerMap, setSessionHistory } = vi.hoisted(() => {
  const listenerMap = new Map<string, (event: { payload: any }) => void>();
  let sessionHistory: any[] = [];

  const invokeMock = vi.fn(async (command: string, args?: Record<string, unknown>) => {
    switch (command) {
      case "send_message":
        return { status: "ok", message: args?.message };
      case "create_session":
        return { session_id: "mock-created-session" };
      case "list_sessions":
        return [];
      case "switch_session":
        return { session_id: args?.sessionId ?? "mock-session", messages: [] };
      case "get_session_history":
        return sessionHistory;
      case "get_settings":
        return {
          llm: {},
          voice: {},
          safety: {},
          ui: { theme: "dark" },
          server: {},
          memory: {},
        };
      case "list_audio_devices":
        return {
          inputs: [],
          outputs: [],
          default_input: null,
          default_output: null,
        };
      case "get_health":
        return {
          status: "healthy",
          services: [{ name: "model_router", status: "healthy" }],
        };
      case "set_google_workspace_account":
        return { account: args?.account ?? "personal", updated: true };
      case "reconcile_mcp_runtime":
        return { reconciled: true };
      case "restart_mcp_server_runtime":
        return { restarted: true, name: args?.name ?? null };
      default:
        return null;
    }
  });

  const listenMock = vi.fn(async (eventName: string, callback: (event: { payload: any }) => void) => {
    listenerMap.set(eventName, callback);
    return () => listenerMap.delete(eventName);
  });

  vi.stubGlobal("setInterval", vi.fn(() => 1));

  return {
    invokeMock,
    listenMock,
    listenerMap,
    setSessionHistory: (history: any[]) => {
      sessionHistory = history;
    },
  };
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: listenMock,
}));

vi.mock("highlight.js/styles/github-dark.css?url", () => ({ default: "dark.css" }));
vi.mock("highlight.js/styles/github.css?url", () => ({ default: "light.css" }));

import { appStore } from "./app";

function emit(eventName: string, payload: any) {
  const callback = listenerMap.get(eventName);
  if (!callback) {
    throw new Error(`Missing listener for ${eventName}`);
  }
  callback({ payload });
}

async function flushAsync(cycles = 2) {
  for (let i = 0; i < cycles; i += 1) {
    await Promise.resolve();
  }
}

describe("appStore low-confidence tool choice flow", () => {
  beforeEach(() => {
    invokeMock.mockClear();
    setSessionHistory([]);
    appStore.dismissToolChoice();
  });

  it("captures tool-choice event and clears thinking state", () => {
    emit("agent:thinking", { status: "planning" });
    expect(appStore.isThinking()).toBe(true);

    const payload = {
      query: "check my unread emails",
      confidence: 0.46,
      minConfidence: 0.55,
      candidates: [
        {
          name: "gw_gmail_inbox",
          label: "Gmail",
          reason: "Primary match from intent classifier",
          confidence: 0.46,
        },
      ],
    };

    emit("agent:tool_choice_required", payload);

    expect(appStore.toolChoiceRequest()).toEqual(payload);
    expect(appStore.isThinking()).toBe(false);
  });

  it("submits a forced-tool continuation prompt", async () => {
    emit("agent:tool_choice_required", {
      query: "check my unread emails",
      confidence: 0.46,
      minConfidence: 0.55,
      candidates: [
        {
          name: "gw_gmail_inbox",
          label: "Gmail",
          reason: "Primary match from intent classifier",
          confidence: 0.46,
        },
      ],
    });

    appStore.submitToolChoice("gw_gmail_inbox");
    await flushAsync(8);

    expect(invokeMock).toHaveBeenCalledWith("send_message", {
      message: "#tool:gw_gmail_inbox check my unread emails",
    });
    expect(appStore.toolChoiceRequest()).toBeNull();
  });

  it("dismisses pending tool choice without sending", () => {
    emit("agent:tool_choice_required", {
      query: "find files",
      confidence: 0.42,
      minConfidence: 0.55,
      candidates: [],
    });

    appStore.dismissToolChoice();

    expect(appStore.toolChoiceRequest()).toBeNull();
    expect(invokeMock).not.toHaveBeenCalledWith(
      "send_message",
      expect.objectContaining({ message: expect.stringContaining("#tool:") }),
    );
  });
});

describe("appStore Google runtime command wiring", () => {
  beforeEach(() => {
    invokeMock.mockClear();
    setSessionHistory([]);
  });

  it("routes Google account/runtime actions to backend commands", async () => {
    await appStore.setGoogleAccount("work");
    await appStore.reconcileMcpRuntime();
    await appStore.restartMcpServerRuntime("gworkspace");

    expect(invokeMock).toHaveBeenCalledWith("set_google_workspace_account", { account: "work" });
    expect(invokeMock).toHaveBeenCalledWith("reconcile_mcp_runtime");
    expect(invokeMock).toHaveBeenCalledWith("restart_mcp_server_runtime", { name: "gworkspace" });
  });
});

describe("appStore session history hydration", () => {
  beforeEach(() => {
    invokeMock.mockClear();
    setSessionHistory([]);
  });

  it("rehydrates persisted tool turns into assistant toolCalls", async () => {
    setSessionHistory([
      {
        role: "assistant",
        content: "I retrieved your latest unread emails.",
        timestamp: "2026-04-18T10:00:00Z",
      },
      {
        role: "tool",
        content: "Tool 'gw_gmail_inbox' returned 3 Gmail message(s).",
        tool_name: "gw_gmail_inbox",
        tool_result: JSON.stringify({
          name: "gw_gmail_inbox",
          args: { query: "in:inbox is:unread", max_results: 3 },
          success: true,
          result: {
            provider: "google_workspace",
            kind: "gmail",
            data: {
              returned_count: 3,
              messages: [
                { subject: "Invoice", from: "billing@example.com" },
                { subject: "Security alert", from: "security@example.com" },
              ],
            },
          },
          metadata: {
            confidence: 0.8,
            source_count: 3,
            freshness_age_hours: null,
            region_match: null,
          },
        }),
        timestamp: "2026-04-18T10:00:01Z",
      },
    ]);

    await appStore.switchSession("session-1");

    const hydrated = appStore.messages();
    expect(hydrated).toHaveLength(1);
    expect(hydrated[0].role).toBe("assistant");
    expect(hydrated[0].content).toBe("I retrieved your latest unread emails.");
    expect(hydrated[0].toolCalls).toHaveLength(1);
    expect(hydrated[0].toolCalls?.[0]).toMatchObject({
      name: "gw_gmail_inbox",
      status: "done",
      args: { query: "in:inbox is:unread", max_results: 3 },
    });
    expect(hydrated[0].toolCalls?.[0].metadata?.sourceCount).toBe(3);
  });
});

describe("appStore stream scope parity", () => {
  beforeEach(async () => {
    invokeMock.mockClear();
    setSessionHistory([]);

    appStore.setCurrentEnvironment("assistant");
    await flushAsync();
    await appStore.switchSession("assistant-reset");

    appStore.setCurrentEnvironment("prompt_lab");
    await flushAsync();
    await appStore.switchSession("prompt-lab-reset");

    appStore.setCurrentEnvironment("assistant");
    await flushAsync();
  });

  it("keeps agent and prompt_lab token streams isolated", async () => {
    emit("agent:token", { text: "assistant token" });
    expect(appStore.messages()).toHaveLength(1);
    expect(appStore.messages()[0].content).toBe("assistant token");

    appStore.setCurrentEnvironment("prompt_lab");
    await flushAsync();
    expect(appStore.messages()).toHaveLength(0);

    emit("prompt_lab:token", { text: "lab token" });
    expect(appStore.messages()).toHaveLength(1);
    expect(appStore.messages()[0].content).toBe("lab token");

    appStore.setCurrentEnvironment("assistant");
    await flushAsync();
    expect(appStore.messages()).toHaveLength(1);
    expect(appStore.messages()[0].content).toBe("assistant token");
  });

  it("tracks current session independently per environment", async () => {
    appStore.setCurrentEnvironment("assistant");
    await flushAsync();
    await appStore.switchSession("assistant-session-1");
    expect(appStore.currentSession()).toBe("assistant-session-1");

    appStore.setCurrentEnvironment("prompt_lab");
    await flushAsync();
    await appStore.switchSession("prompt-lab-session-1");
    expect(appStore.currentSession()).toBe("prompt-lab-session-1");

    appStore.setCurrentEnvironment("assistant");
    await flushAsync();
    expect(appStore.currentSession()).toBe("assistant-session-1");
  });
});

describe("appStore colab stage visibility", () => {
  beforeEach(() => {
    invokeMock.mockClear();
    setSessionHistory([]);
  });

  it("captures colab fallback stage details into warning state", () => {
    emit("agent:stage", {
      step: "colab_dispatch_fallback_local",
      message: "Colab tier requirements were not satisfied; using local fallback",
      detail: {
        reason: "missing capabilities: cell_execution",
        requested_mode: "colab",
        effective_mode: "local",
        runtime_state: "awaiting_browser_connection",
      },
      ts: "2026-04-20T00:00:00Z",
    });

    expect(appStore.latestAgentStage()?.step).toBe("colab_dispatch_fallback_local");
    expect(appStore.colabDispatchWarning()).toContain("colab -> local");
    expect(appStore.colabDispatchWarning()).toContain("missing capabilities: cell_execution");
  });

  it("clears colab warning when ready stage is emitted", () => {
    emit("agent:stage", {
      step: "colab_dispatch_fallback_local",
      message: "fallback",
      detail: {
        reason: "runtime_state=awaiting_browser_connection",
        requested_mode: "colab",
        effective_mode: "local",
      },
      ts: "2026-04-20T00:00:00Z",
    });

    expect(appStore.colabDispatchWarning()).not.toBeNull();

    emit("agent:stage", {
      step: "colab_dispatch_ready",
      message: "Colab tier requirements are satisfied",
      detail: {
        requested_mode: "colab",
        effective_mode: "colab",
      },
      ts: "2026-04-20T00:00:01Z",
    });

    expect(appStore.colabDispatchWarning()).toBeNull();
  });
});
