import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@solidjs/testing-library";

const {
  noopAsync,
  setGoogleAccountMock,
  loadGoogleStatusMock,
  reconcileMcpRuntimeMock,
  restartMcpServerRuntimeMock,
} = vi.hoisted(() => {
  const noopAsync = vi.fn(async () => undefined);

  const setGoogleAccountMock = vi.fn(async (account: string) => ({
    account,
    updated: true,
  }));

  const loadGoogleStatusMock = vi.fn(async (account?: string) => ({
    connected: true,
    account: account ?? "personal",
    credentials_configured: true,
    token_present: true,
    auth_ready: true,
    runtime_ready: true,
    gw_client_wired: true,
    mcp: {
      configured_enabled: true,
      state: "running",
      tool_count: 24,
      error: null,
    },
    capabilities: {
      gmail: true,
      drive: true,
      calendar: true,
      docs: true,
      sheets: true,
      slides: true,
      forms: true,
      meet: false,
      meet_via_calendar: true,
    },
    meet_support_mode: "calendar_conference_link",
    warnings: [],
  }));

  const reconcileMcpRuntimeMock = vi.fn(async () => ({ reconciled: true }));
  const restartMcpServerRuntimeMock = vi.fn(async (_name: string) => ({ restarted: true }));

  return {
    noopAsync,
    setGoogleAccountMock,
    loadGoogleStatusMock,
    reconcileMcpRuntimeMock,
    restartMcpServerRuntimeMock,
  };
});

vi.mock("../stores/app", () => ({
  appStore: {
    setShowSettings: vi.fn(),
    settings: () => ({
      llm: {},
      voice: {},
      safety: {},
      ui: { theme: "dark", language: "en" },
      search: {},
      agent: {},
      hardware: {},
    }),
    loadSettings: noopAsync,
    saveSettings: noopAsync,
    models: () => [],
    loadModels: noopAsync,
    audioDevices: () => ({
      inputs: [],
      outputs: [],
      default_input: null,
      default_output: null,
    }),
    loadAudioDevices: noopAsync,
    theme: () => "dark",
    applyTheme: vi.fn(),
    mcpServers: () => [],
    loadMcpServers: noopAsync,
    addMcpServer: noopAsync,
    removeMcpServer: noopAsync,
    toggleMcpServer: noopAsync,
    healthInfo: () => ({
      status: "healthy",
      uptime_secs: 123,
      services: [],
    }),
    loadHealth: noopAsync,
    scheduledTasks: () => [],
    loadScheduledTasks: noopAsync,
    addScheduledTask: noopAsync,
    removeScheduledTask: noopAsync,
    macros: () => [],
    loadMacros: noopAsync,
    deleteMacro: noopAsync,
    workflows: () => [],
    loadWorkflows: noopAsync,
    deleteWorkflow: noopAsync,
    hardwareInfo: () => null,
    loadHardwareInfo: noopAsync,
    knowledgeBase: () => [],
    loadKnowledgeBase: noopAsync,
    telegramConfig: () => ({
      enabled: false,
      bot_token: "",
      allowed_chat_ids: "",
      auto_start: false,
    }),
    telegramBotInfo: () => null,
    loadTelegramConfig: noopAsync,
    saveTelegramConfig: noopAsync,
    testTelegramConnection: noopAsync,
    startTelegramMcp: noopAsync,
    stopTelegramMcp: noopAsync,
    googleStatus: () => ({
      connected: true,
      account: "personal",
      credentials_configured: true,
      token_present: true,
      auth_ready: true,
      runtime_ready: true,
      gw_client_wired: true,
      mcp: {
        configured_enabled: true,
        state: "running",
        tool_count: 24,
        error: null,
      },
      capabilities: {
        gmail: true,
        drive: true,
        calendar: true,
        docs: true,
        sheets: true,
        slides: true,
        forms: true,
        meet: false,
        meet_via_calendar: true,
      },
      meet_support_mode: "calendar_conference_link",
      warnings: [],
    }),
    loadGoogleStatus: loadGoogleStatusMock,
    setGoogleAccount: setGoogleAccountMock,
    connectGoogle: noopAsync,
    disconnectGoogle: noopAsync,
    colabStatus: () => ({
      enabled: false,
      connected: false,
      ready_for_cloud_task: false,
      notebook_selection_required: false,
      runtime_state: "disconnected",
      selected_notebook: null,
      mcp_server_name: "colab-mcp",
      auto_escalate: true,
      fallback_to_local: true,
      connect_timeout_secs: 60,
      keepalive_interval_secs: 120,
      checkpoint_interval_secs: 300,
      mcp: {
        state: "stopped",
        tool_count: 0,
        error: null,
      },
      capabilities: {
        category: "mcp_colab-mcp",
        tool_count: 0,
        discovered_tools: [],
        features: {
          notebook_discovery: false,
          notebook_selection: false,
          cell_execution: false,
          artifact_io: false,
          runtime_lifecycle: false,
          package_management: false,
          checkpointing: false,
        },
        ready_requirements: {
          requires: [],
          satisfied: false,
          missing: [],
        },
      },
      warnings: [],
    }),
    loadColabStatus: noopAsync,
    connectColab: noopAsync,
    disconnectColab: noopAsync,
    setColabNotebook: noopAsync,
    reconcileMcpRuntime: reconcileMcpRuntimeMock,
    restartMcpServerRuntime: restartMcpServerRuntimeMock,
  },
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => undefined),
}));

import SettingsModal from "./SettingsModal";

describe("SettingsModal Google controls", () => {
  beforeEach(() => {
    setGoogleAccountMock.mockClear();
    loadGoogleStatusMock.mockClear();
    reconcileMcpRuntimeMock.mockClear();
    restartMcpServerRuntimeMock.mockClear();
  });

  it("persists account name on blur", async () => {
    render(() => <SettingsModal />);

    await fireEvent.click(screen.getByRole("button", { name: /^Google$/ }));

    const accountInput = await screen.findByPlaceholderText("personal");
    await fireEvent.input(accountInput, {
      currentTarget: { value: "work" },
      target: { value: "work" },
    });
    await fireEvent.blur(accountInput);

    expect(setGoogleAccountMock).toHaveBeenCalledWith("work");
    expect(loadGoogleStatusMock).toHaveBeenLastCalledWith("work");
  });

  it("runs runtime reconcile and restart controls", async () => {
    render(() => <SettingsModal />);

    await fireEvent.click(screen.getByRole("button", { name: /^Google$/ }));

    await fireEvent.click(await screen.findByRole("button", { name: "Reconcile runtime" }));
    await fireEvent.click(await screen.findByRole("button", { name: "Restart runtime" }));

    expect(reconcileMcpRuntimeMock).toHaveBeenCalledTimes(1);
    expect(restartMcpServerRuntimeMock).toHaveBeenCalledWith("gworkspace");
  });
});
