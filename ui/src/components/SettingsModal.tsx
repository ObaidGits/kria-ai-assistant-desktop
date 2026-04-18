import { Component, Show, For, createSignal, createEffect, createMemo, onMount, onCleanup } from "solid-js";
import { listen } from "@tauri-apps/api/event";
import { appStore } from "../stores/app";
import { SUPPORTED_LANGUAGES, setLocale } from "../stores/i18n";

type Tab = "llm" | "voice" | "safety" | "ui" | "assistant" | "labs" | "search" | "services" | "telegram" | "automation" | "hardware" | "knowledge" | "google";

interface AssistantFrontendPrefs {
  persona: "operator" | "coach" | "researcher" | "chief_of_staff";
  verbosity: "compact" | "balanced" | "deep";
  proactiveSuggestions: boolean;
  missionBriefings: boolean;
  followupQuestions: boolean;
  smartSessionTitles: boolean;
}

interface LabsFrontendPrefs {
  missionBoard: boolean;
  workflowCanvas: boolean;
  mcpMarketplace: boolean;
  autoPilotQueue: boolean;
  contextMap: boolean;
}

interface McpCatalogItem {
  id: string;
  name: string;
  description: string;
  trust: "GREEN" | "YELLOW" | "RED";
  enabled: boolean;
}

const SettingsModal: Component = () => {
  const { setShowSettings, settings, loadSettings, saveSettings, models, loadModels, audioDevices, loadAudioDevices, theme, applyTheme, mcpServers, loadMcpServers, addMcpServer, removeMcpServer, toggleMcpServer, healthInfo, loadHealth, scheduledTasks, loadScheduledTasks, addScheduledTask, removeScheduledTask, macros, loadMacros, deleteMacro, workflows, loadWorkflows, deleteWorkflow, hardwareInfo, loadHardwareInfo, knowledgeBase, loadKnowledgeBase, telegramConfig, telegramBotInfo, loadTelegramConfig, saveTelegramConfig, testTelegramConnection, startTelegramMcp, stopTelegramMcp, googleStatus, loadGoogleStatus, setGoogleAccount, connectGoogle, disconnectGoogle, reconcileMcpRuntime, restartMcpServerRuntime } = appStore;

  const [activeTab, setActiveTab] = createSignal<Tab>("llm");
  const [draft, setDraft] = createSignal<Record<string, any>>({});
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal("");
  const [success, setSuccess] = createSignal("");

  // MCP add server form
  const [newServerName, setNewServerName] = createSignal("");
  const [newServerCommand, setNewServerCommand] = createSignal("");
  const [newServerArgs, setNewServerArgs] = createSignal("");
  const [newServerTrust, setNewServerTrust] = createSignal("YELLOW");

  // Automation form state
  const [newTaskName, setNewTaskName] = createSignal("");
  const [newTaskInterval, setNewTaskInterval] = createSignal("3600");
  const [newTaskPrompt, setNewTaskPrompt] = createSignal("");

  // Telegram form state
  const [tgBotToken, setTgBotToken] = createSignal("");
  const [tgChatIds, setTgChatIds] = createSignal("");
  const [tgAutoStart, setTgAutoStart] = createSignal(true);
  const [tgTesting, setTgTesting] = createSignal(false);
  const [tgTestResult, setTgTestResult] = createSignal<string | null>(null);
  const [tgSaving, setTgSaving] = createSignal(false);

  // Google Workspace state
  const [gwAccount, setGwAccount] = createSignal("personal");
  const [gwConnecting, setGwConnecting] = createSignal(false);
  const [gwPollTimer, setGwPollTimer] = createSignal<ReturnType<typeof setInterval> | null>(null);
  const [gwMessage, setGwMessage] = createSignal("");

  // Frontend-only assistant/labs preferences
  const [assistantPrefs, setAssistantPrefs] = createSignal<AssistantFrontendPrefs>({
    persona: "chief_of_staff",
    verbosity: "balanced",
    proactiveSuggestions: true,
    missionBriefings: true,
    followupQuestions: true,
    smartSessionTitles: true,
  });
  const [labsPrefs, setLabsPrefs] = createSignal<LabsFrontendPrefs>({
    missionBoard: true,
    workflowCanvas: false,
    mcpMarketplace: true,
    autoPilotQueue: false,
    contextMap: false,
  });
  const [mcpCatalog, setMcpCatalog] = createSignal<McpCatalogItem[]>([
    {
      id: "gmail-ops",
      name: "Gmail Operations",
      description: "Summaries, triage suggestions, and thread drafting tools.",
      trust: "YELLOW",
      enabled: true,
    },
    {
      id: "calendar-orchestrator",
      name: "Calendar Orchestrator",
      description: "Planning blocks, conflict resolution, and follow-up scheduling.",
      trust: "GREEN",
      enabled: false,
    },
    {
      id: "docs-briefing-kit",
      name: "Docs Briefing Kit",
      description: "Extract action items, owners, and deadlines from docs.",
      trust: "YELLOW",
      enabled: false,
    },
    {
      id: "ops-sentinel",
      name: "Ops Sentinel",
      description: "Watchdog connector for system/service event streams.",
      trust: "RED",
      enabled: false,
    },
  ]);

  onMount(() => {
    let disposed = false;
    let unlistenConnected: (() => void) | null = null;
    let unlistenError: (() => void) | null = null;

    const initialize = async () => {
      await loadSettings();
      await loadModels();
      await loadAudioDevices();
      await loadMcpServers();
      await loadHealth();
      await loadScheduledTasks();
      await loadMacros();
      await loadWorkflows();
      await loadHardwareInfo();
      await loadKnowledgeBase();
      await loadTelegramConfig();
      const initialGoogleStatus = await loadGoogleStatus();
      if (disposed) return;
      if (initialGoogleStatus?.account) {
        setGwAccount(initialGoogleStatus.account);
      }

      // Restore frontend-only preferences from local storage.
      try {
        const assistantRaw = localStorage.getItem("kria_assistant_frontend_prefs");
        if (assistantRaw) {
          setAssistantPrefs({ ...assistantPrefs(), ...JSON.parse(assistantRaw) });
        }
        const labsRaw = localStorage.getItem("kria_labs_frontend_prefs");
        if (labsRaw) {
          setLabsPrefs({ ...labsPrefs(), ...JSON.parse(labsRaw) });
        }
        const catalogRaw = localStorage.getItem("kria_mcp_catalog");
        if (catalogRaw) {
          const parsed = JSON.parse(catalogRaw);
          if (Array.isArray(parsed)) {
            setMcpCatalog(parsed as McpCatalogItem[]);
          }
        }
      } catch (e) {
        console.warn("Failed to restore frontend preferences:", e);
      }

      if (disposed) return;

      // Listen for OAuth completion events from Tauri backend.
      unlistenConnected = await listen("gw:connected", async (_event: any) => {
        setGwConnecting(false);
        setGwMessage("");
        const pol = gwPollTimer();
        if (pol) {
          clearInterval(pol);
          setGwPollTimer(null);
        }
        try {
          await reconcileMcpRuntime();
        } catch (e) {
          console.warn("Failed to reconcile MCP runtime after Google connect:", e);
        }
        await loadMcpServers();
        await loadGoogleStatus(gwAccount());
      });

      if (disposed) {
        unlistenConnected?.();
        return;
      }

      unlistenError = await listen("gw:error", (event: any) => {
        setGwConnecting(false);
        const pol = gwPollTimer();
        if (pol) {
          clearInterval(pol);
          setGwPollTimer(null);
        }
        setGwMessage(`Authorization failed: ${event.payload?.message ?? "unknown error"}`);
      });

      if (disposed) {
        unlistenError?.();
      }
    };

    void initialize();

    onCleanup(() => {
      disposed = true;
      unlistenConnected?.();
      unlistenError?.();
      const pol = gwPollTimer();
      if (pol) clearInterval(pol);
    });
  });

  // Sync draft from loaded settings
  createEffect(() => {
    const s = settings();
    if (s) setDraft(JSON.parse(JSON.stringify(s)));
  });

  // Sync telegram form from loaded config
  createEffect(() => {
    const tg = telegramConfig();
    if (tg) {
      setTgBotToken(tg.bot_token);
      setTgChatIds(tg.allowed_chat_ids);
      setTgAutoStart(tg.auto_start);
    }
  });

  createEffect(() => {
    const status = googleStatus();
    if (status?.account && !gwConnecting()) {
      setGwAccount(status.account);
    }
  });

  createEffect(() => {
    localStorage.setItem("kria_assistant_frontend_prefs", JSON.stringify(assistantPrefs()));
  });

  createEffect(() => {
    localStorage.setItem("kria_labs_frontend_prefs", JSON.stringify(labsPrefs()));
  });

  createEffect(() => {
    localStorage.setItem("kria_mcp_catalog", JSON.stringify(mcpCatalog()));
  });

  const updateField = (section: string, field: string, value: any) => {
    setDraft((prev) => ({
      ...prev,
      [section]: { ...prev[section], [field]: value },
    }));
  };

  const handleSave = async () => {
    setSaving(true);
    setError("");
    setSuccess("");
    try {
      await saveSettings(draft());
      setSuccess("Settings saved");
      setTimeout(() => setSuccess(""), 2000);
    } catch (e) {
      setError(`Failed to save: ${e}`);
    } finally {
      setSaving(false);
    }
  };

  const maskKey = (key: string) => {
    if (!key || key.length < 8) return key ? "••••" : "";
    return "••••••••" + key.slice(-4);
  };

  const formatUptime = (secs: number): string => {
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    if (h > 0) return `${h}h ${m}m`;
    if (m > 0) return `${m}m`;
    return `${secs}s`;
  };

  const formatInterval = (secs: number): string => {
    if (secs >= 86400) return `${Math.round(secs / 86400)}d`;
    if (secs >= 3600) return `${Math.round(secs / 3600)}h`;
    if (secs >= 60) return `${Math.round(secs / 60)}m`;
    return `${secs}s`;
  };

  const runtimeDotClass = (state?: string): "running" | "stopped" | "" => {
    const normalized = String(state ?? "").toLowerCase();
    if (normalized === "running") return "running";
    if (normalized === "starting") return "";
    return "stopped";
  };

  const runtimeStateLabel = (state?: string): string => {
    const normalized = String(state ?? "stopped").toLowerCase();
    if (normalized === "starting") return "starting";
    if (normalized === "running") return "running";
    if (normalized === "error") return "error";
    return "stopped";
  };

  const googleStatusMessage = (): string => {
    const status = googleStatus();
    if (!status) {
      return "Checking Google integration status...";
    }
    if (status.connected) {
      return `Connected as ${status.account}`;
    }
    if (status.auth_ready && !status.runtime_ready) {
      return `OAuth is ready, but runtime is unavailable (state=${status.mcp?.state ?? "unknown"}).`;
    }
    if (!status.credentials_configured) {
      return "OAuth credentials are missing.";
    }
    if (!status.token_present) {
      return `OAuth token is missing for account '${status.account}'.`;
    }
    return "Google integration is not ready.";
  };

  const googleCapabilityEntries = () => {
    const capabilities = googleStatus()?.capabilities;
    if (!capabilities) return [];
    return [
      ["Gmail", capabilities.gmail],
      ["Calendar", capabilities.calendar],
      ["Drive", capabilities.drive],
      ["Docs", capabilities.docs],
      ["Sheets", capabilities.sheets],
      ["Slides", capabilities.slides],
      ["Forms", capabilities.forms],
      ["Meet (direct)", capabilities.meet],
      ["Meet via Calendar", capabilities.meet_via_calendar],
    ] as const;
  };

  const setAssistantPref = <K extends keyof AssistantFrontendPrefs>(key: K, value: AssistantFrontendPrefs[K]) => {
    setAssistantPrefs((prev) => ({ ...prev, [key]: value }));
  };

  const setLabsPref = <K extends keyof LabsFrontendPrefs>(key: K, value: LabsFrontendPrefs[K]) => {
    setLabsPrefs((prev) => ({ ...prev, [key]: value }));
  };

  const toggleCatalogItem = (id: string) => {
    setMcpCatalog((prev) =>
      prev.map((item) => (item.id === id ? { ...item, enabled: !item.enabled } : item))
    );
  };

  const tabGroups: {
    title: string;
    tabs: { id: Tab; label: string; icon: string; description: string }[];
  }[] = [
    {
      title: "General",
      tabs: [
        {
          id: "llm",
          label: "Model",
          icon: "M",
          description: "Choose local or cloud model routing, providers, and generation controls.",
        },
        {
          id: "voice",
          label: "Voice",
          icon: "V",
          description: "Configure microphone selection, VAD sensitivity, language, and TTS behavior.",
        },
        {
          id: "safety",
          label: "Safety",
          icon: "S",
          description: "Tune approval thresholds, rollback windows, and tool execution safety limits.",
        },
        {
          id: "search",
          label: "Search",
          icon: "Q",
          description: "Set the default search provider and endpoint for web retrieval.",
        },
      ],
    },
    {
      title: "Personalization",
      tabs: [
        {
          id: "ui",
          label: "Appearance",
          icon: "A",
          description: "Customize visual theme, language, contrast, motion, and text scale.",
        },
        {
          id: "assistant",
          label: "Assistant",
          icon: "H",
          description: "Select persona, response depth, and helper behavior preferences.",
        },
        {
          id: "labs",
          label: "Labs",
          icon: "L",
          description: "Toggle preview interfaces and prototype modules for advanced workflows.",
        },
      ],
    },
    {
      title: "Connected Apps",
      tabs: [
        {
          id: "services",
          label: "MCP Services",
          icon: "P",
          description: "Manage MCP servers, runtime status, trust levels, and command registration.",
        },
        {
          id: "telegram",
          label: "Telegram",
          icon: "T",
          description: "Connect a Telegram bot for mobile chat and remote assistant access.",
        },
        {
          id: "google",
          label: "Google",
          icon: "G",
          description: "Manage Google auth, runtime health, capabilities, and synchronization warnings.",
        },
      ],
    },
    {
      title: "System & Data",
      tabs: [
        {
          id: "automation",
          label: "Automation",
          icon: "U",
          description: "Inspect health, schedule jobs, and manage stored macros and workflow assets.",
        },
        {
          id: "hardware",
          label: "Hardware",
          icon: "R",
          description: "Review detected hardware and recommended runtime tiers and performance values.",
        },
        {
          id: "knowledge",
          label: "Knowledge",
          icon: "K",
          description: "Review indexed documents and retrieval corpus status for knowledge grounding.",
        },
      ],
    },
  ];

  const activeTabInfo = createMemo(() => {
    for (const group of tabGroups) {
      const tab = group.tabs.find((item) => item.id === activeTab());
      if (tab) {
        return { group: group.title, tab };
      }
    }
    return { group: tabGroups[0].title, tab: tabGroups[0].tabs[0] };
  });

  return (
    <div class="modal-overlay" onClick={() => setShowSettings(false)}>
      <div class="modal settings-modal" onClick={(e) => e.stopPropagation()}>
        <div class="modal-header">
          <h2>Settings</h2>
          <button class="close-btn" onClick={() => setShowSettings(false)}>×</button>
        </div>

        <div class="modal-body settings-shell">
          <aside class="settings-sidebar-nav">
            <div class="settings-sidebar-head">
              <h3>Preferences</h3>
              <p>Select a category</p>
            </div>

            <For each={tabGroups}>
              {(group) => (
                <div class="settings-nav-group">
                  <div class="settings-nav-group-title">{group.title}</div>
                  <For each={group.tabs}>
                    {(tab) => (
                      <button
                        class={`settings-nav-item ${activeTab() === tab.id ? "active" : ""}`}
                        onClick={() => setActiveTab(tab.id)}
                        title={tab.description}
                      >
                        <span class="settings-nav-icon" aria-hidden="true">{tab.icon}</span>
                        <span class="settings-nav-label">{tab.label}</span>
                      </button>
                    )}
                  </For>
                </div>
              )}
            </For>
          </aside>

          <section class="settings-content">
            <div class="settings-content-header">
              <span class="settings-content-group">{activeTabInfo().group}</span>
              <h3>{activeTabInfo().tab.label}</h3>
              <p>{activeTabInfo().tab.description}</p>
            </div>

            <div class="settings-content-scroll">
              <Show when={error()}>
                <div class="settings-error">{error()}</div>
              </Show>
              <Show when={success()}>
                <div class="settings-success">{success()}</div>
              </Show>

          {/* LLM Tab */}
          <Show when={activeTab() === "llm"}>
            <section class="settings-section">
              <h3>Routing Mode</h3>
              <div class="settings-field">
                <label>Mode</label>
                <select
                  value={draft()?.llm?.routing_mode ?? "local"}
                  onChange={(e) => updateField("llm", "routing_mode", e.currentTarget.value)}
                >
                  <option value="local">Local Only</option>
                  <option value="cloud">Cloud Only</option>
                  <option value="hybrid">Hybrid (Local + Cloud Fallback)</option>
                </select>
              </div>

              <h3>Local LLM</h3>
              <div class="settings-field">
                <label>Active Model</label>
                <select
                  value={draft()?.llm?.active_model ?? ""}
                  onChange={(e) => updateField("llm", "active_model", e.currentTarget.value)}
                >
                  <option value={draft()?.llm?.active_model}>{draft()?.llm?.active_model}</option>
                  {models().map((m: any) => (
                    <Show when={m.name !== draft()?.llm?.active_model}>
                      <option value={m.name}>{m.display_name || m.name}</option>
                    </Show>
                  ))}
                </select>
              </div>
              <div class="settings-field">
                <label>API URL</label>
                <input
                  type="text"
                  value={draft()?.llm?.local_api_url ?? ""}
                  onInput={(e) => updateField("llm", "local_api_url", e.currentTarget.value)}
                />
              </div>
              <div class="settings-field">
                <label>GPU Layers</label>
                <input
                  type="number"
                  value={draft()?.llm?.gpu_layers ?? -1}
                  onInput={(e) => updateField("llm", "gpu_layers", parseInt(e.currentTarget.value) || 0)}
                />
                <span class="field-hint">-1 = auto, 0 = CPU only</span>
              </div>

              <h3>Cloud Provider</h3>
              <div class="settings-field">
                <label>Provider</label>
                <select
                  value={draft()?.llm?.cloud_provider ?? ""}
                  onChange={(e) => updateField("llm", "cloud_provider", e.currentTarget.value)}
                >
                  <option value="">None</option>
                  <option value="gemini">Google Gemini</option>
                  <option value="openai">OpenAI</option>
                  <option value="anthropic">Anthropic</option>
                </select>
              </div>
              <div class="settings-field">
                <label>API Key</label>
                <input
                  type="password"
                  placeholder={maskKey(settings()?.llm?.cloud_api_key ?? "")}
                  value={draft()?.llm?.cloud_api_key ?? ""}
                  onInput={(e) => updateField("llm", "cloud_api_key", e.currentTarget.value)}
                />
              </div>
              <div class="settings-field">
                <label>Cloud Model ID</label>
                <input
                  type="text"
                  value={draft()?.llm?.cloud_model_id ?? ""}
                  onInput={(e) => updateField("llm", "cloud_model_id", e.currentTarget.value)}
                />
              </div>

              <h3>Generation</h3>
              <div class="settings-row">
                <div class="settings-field">
                  <label>Temperature</label>
                  <input
                    type="range"
                    min="0"
                    max="2"
                    step="0.1"
                    value={draft()?.llm?.temperature ?? 0.6}
                    onInput={(e) => updateField("llm", "temperature", parseFloat(e.currentTarget.value))}
                  />
                  <span class="field-value">{(draft()?.llm?.temperature ?? 0.6).toFixed(1)}</span>
                </div>
                <div class="settings-field">
                  <label>Max Tokens</label>
                  <input
                    type="number"
                    value={draft()?.llm?.max_tokens ?? 2048}
                    onInput={(e) => updateField("llm", "max_tokens", parseInt(e.currentTarget.value) || 2048)}
                  />
                </div>
                <div class="settings-field">
                  <label>Context Window</label>
                  <input
                    type="number"
                    value={draft()?.llm?.context_window ?? 4096}
                    onInput={(e) => updateField("llm", "context_window", parseInt(e.currentTarget.value) || 4096)}
                  />
                </div>
              </div>
            </section>
          </Show>

          {/* Voice Tab */}
          <Show when={activeTab() === "voice"}>
            <section class="settings-section">
              <div class="settings-field">
                <label>
                  <input
                    type="checkbox"
                    checked={draft()?.voice?.enabled ?? false}
                    onChange={(e) => updateField("voice", "enabled", e.currentTarget.checked)}
                  />
                  Enable Voice
                </label>
              </div>
              <div class="settings-field">
                <label>Mode</label>
                <select
                  value={draft()?.voice?.mode ?? "push_to_talk"}
                  onChange={(e) => updateField("voice", "mode", e.currentTarget.value)}
                >
                  <option value="push_to_talk">Push to Talk</option>
                  <option value="continuous">Continuous</option>
                  <option value="wake_word">Wake Word</option>
                </select>
              </div>
              <div class="settings-field">
                <label>Microphone</label>
                <select
                  value={draft()?.voice?.mic_device ?? "auto"}
                  onChange={(e) => {
                    const selected = e.currentTarget.value;
                    const followDefault = selected === "auto";
                    updateField("voice", "mic_device", selected);
                    updateField("voice", "follow_system_default_mic", followDefault);
                  }}
                >
                  <option value="auto">
                    {audioDevices()?.default_input
                      ? `System Default (${audioDevices()?.default_input})`
                      : "System Default"}
                  </option>
                  <For each={audioDevices()?.inputs ?? []}>
                    {(device) => (
                      <Show when={device !== "auto"}>
                        <option value={device}>{device}</option>
                      </Show>
                    )}
                  </For>
                  <Show
                    when={
                      (draft()?.voice?.mic_device ?? "auto") !== "auto" &&
                      !(audioDevices()?.inputs ?? []).includes(draft()?.voice?.mic_device ?? "")
                    }
                  >
                    <option value={draft()?.voice?.mic_device}>
                      {(draft()?.voice?.mic_device ?? "Unknown device") + " (unavailable)"}
                    </option>
                  </Show>
                </select>
                <span class="field-hint">If not selected, KRIA uses the system default microphone.</span>
              </div>
              <div class="settings-field">
                <label>TTS Voice</label>
                <select
                  value={draft()?.voice?.tts_voice ?? "en_US-lessac-high"}
                  onChange={(e) => updateField("voice", "tts_voice", e.currentTarget.value)}
                >
                  <option value="en_US-lessac-high">Lessac (High)</option>
                  <option value="en_US-ryan-high">Ryan (High)</option>
                </select>
              </div>
              <div class="settings-field">
                <label>STT Language</label>
                <select
                  value={draft()?.voice?.language ?? "auto"}
                  onChange={(e) => updateField("voice", "language", e.currentTarget.value)}
                >
                  <option value="auto">Auto Detect</option>
                  <option value="en">English</option>
                  <option value="hi">Hindi</option>
                </select>
              </div>
              <div class="settings-field">
                <label>Noise Suppression</label>
                <select
                  value={draft()?.voice?.noise_suppression_mode ?? "off"}
                  onChange={(e) => updateField("voice", "noise_suppression_mode", e.currentTarget.value)}
                >
                  <option value="off">Off</option>
                  <option value="light">Light</option>
                  <option value="aggressive">Aggressive</option>
                </select>
              </div>
              <div class="settings-field">
                <label>VAD Silence (ms)</label>
                <input
                  type="number"
                  value={draft()?.voice?.vad_silence_ms ?? 1000}
                  onInput={(e) => updateField("voice", "vad_silence_ms", parseInt(e.currentTarget.value) || 1000)}
                />
              </div>
              <div class="settings-field">
                <label>Energy Threshold (normalized)</label>
                <input
                  type="number"
                  min="0.001"
                  max="1"
                  step="0.005"
                  value={draft()?.voice?.energy_threshold ?? 0.02}
                  onInput={(e) => updateField("voice", "energy_threshold", parseFloat(e.currentTarget.value) || 0.02)}
                />
                <span class="field-hint">Typical range is 0.01 to 0.08. Lower values make voice activation more sensitive.</span>
              </div>
              <div class="settings-field">
                <label>Partial Transcript Interval (ms)</label>
                <input
                  type="number"
                  min="200"
                  value={draft()?.voice?.partial_update_ms ?? 2000}
                  onInput={(e) => updateField("voice", "partial_update_ms", parseInt(e.currentTarget.value) || 2000)}
                />
              </div>
              <div class="settings-field">
                <label>Transcript Confidence Threshold</label>
                <input
                  type="range"
                  min="0"
                  max="1"
                  step="0.05"
                  value={draft()?.voice?.confidence_threshold ?? 0.3}
                  onInput={(e) => updateField("voice", "confidence_threshold", parseFloat(e.currentTarget.value))}
                />
                <span class="field-value">{(draft()?.voice?.confidence_threshold ?? 0.3).toFixed(2)}</span>
                <span class="field-hint">Final transcripts below this threshold are ignored.</span>
              </div>
            </section>
          </Show>

          {/* Safety Tab */}
          <Show when={activeTab() === "safety"}>
            <section class="settings-section">
              <div class="settings-field">
                <label>HITL Timeout (seconds)</label>
                <input
                  type="number"
                  value={draft()?.safety?.hitl_timeout_secs ?? 30}
                  onInput={(e) => updateField("safety", "hitl_timeout_secs", parseInt(e.currentTarget.value) || 30)}
                />
              </div>
              <div class="settings-field">
                <label>Rollback Retention (hours)</label>
                <input
                  type="number"
                  value={draft()?.safety?.rollback_retention_hours ?? 72}
                  onInput={(e) => updateField("safety", "rollback_retention_hours", parseInt(e.currentTarget.value) || 72)}
                />
              </div>
              <div class="settings-field">
                <label>Tool Timeout (seconds)</label>
                <input
                  type="number"
                  value={draft()?.safety?.tool_timeout_secs ?? 30}
                  onInput={(e) => updateField("safety", "tool_timeout_secs", parseInt(e.currentTarget.value) || 30)}
                />
              </div>
              <div class="settings-field">
                <label>Max Concurrent Tools</label>
                <input
                  type="number"
                  value={draft()?.safety?.max_concurrent_tools ?? 3}
                  onInput={(e) => updateField("safety", "max_concurrent_tools", parseInt(e.currentTarget.value) || 3)}
                />
              </div>
              <div class="settings-field">
                <label>
                  <input
                    type="checkbox"
                    checked={draft()?.safety?.emergency_mode ?? false}
                    onChange={(e) => updateField("safety", "emergency_mode", e.currentTarget.checked)}
                  />
                  Emergency Mode (disable all tools)
                </label>
              </div>
            </section>
          </Show>

          {/* Appearance Tab */}
          <Show when={activeTab() === "ui"}>
            <section class="settings-section">
              <div class="settings-field">
                <label>Theme</label>
                <div class="theme-toggle">
                  <button
                    class={`theme-btn ${theme() === "dark" ? "active" : ""}`}
                    onClick={() => {
                      applyTheme("dark");
                      updateField("ui", "theme", "dark");
                    }}
                  >
                    🌙 Dark
                  </button>
                  <button
                    class={`theme-btn ${theme() === "light" ? "active" : ""}`}
                    onClick={() => {
                      applyTheme("light");
                      updateField("ui", "theme", "light");
                    }}
                  >
                    ☀️ Light
                  </button>
                </div>
              </div>

              <div class="settings-field">
                <label>Language</label>
                <select
                  value={draft()?.ui?.language ?? "en"}
                  onChange={(e) => {
                    const lang = e.currentTarget.value;
                    updateField("ui", "language", lang);
                    setLocale(lang);
                  }}
                >
                  <For each={SUPPORTED_LANGUAGES}>
                    {(lang) => <option value={lang.code}>{lang.label}</option>}
                  </For>
                </select>
              </div>

              <div class="settings-field">
                <label>
                  <input
                    type="checkbox"
                    checked={draft()?.ui?.high_contrast ?? false}
                    onChange={(e) => {
                      const val = e.currentTarget.checked;
                      updateField("ui", "high_contrast", val);
                      document.documentElement.setAttribute("data-high-contrast", String(val));
                    }}
                  />
                  {" "}High Contrast
                </label>
                <span class="field-hint">Increase contrast for better visibility.</span>
              </div>

              <div class="settings-field">
                <label>
                  <input
                    type="checkbox"
                    checked={draft()?.ui?.reduce_motion ?? false}
                    onChange={(e) => {
                      const val = e.currentTarget.checked;
                      updateField("ui", "reduce_motion", val);
                      document.documentElement.setAttribute("data-reduce-motion", String(val));
                    }}
                  />
                  {" "}Reduce Motion
                </label>
                <span class="field-hint">Minimize animations for motion sensitivity.</span>
              </div>

              <div class="settings-field">
                <label>Font Scale</label>
                <select
                  value={String(draft()?.ui?.font_scale ?? 1.0)}
                  onChange={(e) => {
                    const scale = e.currentTarget.value;
                    updateField("ui", "font_scale", parseFloat(scale));
                    document.documentElement.setAttribute("data-font-scale", scale);
                  }}
                >
                  <option value="0.8">Small (80%)</option>
                  <option value="0.9">Compact (90%)</option>
                  <option value="1.0">Normal (100%)</option>
                  <option value="1.2">Large (120%)</option>
                  <option value="1.5">Extra Large (150%)</option>
                  <option value="2.0">Huge (200%)</option>
                </select>
              </div>
            </section>
          </Show>

          {/* Search Tab */}
          <Show when={activeTab() === "assistant"}>
            <section class="settings-section">
              <h3>Assistant Persona</h3>
              <div class="settings-field">
                <label>Default Persona</label>
                <select
                  value={assistantPrefs().persona}
                  onChange={(e) => setAssistantPref("persona", e.currentTarget.value as AssistantFrontendPrefs["persona"])}
                >
                  <option value="chief_of_staff">Chief of Staff</option>
                  <option value="operator">Operator</option>
                  <option value="coach">Coach</option>
                  <option value="researcher">Researcher</option>
                </select>
                <span class="field-hint">Frontend-only preference. Affects assistant framing and UX labels.</span>
              </div>

              <div class="settings-field">
                <label>Response Detail</label>
                <select
                  value={assistantPrefs().verbosity}
                  onChange={(e) => setAssistantPref("verbosity", e.currentTarget.value as AssistantFrontendPrefs["verbosity"])}
                >
                  <option value="compact">Compact</option>
                  <option value="balanced">Balanced</option>
                  <option value="deep">Deep-dive</option>
                </select>
              </div>

              <h3>Interaction Style</h3>
              <div class="settings-field">
                <label>
                  <input
                    type="checkbox"
                    checked={assistantPrefs().proactiveSuggestions}
                    onChange={(e) => setAssistantPref("proactiveSuggestions", e.currentTarget.checked)}
                  />
                  {" "}Proactive suggestions panel
                </label>
              </div>
              <div class="settings-field">
                <label>
                  <input
                    type="checkbox"
                    checked={assistantPrefs().missionBriefings}
                    onChange={(e) => setAssistantPref("missionBriefings", e.currentTarget.checked)}
                  />
                  {" "}Mission briefings in new chats
                </label>
              </div>
              <div class="settings-field">
                <label>
                  <input
                    type="checkbox"
                    checked={assistantPrefs().followupQuestions}
                    onChange={(e) => setAssistantPref("followupQuestions", e.currentTarget.checked)}
                  />
                  {" "}Auto follow-up prompts
                </label>
              </div>
              <div class="settings-field">
                <label>
                  <input
                    type="checkbox"
                    checked={assistantPrefs().smartSessionTitles}
                    onChange={(e) => setAssistantPref("smartSessionTitles", e.currentTarget.checked)}
                  />
                  {" "}Smart session title suggestions
                </label>
              </div>

              <div class="tg-howto">
                <p>
                  Persona preview: <strong>{assistantPrefs().persona.replaceAll("_", " ")}</strong> ·
                  Detail level: <strong>{assistantPrefs().verbosity}</strong>
                </p>
              </div>
            </section>
          </Show>

          <Show when={activeTab() === "labs"}>
            <section class="settings-section">
              <h3>Scalable Frontend Modules</h3>
              <p class="field-hint">These controls are UI-only and designed for future backend wiring.</p>

              <div class="settings-field">
                <label>
                  <input
                    type="checkbox"
                    checked={labsPrefs().missionBoard}
                    onChange={(e) => setLabsPref("missionBoard", e.currentTarget.checked)}
                  />
                  {" "}Mission board workspace
                </label>
              </div>
              <div class="settings-field">
                <label>
                  <input
                    type="checkbox"
                    checked={labsPrefs().workflowCanvas}
                    onChange={(e) => setLabsPref("workflowCanvas", e.currentTarget.checked)}
                  />
                  {" "}Workflow canvas
                </label>
              </div>
              <div class="settings-field">
                <label>
                  <input
                    type="checkbox"
                    checked={labsPrefs().mcpMarketplace}
                    onChange={(e) => setLabsPref("mcpMarketplace", e.currentTarget.checked)}
                  />
                  {" "}MCP marketplace drawer
                </label>
              </div>
              <div class="settings-field">
                <label>
                  <input
                    type="checkbox"
                    checked={labsPrefs().autoPilotQueue}
                    onChange={(e) => setLabsPref("autoPilotQueue", e.currentTarget.checked)}
                  />
                  {" "}Autopilot queue monitor
                </label>
              </div>
              <div class="settings-field">
                <label>
                  <input
                    type="checkbox"
                    checked={labsPrefs().contextMap}
                    onChange={(e) => setLabsPref("contextMap", e.currentTarget.checked)}
                  />
                  {" "}Context map overlay
                </label>
              </div>

              <h3>MCP Skill Catalog (UI Prototype)</h3>
              <div class="mcp-server-list">
                <For each={mcpCatalog()}>
                  {(item) => (
                    <div class="mcp-server-card">
                      <div class="mcp-server-info">
                        <div class="mcp-server-name">
                          <span class={`mcp-status-dot ${item.enabled ? "running" : "stopped"}`}></span>
                          {item.name}
                        </div>
                        <div class="mcp-server-cmd">{item.description}</div>
                        <div class="mcp-server-trust">Trust profile: {item.trust}</div>
                      </div>
                      <div class="mcp-server-actions">
                        <button
                          class={`btn-small ${item.enabled ? "btn-warning" : "btn-success"}`}
                          onClick={() => toggleCatalogItem(item.id)}
                        >
                          {item.enabled ? "Disable" : "Enable"}
                        </button>
                      </div>
                    </div>
                  )}
                </For>
              </div>
            </section>
          </Show>

          {/* Search Tab */}
          <Show when={activeTab() === "search"}>
            <section class="settings-section">
              <div class="settings-field">
                <label>Search Engine</label>
                <select
                  value={draft()?.search?.engine ?? "duckduckgo"}
                  onChange={(e) => updateField("search", "engine", e.currentTarget.value)}
                >
                  <option value="duckduckgo">DuckDuckGo</option>
                  <option value="searxng">SearXNG</option>
                </select>
              </div>
              <Show when={draft()?.search?.engine === "searxng"}>
                <div class="settings-field">
                  <label>SearXNG URL</label>
                  <input
                    type="text"
                    value={draft()?.search?.searxng_url ?? ""}
                    onInput={(e) => updateField("search", "searxng_url", e.currentTarget.value)}
                    placeholder="http://localhost:8888"
                  />
                </div>
              </Show>
            </section>
          </Show>

          {/* Services (MCP) Tab */}
          <Show when={activeTab() === "services"}>
            <section class="settings-section">
              <h3>MCP Servers</h3>
              <p class="field-hint">
                Model Context Protocol servers provide external tools to the AI agent.
              </p>

              <div class="mcp-server-list">
                <For each={mcpServers()} fallback={
                  <div class="mcp-empty">No MCP servers configured.</div>
                }>
                  {(server) => (
                    <div class="mcp-server-card">
                      <div class="mcp-server-info">
                        <div class="mcp-server-name">
                          <span class={`mcp-status-dot ${runtimeDotClass(server.runtime_state)}`}></span>
                          {server.name}
                        </div>
                        <div class="mcp-server-cmd">{server.command} {server.args.join(" ")}</div>
                        <div class="mcp-server-trust">Trust: {server.trust_level}</div>
                        <div class="mcp-server-trust">
                          Runtime: {runtimeStateLabel(server.runtime_state)}
                          {typeof server.runtime_tool_count === "number" ? ` (${server.runtime_tool_count} tools)` : ""}
                        </div>
                        <Show when={server.runtime_error}>
                          <div class="mcp-server-trust" style="color:#ef4444">Error: {server.runtime_error}</div>
                        </Show>
                      </div>
                      <div class="mcp-server-actions">
                        <button
                          class={`btn-small ${server.enabled ? "btn-warning" : "btn-success"}`}
                          onClick={async () => {
                            try {
                              await toggleMcpServer(server.name, !server.enabled);
                            } catch (e) {
                              setError(`${e}`);
                            }
                          }}
                        >
                          {server.enabled ? "Disable" : "Enable"}
                        </button>
                        <button
                          class="btn-small btn-danger"
                          onClick={async () => {
                            try {
                              await removeMcpServer(server.name);
                            } catch (e) {
                              setError(`${e}`);
                            }
                          }}
                        >
                          Remove
                        </button>
                      </div>
                    </div>
                  )}
                </For>
              </div>

              <h3>Add Server</h3>
              <div class="settings-field">
                <label>Name</label>
                <input
                  type="text"
                  value={newServerName()}
                  onInput={(e) => setNewServerName(e.currentTarget.value)}
                  placeholder="my-server"
                />
              </div>
              <div class="settings-field">
                <label>Command</label>
                <input
                  type="text"
                  value={newServerCommand()}
                  onInput={(e) => setNewServerCommand(e.currentTarget.value)}
                  placeholder="npx -y @modelcontextprotocol/server-filesystem"
                />
              </div>
              <div class="settings-field">
                <label>Arguments (space-separated)</label>
                <input
                  type="text"
                  value={newServerArgs()}
                  onInput={(e) => setNewServerArgs(e.currentTarget.value)}
                  placeholder="/home /tmp"
                />
              </div>
              <div class="settings-field">
                <label>Trust Level</label>
                <select
                  value={newServerTrust()}
                  onChange={(e) => setNewServerTrust(e.currentTarget.value)}
                >
                  <option value="GREEN">GREEN (auto-approve)</option>
                  <option value="YELLOW">YELLOW (ask first)</option>
                  <option value="RED">RED (strict approval)</option>
                </select>
              </div>
              <button
                class="btn-primary"
                disabled={!newServerName().trim() || !newServerCommand().trim()}
                onClick={async () => {
                  try {
                    const args = newServerArgs().trim() ? newServerArgs().trim().split(/\s+/) : [];
                    await addMcpServer(newServerName().trim(), newServerCommand().trim(), args, newServerTrust());
                    setNewServerName("");
                    setNewServerCommand("");
                    setNewServerArgs("");
                    setNewServerTrust("YELLOW");
                    setSuccess("MCP server added");
                    setTimeout(() => setSuccess(""), 2000);
                  } catch (e) {
                    setError(`Failed to add server: ${e}`);
                  }
                }}
              >
                Add Server
              </button>
            </section>
          </Show>

          {/* Telegram Tab */}
          <Show when={activeTab() === "telegram"}>
            <section class="settings-section">
              <h3>Telegram Bot Integration</h3>
              <p class="field-hint">
                Connect a Telegram bot to chat with your AI assistant from your phone.
                Create a bot via <a href="https://t.me/BotFather" target="_blank" rel="noopener">@BotFather</a> on Telegram to get a token.
              </p>

              <Show when={telegramConfig()?.enabled}>
                <div class="tg-status-banner tg-connected">
                  <span class="mcp-status-dot running"></span>
                  <span>Telegram integration is <strong>enabled</strong></span>
                  <Show when={telegramBotInfo()}>
                    <span> — @{telegramBotInfo()!.bot_username}</span>
                  </Show>
                </div>
              </Show>

              <div class="settings-field">
                <label>Bot Token</label>
                <input
                  type="password"
                  value={tgBotToken()}
                  onInput={(e) => setTgBotToken(e.currentTarget.value)}
                  placeholder="123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11"
                />
                <span class="field-hint">Get this from @BotFather on Telegram</span>
              </div>

              <div class="settings-field">
                <label>Allowed Chat IDs</label>
                <input
                  type="text"
                  value={tgChatIds()}
                  onInput={(e) => setTgChatIds(e.currentTarget.value)}
                  placeholder="123456789, 987654321"
                />
                <span class="field-hint">Comma-separated. Empty = allow all (less secure). Send /start to your bot and check logs for your chat ID.</span>
              </div>

              <div class="settings-field">
                <label>
                  <input
                    type="checkbox"
                    checked={tgAutoStart()}
                    onChange={(e) => setTgAutoStart(e.currentTarget.checked)}
                  />
                  {" "}Auto-start on launch
                </label>
                <span class="field-hint">Automatically register and connect the Telegram MCP server when KRIA starts.</span>
              </div>

              <div class="tg-actions">
                <button
                  class="btn-secondary"
                  disabled={!tgBotToken().trim() || tgTesting()}
                  onClick={async () => {
                    setTgTesting(true);
                    setTgTestResult(null);
                    try {
                      const info = await testTelegramConnection(tgBotToken().trim());
                      setTgTestResult(`Connected to @${info.bot_username} (${info.bot_name})`);
                    } catch (e) {
                      setTgTestResult(`Failed: ${e}`);
                    } finally {
                      setTgTesting(false);
                    }
                  }}
                >
                  {tgTesting() ? "Testing..." : "Test Connection"}
                </button>

                <button
                  class="btn-primary"
                  disabled={!tgBotToken().trim() || tgSaving()}
                  onClick={async () => {
                    setTgSaving(true);
                    setError("");
                    try {
                      await saveTelegramConfig({
                        enabled: true,
                        bot_token: tgBotToken().trim(),
                        allowed_chat_ids: tgChatIds().trim(),
                        auto_start: tgAutoStart(),
                      });
                      await startTelegramMcp();
                      setSuccess("Telegram connected! MCP server registered.");
                      setTimeout(() => setSuccess(""), 3000);
                    } catch (e) {
                      setError(`Failed: ${e}`);
                    } finally {
                      setTgSaving(false);
                    }
                  }}
                >
                  {tgSaving() ? "Saving..." : (telegramConfig()?.enabled ? "Update & Reconnect" : "Enable Telegram")}
                </button>

                <Show when={telegramConfig()?.enabled}>
                  <button
                    class="btn-danger"
                    onClick={async () => {
                      try {
                        await stopTelegramMcp();
                        setTgTestResult(null);
                        setSuccess("Telegram disconnected.");
                        setTimeout(() => setSuccess(""), 2000);
                      } catch (e) {
                        setError(`Failed: ${e}`);
                      }
                    }}
                  >
                    Disconnect
                  </button>
                </Show>
              </div>

              <Show when={tgTestResult()}>
                <div class={`tg-test-result ${tgTestResult()!.startsWith("Failed") ? "tg-error" : "tg-success"}`}>
                  {tgTestResult()}
                </div>
              </Show>

              <h3>How it works</h3>
              <div class="tg-howto">
                <ol>
                  <li>Open Telegram and search for <strong>@BotFather</strong></li>
                  <li>Send <code>/newbot</code> and follow the prompts to create a bot</li>
                  <li>Copy the bot token and paste it above</li>
                  <li>Click "Enable Telegram" — this registers a Telegram MCP server</li>
                  <li>Send a message to your bot from your phone — KRIA will respond!</li>
                </ol>
              </div>
            </section>
          </Show>

          {/* Automation Tab */}
          <Show when={activeTab() === "automation"}>
            <section class="settings-section">
              {/* Health Status */}
              <h3>System Health</h3>
              <Show when={healthInfo()} fallback={<p class="field-hint">Loading health info...</p>}>
                <div class="health-summary">
                  <span class={`health-badge ${healthInfo()!.status}`}>
                    {healthInfo()!.status}
                  </span>
                  <span class="field-hint">Uptime: {formatUptime(healthInfo()!.uptime_secs)} · {healthInfo()!.tool_count} tools</span>
                </div>
                <div class="health-services">
                  <For each={healthInfo()!.services ?? []}>
                    {(svc: any) => (
                      <div class="health-service-row">
                        <span class={`mcp-status-dot ${svc.status === "healthy" ? "running" : "stopped"}`}></span>
                        <span class="health-svc-name">{svc.name}</span>
                        <span class="health-svc-status">{svc.status}</span>
                        <Show when={svc.message}>
                          <span class="field-hint">({svc.message})</span>
                        </Show>
                      </div>
                    )}
                  </For>
                </div>
              </Show>

              {/* Scheduled Tasks */}
              <h3>Scheduled Tasks</h3>
              <div class="mcp-server-list">
                <For each={scheduledTasks()} fallback={
                  <div class="mcp-empty">No scheduled tasks.</div>
                }>
                  {(task) => (
                    <div class="mcp-server-card">
                      <div class="mcp-server-info">
                        <div class="mcp-server-name">{task.name}</div>
                        <div class="mcp-server-cmd">{task.prompt}</div>
                        <div class="mcp-server-trust">Every {formatInterval(task.interval_secs)}</div>
                      </div>
                      <div class="mcp-server-actions">
                        <button
                          class="btn-small btn-danger"
                          onClick={async () => {
                            try { await removeScheduledTask(task.id); }
                            catch (e) { setError(`${e}`); }
                          }}
                        >
                          Remove
                        </button>
                      </div>
                    </div>
                  )}
                </For>
              </div>

              <h3>Add Task</h3>
              <div class="settings-field">
                <label>Name</label>
                <input
                  type="text"
                  value={newTaskName()}
                  onInput={(e) => setNewTaskName(e.currentTarget.value)}
                  placeholder="Daily summary"
                />
              </div>
              <div class="settings-field">
                <label>Interval (seconds)</label>
                <input
                  type="number"
                  value={newTaskInterval()}
                  onInput={(e) => setNewTaskInterval(e.currentTarget.value)}
                  min="60"
                />
                <span class="field-hint">{formatInterval(parseInt(newTaskInterval()) || 0)}</span>
              </div>
              <div class="settings-field">
                <label>Agent Prompt</label>
                <input
                  type="text"
                  value={newTaskPrompt()}
                  onInput={(e) => setNewTaskPrompt(e.currentTarget.value)}
                  placeholder="Check my email and summarize"
                />
              </div>
              <button
                class="btn-primary"
                disabled={!newTaskName().trim() || !newTaskPrompt().trim()}
                onClick={async () => {
                  try {
                    await addScheduledTask(
                      newTaskName().trim(),
                      parseInt(newTaskInterval()) || 3600,
                      newTaskPrompt().trim()
                    );
                    setNewTaskName("");
                    setNewTaskInterval("3600");
                    setNewTaskPrompt("");
                    setSuccess("Task added");
                    setTimeout(() => setSuccess(""), 2000);
                  } catch (e) {
                    setError(`Failed to add task: ${e}`);
                  }
                }}
              >
                Add Task
              </button>

              {/* Recorded Macros */}
              <h3>Recorded Macros</h3>
              <div class="mcp-server-list">
                <For each={macros()} fallback={
                  <div class="mcp-empty">No recorded macros. Use the agent to record actions.</div>
                }>
                  {(macro_) => (
                    <div class="mcp-server-card">
                      <div class="mcp-server-info">
                        <div class="mcp-server-name">{macro_.name}</div>
                        <div class="mcp-server-cmd">{macro_.description}</div>
                        <div class="mcp-server-trust">{macro_.step_count} steps</div>
                      </div>
                      <div class="mcp-server-actions">
                        <button
                          class="btn-small btn-danger"
                          onClick={async () => {
                            try { await deleteMacro(macro_.name); }
                            catch (e) { setError(`${e}`); }
                          }}
                        >
                          Delete
                        </button>
                      </div>
                    </div>
                  )}
                </For>
              </div>

              {/* Workflows */}
              <h3>Workflows</h3>
              <div class="mcp-server-list">
                <For each={workflows()} fallback={
                  <div class="mcp-empty">No workflows configured.</div>
                }>
                  {(wf) => (
                    <div class="mcp-server-card">
                      <div class="mcp-server-info">
                        <div class="mcp-server-name">{wf.name}</div>
                        <div class="mcp-server-cmd">{wf.description}</div>
                        <div class="mcp-server-trust">{wf.step_count} steps</div>
                      </div>
                      <div class="mcp-server-actions">
                        <button
                          class="btn-small btn-danger"
                          onClick={async () => {
                            try { await deleteWorkflow(wf.id); }
                            catch (e) { setError(`${e}`); }
                          }}
                        >
                          Delete
                        </button>
                      </div>
                    </div>
                  )}
                </For>
              </div>
            </section>
          </Show>

          {/* Hardware Tab */}
          <Show when={activeTab() === "hardware"}>
            <section class="settings-section">
              <h3>Detected Hardware</h3>
              <Show when={hardwareInfo()} fallback={<p>Loading hardware information...</p>}>
                {(hw) => (
                  <>
                    <div class="hw-tier-banner" data-tier={hw().tier}>
                      <span class="hw-tier-label">{hw().tier.toUpperCase()}</span>
                      <span class="hw-tier-host">{hw().hostname} — {hw().os}</span>
                    </div>

                    <div class="hw-grid">
                      <div class="hw-stat">
                        <div class="hw-stat-label">CPU Cores</div>
                        <div class="hw-stat-value">{hw().cpu_cores}</div>
                      </div>
                      <div class="hw-stat">
                        <div class="hw-stat-label">Total RAM</div>
                        <div class="hw-stat-value">{(hw().total_ram_mb / 1024).toFixed(1)} GB</div>
                      </div>
                      <div class="hw-stat">
                        <div class="hw-stat-label">GPU</div>
                        <div class="hw-stat-value">{hw().gpu_name || "None detected"}</div>
                      </div>
                      <div class="hw-stat">
                        <div class="hw-stat-label">VRAM</div>
                        <div class="hw-stat-value">{hw().vram_mb ? `${(hw().vram_mb! / 1024).toFixed(1)} GB` : "N/A"}</div>
                      </div>
                      <div class="hw-stat">
                        <div class="hw-stat-label">Vision</div>
                        <div class="hw-stat-value">{hw().vision_capable ? "Enabled" : "Disabled"}</div>
                      </div>
                      <div class="hw-stat">
                        <div class="hw-stat-label">Context Window</div>
                        <div class="hw-stat-value">{hw().context_window} tokens</div>
                      </div>
                    </div>

                    <h3>Tier Recommendations</h3>
                    <div class="hw-grid">
                      <div class="hw-stat">
                        <div class="hw-stat-label">Recommended LLM</div>
                        <div class="hw-stat-value">{hw().recommended_model}</div>
                      </div>
                      <div class="hw-stat">
                        <div class="hw-stat-label">Recommended STT</div>
                        <div class="hw-stat-value">{hw().recommended_stt}</div>
                      </div>
                      <div class="hw-stat">
                        <div class="hw-stat-label">GPU Layers</div>
                        <div class="hw-stat-value">{hw().gpu_layers === 0 ? "CPU only" : `${hw().gpu_layers} (all)`}</div>
                      </div>
                      <div class="hw-stat">
                        <div class="hw-stat-label">Inference Threads</div>
                        <div class="hw-stat-value">{hw().threads}</div>
                      </div>
                    </div>

                    <h3>Override Tier</h3>
                    <div class="settings-field">
                      <label>Manual Tier (empty = auto-detect)</label>
                      <select
                        value={draft()?.hardware?.tier || ""}
                        onChange={(e) => updateField("hardware", "tier", e.currentTarget.value)}
                      >
                        <option value="">Auto-detect</option>
                        <option value="lite">Lite</option>
                        <option value="standard">Standard</option>
                        <option value="performance">Performance</option>
                        <option value="high">High</option>
                      </select>
                    </div>
                  </>
                )}
              </Show>
            </section>
          </Show>

          {/* Google Workspace Tab */}
          <Show when={activeTab() === "google"}>
            <section class="settings-section">
              <h3>Google Workspace</h3>
              <p class="field-hint">
                Connect your Google account to let KRIA read Gmail, Calendar, Drive, Docs, Sheets, and Slides.
                Uses OAuth 2.0 — KRIA never sees your password.
              </p>

              {/* Connection status banner and details */}
              <Show when={googleStatus()}>
                {(status) => (
                  <>
                    <div
                      class={`tg-status-banner ${status().connected ? "tg-connected" : ""}`}
                      style={status().connected
                        ? ""
                        : "background:var(--surface-2,#2a2a2a);border-left:3px solid var(--text-muted,#888)"}
                    >
                      <span
                        class={`mcp-status-dot ${status().connected ? "running" : runtimeDotClass(status().mcp?.state)}`}
                      ></span>
                      <span>{googleStatusMessage()}</span>
                    </div>

                    <div class="settings-field" style="margin-top:0.5rem">
                      <label>Runtime signals</label>
                      <div style="display:flex;flex-wrap:wrap;gap:0.45rem;margin-top:0.35rem">
                        <span class="mcp-server-trust">Auth: {status().auth_ready ? "ready" : "not ready"}</span>
                        <span class="mcp-server-trust">Runtime: {status().runtime_ready ? "ready" : "not ready"}</span>
                        <span class="mcp-server-trust">MCP state: {runtimeStateLabel(status().mcp?.state)}</span>
                        <span class="mcp-server-trust">Tools: {status().mcp?.tool_count ?? 0}</span>
                        <span class="mcp-server-trust">Bridge: {status().gw_client_wired ? "wired" : "not wired"}</span>
                      </div>
                    </div>

                    <div class="settings-field" style="margin-top:0.75rem">
                      <label>Capabilities</label>
                      <div style="display:flex;flex-wrap:wrap;gap:0.45rem;margin-top:0.35rem">
                        <For each={googleCapabilityEntries()}>
                          {(entry) => (
                            <span class="mcp-server-trust" style={entry[1] ? "" : "opacity:0.7"}>
                              {entry[0]}: {entry[1] ? "yes" : "no"}
                            </span>
                          )}
                        </For>
                      </div>
                      <span class="field-hint">
                        Meet support mode: <code>{status().meet_support_mode}</code> (calendar conference-link fallback)
                      </span>
                    </div>

                    <Show when={status().mcp?.error}>
                      <div class="settings-error" style="margin-top:0.6rem">
                        <strong>MCP runtime error:</strong> {status().mcp?.error}
                      </div>
                    </Show>

                    <Show when={(status().warnings?.length ?? 0) > 0}>
                      <div class="settings-error" style="margin-top:0.6rem">
                        <strong>Warnings</strong>
                        <ul style="margin:0.4rem 0 0 1.2rem;padding:0">
                          <For each={status().warnings}>
                            {(warning) => <li>{warning}</li>}
                          </For>
                        </ul>
                      </div>
                    </Show>
                  </>
                )}
              </Show>

              {/* Missing credentials warning */}
              <Show when={googleStatus() && !googleStatus()!.credentials_configured}>
                <div class="settings-error" style="margin-top:0.75rem">
                  <strong>credentials.json missing.</strong> Create{" "}
                  <code>~/.google-mcp/credentials.json</code> with your Google Cloud OAuth
                  client credentials (installed app type) before connecting.
                </div>
              </Show>

              {/* Account name */}
              <div class="settings-field" style="margin-top:1rem">
                <label>Account name</label>
                <input
                  type="text"
                  value={gwAccount()}
                  onInput={(e) => setGwAccount(e.currentTarget.value)}
                  onBlur={async () => {
                    const normalized = gwAccount().trim();
                    if (!normalized) return;
                    try {
                      await setGoogleAccount(normalized);
                      await loadGoogleStatus(normalized);
                    } catch (e) {
                      setGwMessage(`Failed to persist account: ${e}`);
                    }
                  }}
                  placeholder="personal"
                  disabled={gwConnecting()}
                  style="max-width:220px"
                />
                <span class="field-hint">
                  Name you'll use to identify this Google account (e.g. "personal", "work").
                  This is now persisted as KRIA's single active Google account.
                </span>
              </div>

              {/* Connecting spinner + message */}
              <Show when={gwConnecting()}>
                <div class="tg-status-banner" style="background:var(--surface-2,#2a2a2a);border-left:3px solid #4a9eff;margin-top:0.5rem">
                  <span class="mcp-status-dot" style="background:#4a9eff;animation:pulse 1s infinite"></span>
                  <span>Waiting for authorization in browser...</span>
                </div>
                <p class="field-hint" style="margin-top:0.4rem">
                  A browser tab has opened. Sign in with Google and click <strong>Allow</strong>.
                  This window will update automatically when done.
                </p>
              </Show>

              {/* Feedback message */}
              <Show when={gwMessage()}>
                <div class={`tg-test-result ${gwMessage().startsWith("Authorization failed") ? "tg-error" : "tg-success"}`} style="margin-top:0.5rem">
                  {gwMessage()}
                </div>
              </Show>

              {/* Action buttons */}
              <div class="tg-actions" style="margin-top:1rem">
                <Show when={!googleStatus()?.auth_ready}>
                  <button
                    class="btn-primary"
                    disabled={gwConnecting() || !googleStatus()?.credentials_configured}
                    onClick={async () => {
                      const normalized = gwAccount().trim() || "personal";
                      const existing = gwPollTimer();
                      if (existing) {
                        clearInterval(existing);
                        setGwPollTimer(null);
                      }

                      setGwConnecting(true);
                      setGwMessage("");
                      try {
                        await setGoogleAccount(normalized);
                        await connectGoogle(normalized);
                        let attempts = 0;
                        const maxAttempts = 20;

                        // Poll every 3s while OAuth browser flow is in progress.
                        const timer = setInterval(async () => {
                          attempts += 1;
                          const status = await loadGoogleStatus(normalized);

                          if (status?.connected) {
                            clearInterval(timer);
                            setGwPollTimer(null);
                            setGwConnecting(false);
                            setGwMessage("Connected! Google Workspace tools are now active.");
                            await loadMcpServers();
                            setTimeout(() => setGwMessage(""), 4000);
                            return;
                          }

                          if (status?.auth_ready && !status.runtime_ready && status.mcp?.state !== "starting") {
                            clearInterval(timer);
                            setGwPollTimer(null);
                            setGwConnecting(false);
                            setGwMessage(
                              status.warnings?.[0] || "Authorization succeeded, but runtime is not ready yet."
                            );
                            await loadMcpServers();
                            return;
                          }

                          if (attempts >= maxAttempts) {
                            clearInterval(timer);
                            setGwPollTimer(null);
                            setGwConnecting(false);
                            setGwMessage(
                              status?.warnings?.[0] || "Authorization still pending. Please finish OAuth in the browser."
                            );
                          }
                        }, 3000);

                        setGwPollTimer(timer);
                      } catch (e) {
                        setGwConnecting(false);
                        setGwMessage(`Failed to start OAuth: ${e}`);
                      }
                    }}
                  >
                    {gwConnecting() ? "Waiting for browser…" : "Connect with Google"}
                  </button>
                </Show>

                <Show when={googleStatus()}>
                  <button
                    class="btn-secondary"
                    onClick={async () => {
                      const normalized = gwAccount().trim() || "personal";
                      await setGoogleAccount(normalized);
                      const status = await loadGoogleStatus(normalized);
                      await loadMcpServers();

                      if (!status) {
                        setGwMessage("Unable to fetch Google status.");
                      } else if (status.connected) {
                        setGwMessage("Google auth and runtime are healthy.");
                      } else if (status.auth_ready && !status.runtime_ready) {
                        setGwMessage(`OAuth ready; runtime not ready (state=${status.mcp?.state ?? "unknown"}).`);
                      } else if (!status.auth_ready) {
                        setGwMessage("Google OAuth is not ready.");
                      } else {
                        setGwMessage("Google integration is not ready.");
                      }

                      setTimeout(() => setGwMessage(""), 2500);
                    }}
                  >
                    Refresh status
                  </button>
                </Show>

                <Show when={googleStatus()}>
                  <button
                    class="btn-secondary"
                    onClick={async () => {
                      try {
                        await reconcileMcpRuntime();
                        await loadMcpServers();
                        await loadGoogleStatus(gwAccount());
                        setGwMessage("MCP runtime reconciled.");
                      } catch (e) {
                        setGwMessage(`Failed to reconcile runtime: ${e}`);
                      }
                    }}
                  >
                    Reconcile runtime
                  </button>
                  <button
                    class="btn-secondary"
                    onClick={async () => {
                      try {
                        await restartMcpServerRuntime("gworkspace");
                        await loadMcpServers();
                        await loadGoogleStatus(gwAccount());
                        setGwMessage("gworkspace runtime restarted.");
                      } catch (e) {
                        setGwMessage(`Failed to restart runtime: ${e}`);
                      }
                    }}
                  >
                    Restart runtime
                  </button>
                </Show>

                <Show when={googleStatus()?.token_present}>
                  <button
                    class="btn-danger"
                    onClick={async () => {
                      try {
                        const normalized = gwAccount().trim() || "personal";
                        await setGoogleAccount(normalized);
                        await disconnectGoogle(normalized);
                        await loadMcpServers();
                        setGwMessage("Disconnected. OAuth token removed.");
                      } catch (e) {
                        setGwMessage(`Failed to disconnect: ${e}`);
                      }
                    }}
                  >
                    Disconnect
                  </button>
                </Show>
              </div>

              {/* How it works */}
              <h3 style="margin-top:1.5rem">How it works</h3>
              <div class="tg-howto">
                <ol>
                  <li>Go to <a href="https://console.cloud.google.com/" target="_blank" rel="noopener">Google Cloud Console</a> → APIs &amp; Services → Credentials</li>
                  <li>Create an <strong>OAuth 2.0 Client ID</strong> (Application type: <em>Desktop app</em>)</li>
                  <li>Download the JSON and save it as <code>~/.google-mcp/credentials.json</code></li>
                  <li>Enable these APIs: Gmail, Calendar, Drive, Docs, Sheets, Slides, Forms</li>
                  <li>Come back here and click <strong>Connect with Google</strong></li>
                  <li>Sign in and click <strong>Allow</strong> - KRIA can now access your Workspace</li>
                  <li>Meet requests use calendar conference-link mode (<code>calendar_conference_link</code>)</li>
                </ol>
              </div>
            </section>
          </Show>

          {/* Knowledge Base Tab */}
          <Show when={activeTab() === "knowledge"}>
            <section>
              <h3>Knowledge Base (RAG)</h3>
              <p class="settings-hint">Documents ingested for retrieval-augmented generation. Use the <code>ingest_document_rag</code> tool or ask the assistant to ingest a file.</p>
              <Show when={knowledgeBase().length > 0} fallback={<p class="settings-hint">No documents ingested yet.</p>}>
                <table class="kb-table">
                  <thead>
                    <tr>
                      <th>Name</th>
                      <th>Type</th>
                      <th>Chunks</th>
                      <th>Doc ID</th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={knowledgeBase()}>{(doc) => (
                      <tr>
                        <td>{doc.name}</td>
                        <td>{doc.type}</td>
                        <td>{doc.chunks}</td>
                        <td class="kb-doc-id">{doc.doc_id}</td>
                      </tr>
                    )}</For>
                  </tbody>
                </table>
              </Show>
              <p class="settings-hint">{knowledgeBase().length} document(s) in knowledge base</p>
            </section>
          </Show>

            </div>
          </section>
        </div>

        <div class="modal-footer">
          <button class="btn-secondary" onClick={() => setShowSettings(false)}>Cancel</button>
          <button class="btn-primary" onClick={handleSave} disabled={saving()}>
            {saving() ? "Saving..." : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
};

export default SettingsModal;
