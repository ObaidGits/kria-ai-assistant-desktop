import { Component, For, Show, createSignal } from "solid-js";
import { appStore } from "../stores/app";

const SessionSidebar: Component = () => {
  const { sessions, currentSession, setShowSettings, createSession, switchSession, deleteSession } = appStore;
  const [collapsed, setCollapsed] = createSignal(false);

  return (
    <aside class={`sidebar ${collapsed() ? "collapsed" : ""}`}>
      <div class="sidebar-header">
        <Show when={!collapsed()}>
          <h1 class="logo">K.R.I.A.</h1>
        </Show>
        <div style={{ display: "flex", gap: "4px" }}>
          <button class="sidebar-toggle" title={collapsed() ? "Expand sidebar" : "Collapse sidebar"} onClick={() => setCollapsed((v) => !v)}>
            {collapsed() ? "▶" : "◀"}
          </button>
          <Show when={!collapsed()}>
            <button class="new-session-btn" title="New session" onClick={() => createSession()}>+</button>
          </Show>
        </div>
      </div>

      <Show when={!collapsed()}>
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
          <button class="settings-btn" onClick={() => setShowSettings(true)}>
            ⚙ Settings
          </button>
        </div>
      </Show>
    </aside>
  );
};

export default SessionSidebar;
