import { Component, Show, For, createSignal, createEffect, onMount } from "solid-js";
import { appStore } from "../stores/app";
import { SUPPORTED_LANGUAGES, setLocale } from "../stores/i18n";

type Tab = "llm" | "voice" | "safety" | "ui" | "search" | "services" | "automation" | "hardware" | "knowledge";

const SettingsModal: Component = () => {
  const { setShowSettings, settings, loadSettings, saveSettings, models, loadModels, theme, applyTheme, mcpServers, loadMcpServers, addMcpServer, removeMcpServer, toggleMcpServer, healthInfo, loadHealth, scheduledTasks, loadScheduledTasks, addScheduledTask, removeScheduledTask, macros, loadMacros, deleteMacro, workflows, loadWorkflows, deleteWorkflow, hardwareInfo, loadHardwareInfo, knowledgeBase, loadKnowledgeBase } = appStore;

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

  onMount(async () => {
    await loadSettings();
    await loadModels();
    await loadMcpServers();
    await loadHealth();
    await loadScheduledTasks();
    await loadMacros();
    await loadWorkflows();
    await loadHardwareInfo();
    await loadKnowledgeBase();
  });

  // Sync draft from loaded settings
  createEffect(() => {
    const s = settings();
    if (s) setDraft(JSON.parse(JSON.stringify(s)));
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

  const tabs: { id: Tab; label: string }[] = [
    { id: "llm", label: "LLM" },
    { id: "voice", label: "Voice" },
    { id: "safety", label: "Safety" },
    { id: "ui", label: "Appearance" },
    { id: "search", label: "Search" },
    { id: "services", label: "Services" },
    { id: "automation", label: "Automation" },
    { id: "hardware", label: "Hardware" },
    { id: "knowledge", label: "Knowledge" },
  ];

  return (
    <div class="modal-overlay" onClick={() => setShowSettings(false)}>
      <div class="modal settings-modal" onClick={(e) => e.stopPropagation()}>
        <div class="modal-header">
          <h2>Settings</h2>
          <button class="close-btn" onClick={() => setShowSettings(false)}>×</button>
        </div>

        <div class="settings-tabs">
          {tabs.map((tab) => (
            <button
              class={`settings-tab ${activeTab() === tab.id ? "active" : ""}`}
              onClick={() => setActiveTab(tab.id)}
            >
              {tab.label}
            </button>
          ))}
        </div>

        <div class="modal-body">
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
                <label>VAD Silence (ms)</label>
                <input
                  type="number"
                  value={draft()?.voice?.vad_silence_ms ?? 1000}
                  onInput={(e) => updateField("voice", "vad_silence_ms", parseInt(e.currentTarget.value) || 1000)}
                />
              </div>
              <div class="settings-field">
                <label>Energy Threshold</label>
                <input
                  type="number"
                  value={draft()?.voice?.energy_threshold ?? 2000}
                  onInput={(e) => updateField("voice", "energy_threshold", parseFloat(e.currentTarget.value) || 2000)}
                />
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
                          <span class={`mcp-status-dot ${server.enabled ? "running" : "stopped"}`}></span>
                          {server.name}
                        </div>
                        <div class="mcp-server-cmd">{server.command} {server.args.join(" ")}</div>
                        <div class="mcp-server-trust">Trust: {server.trust_level}</div>
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
