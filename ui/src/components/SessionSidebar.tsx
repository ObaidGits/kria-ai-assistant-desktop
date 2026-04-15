import { Component, For } from "solid-js";
import { appStore } from "../stores/app";

const SessionSidebar: Component = () => {
  const { sessions, currentSession, setShowSettings } = appStore;

  return (
    <aside class="sidebar">
      <div class="sidebar-header">
        <h1 class="logo">K.R.I.A.</h1>
        <button class="new-session-btn" title="New session">+</button>
      </div>

      <div class="session-list">
        <For each={sessions()}>
          {(session) => (
            <div
              class={`session-item ${currentSession() === session.id ? "active" : ""}`}
            >
              <span class="session-title">{session.title}</span>
            </div>
          )}
        </For>
      </div>

      <div class="sidebar-footer">
        <button class="settings-btn" onClick={() => setShowSettings(true)}>
          ⚙ Settings
        </button>
      </div>
    </aside>
  );
};

export default SessionSidebar;
