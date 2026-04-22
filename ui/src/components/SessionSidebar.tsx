import { Component, For, Show, createSignal } from "solid-js";
import { appStore } from "../stores/app";
import logo from "../assets/kria-logo.png";

const SessionSidebar: Component = () => {
  const {
    sessions,
    currentSession,
    setShowSettings,
    createSession,
    switchSession,
    deleteSession,
    currentEnvironment,
    setCurrentEnvironment,
  } = appStore;
  const [collapsed, setCollapsed] = createSignal(false);

  return (
    <aside class={`sidebar ${collapsed() ? "collapsed" : ""}`}>
      <div class="sidebar-header">
        <Show when={!collapsed()}>
          <div class="logo">
            <img src={logo} alt="KRIA" class="logo-img" />
            <span class="logo-text">K.R.I.A.</span>
          </div>
        </Show>
        <Show when={collapsed()}>
          <img src={logo} alt="KRIA" class="logo-collapsed" />
        </Show>
        <div class="sidebar-header-actions" style={{ display: "flex", gap: "4px" }}>
          <button class="sidebar-toggle" title={collapsed() ? "Expand sidebar" : "Collapse sidebar"} onClick={() => setCollapsed((v) => !v)}>
            {collapsed() ? "▶" : "◀"}
          </button>
          <Show when={!collapsed()}>
            <button class="new-session-btn" title="New session" onClick={() => createSession()}>+</button>
          </Show>
        </div>
      </div>

      <Show when={!collapsed()}>
        <div class="env-tabs">
          <button
            class={`env-tab ${currentEnvironment() === "assistant" ? "active" : ""}`}
            onClick={() => setCurrentEnvironment("assistant")}
          >
            Assistant
          </button>
          <button
            class={`env-tab ${currentEnvironment() === "prompt_lab" ? "active" : ""}`}
            onClick={() => setCurrentEnvironment("prompt_lab")}
          >
            Prompt Lab
          </button>
        </div>

        <div class="sidebar-intro-card">
          <div class="sidebar-intro-title">
            {currentEnvironment() === "assistant" ? "Assistant Mode" : "Prompt Lab Mode"}
          </div>
          <div class="sidebar-intro-copy">
            {currentEnvironment() === "assistant"
              ? "Context-aware planning with adaptive tool access."
              : "Tool-locked prompt testing for integration diagnostics."}
          </div>
        </div>

        <div class="sidebar-quick-actions">
          <button class="settings-btn primary" onClick={() => createSession()}>
            + New Mission
          </button>
          <button class="settings-btn" onClick={() => setShowSettings(true)}>
            Configure Assistant
          </button>
        </div>

        <div class="session-list">
          <Show when={sessions().length === 0}>
            <div class="session-empty">No conversations yet</div>
          </Show>
          <For each={sessions()}>
            {(session) => (
              <div
                class={`session-item ${currentSession() === session.id ? "active" : ""}`}
                onClick={() => switchSession(session.id)}
              >
                <span class="session-title">{session.title}</span>
                <button
                  class="session-delete"
                  title="Delete session"
                  onClick={(e) => {
                    e.stopPropagation();
                    deleteSession(session.id);
                  }}
                >×</button>
              </div>
            )}
          </For>
        </div>

        <div class="sidebar-footer">
          <div class="sidebar-meta">
            {sessions().length} active session{sessions().length === 1 ? "" : "s"}
          </div>
        </div>
      </Show>
    </aside>
  );
};

export default SessionSidebar;
