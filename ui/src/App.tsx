import { Component, Show, For, createSignal, createMemo, onMount, onCleanup } from "solid-js";
import { appStore } from "./stores/app";
import ChatView from "./components/ChatView";
import SessionSidebar from "./components/SessionSidebar";
import SettingsModal from "./components/SettingsModal";
import HitlModal from "./components/HitlModal";
import VoiceOverlay from "./components/VoiceOverlay";

interface Toast {
  id: number;
  message: string;
  type: "success" | "error" | "info";
}

let toastId = 0;

export function addToast(message: string, type: Toast["type"] = "info") {
  const id = ++toastId;
  setToasts((prev) => [...prev, { id, message, type }]);
  setTimeout(() => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }, 4000);
}

const [toasts, setToasts] = createSignal<Toast[]>([]);

const App: Component = () => {
  const { showSettings, showHitl, voiceActive, setShowSettings } = appStore;
  const [showShortcuts, setShowShortcuts] = createSignal(false);

  const assistantStatus = createMemo(() => appStore.assistantStatus());
  const connectedMcpServers = createMemo(
    () => appStore.mcpServers().filter((server) => server.enabled).length
  );
  const statusDotClass = createMemo(() => {
    const state = assistantStatus().state;
    return state === "ready"
      ? "status-dot"
      : state === "warming"
      ? "status-dot warming"
      : state === "degraded"
      ? "status-dot degraded"
      : "status-dot disconnected";
  });

  const shortcuts: { key: string; desc: string }[] = [
    { key: "Ctrl+,", desc: "Open settings" },
    { key: "Ctrl+N", desc: "New session" },
    { key: "Ctrl+Shift+V", desc: "Toggle voice" },
    { key: "Ctrl+K", desc: "Show shortcuts" },
    { key: "Enter", desc: "Send message" },
    { key: "Shift+Enter", desc: "New line" },
    { key: "/command", desc: "Slash commands" },
  ];

  const handleGlobalKeydown = (e: KeyboardEvent) => {
    // Ctrl+, → settings
    if (e.ctrlKey && e.key === ",") {
      e.preventDefault();
      setShowSettings(true);
    }
    // Ctrl+N → new session
    if (e.ctrlKey && e.key === "n") {
      e.preventDefault();
      appStore.createSession();
    }
    // Ctrl+Shift+V → toggle voice
    if (e.ctrlKey && e.shiftKey && e.key === "V") {
      e.preventDefault();
      appStore.toggleVoice();
    }
    // Ctrl+K → show shortcuts
    if (e.ctrlKey && e.key === "k") {
      e.preventDefault();
      setShowShortcuts((v) => !v);
    }
    // Escape → close overlays
    if (e.key === "Escape") {
      setShowShortcuts(false);
    }
  };

  onMount(() => {
    document.addEventListener("keydown", handleGlobalKeydown);
    appStore.loadHealth();
    appStore.loadMcpServers();
    appStore.loadAlerts();
  });

  onCleanup(() => {
    document.removeEventListener("keydown", handleGlobalKeydown);
  });

  return (
    <div class="app">
      <div class="app-layout">
        <SessionSidebar />
        <main class="main-content">
          <div class="assistant-header">
            <div>
              <div class="assistant-header-kicker">Adaptive Workspace Assistant</div>
              <h1>KRIA Command Center</h1>
              <p>{assistantStatus().detail}</p>
            </div>
            <div class="assistant-header-chips">
              <div class="status-pill">
                <span class={statusDotClass()} />
                <span>{assistantStatus().label}</span>
              </div>
              <div class="status-pill subtle">{connectedMcpServers()} MCP online</div>
              <div class="status-pill subtle">{appStore.alerts().length} active alerts</div>
            </div>
          </div>
          <ChatView />
          <div class="status-bar">
            <div class="status-item">
              <span class={statusDotClass()} />
              <span>{assistantStatus().label}</span>
            </div>
            <div class="status-item">
              <span>Core: {assistantStatus().detail}</span>
            </div>
            <div class="status-item">
              <span>MCP: {connectedMcpServers()} online</span>
            </div>
            <div class="status-item">
              <span>{appStore.theme() === "dark" ? "🌙" : "☀️"}</span>
            </div>
          </div>
        </main>
      </div>

      <Show when={showSettings()}>
        <SettingsModal />
      </Show>

      <Show when={showHitl()}>
        <HitlModal />
      </Show>

      <Show when={voiceActive()}>
        <VoiceOverlay />
      </Show>

      {/* Keyboard shortcuts overlay */}
      <Show when={showShortcuts()}>
        <div class="shortcuts-overlay" onClick={() => setShowShortcuts(false)}>
          <div class="shortcuts-panel" onClick={(e) => e.stopPropagation()}>
            <h2>Keyboard Shortcuts</h2>
            {shortcuts.map((s) => (
              <div class="shortcut-row">
                <span>{s.desc}</span>
                <span class="shortcut-key">{s.key}</span>
              </div>
            ))}
          </div>
        </div>
      </Show>

      {/* Toast notifications */}
      <div class="toast-container">
        <For each={toasts()}>
          {(toast) => (
            <div class={`toast toast-${toast.type}`}>
              {toast.message}
            </div>
          )}
        </For>
      </div>
    </div>
  );
};

export default App;
