import { Component, Show, For, createSignal, createMemo, onMount, onCleanup } from "solid-js";
import { appStore } from "./stores/app";
import { provisioningStore } from "./stores/provisioning";
import ChatView from "./components/ChatView";
import PromptLabView from "./components/PromptLabView";
import SessionSidebar from "./components/SessionSidebar";
import SettingsModal from "./components/SettingsModal";
import HitlModal from "./components/HitlModal";
import VoiceOverlay from "./components/VoiceOverlay";
import SetupWizard from "./components/SetupWizard";

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
  const {
    showSettings,
    showHitl,
    voiceActive,
    setShowSettings,
    currentEnvironment,
    colabDispatchWarning,
  } = appStore;
  const [showShortcuts, setShowShortcuts] = createSignal(false);
  const [showWizard, setShowWizard] = createSignal(false);
  const [wizardLoading, setWizardLoading] = createSignal(true);

  const assistantStatus = createMemo(() => appStore.assistantStatus());
  const routingMode = createMemo(() => {
    const raw = String(appStore.settings()?.llm?.routing_mode ?? "local").toLowerCase();
    if (raw === "cloud") return "gemini";
    if (raw === "hybrid") return "local";
    if (["local", "colab", "gemini", "external"].includes(raw)) return raw;
    return "local";
  });
  const routingSummary = createMemo(() => {
    const requested = routingMode();
    if (colabDispatchWarning()) {
      return `${requested} -> local fallback`;
    }
    return requested;
  });
  const connectedMcpServers = createMemo(
    () => appStore
      .mcpServers()
      .filter((server) => {
        const runtimeState = String(server.runtime_state ?? (server.enabled ? "running" : "stopped")).toLowerCase();
        return runtimeState === "running";
      })
      .length
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

  const ocrStartupWarning = createMemo(() => {
    const info = appStore.healthInfo();
    const services = Array.isArray(info?.services) ? info!.services : [];
    const ocrSvc = services.find((svc: any) => svc?.name === "ocr_dependency");
    if (!ocrSvc) return null;

    const status = String(ocrSvc.status ?? "").toLowerCase();
    if (status === "degraded" || status === "unhealthy" || status === "stopped") {
        return String(
          ocrSvc.message ||
          "OCR dependency is unavailable. Vision analysis still works, but text extraction quality may be reduced."
        );
    }

    return null;
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

    // Check provisioning state before loading main app
    void (async () => {
      const wizardAlreadyCompleted =
        typeof window !== "undefined" &&
        window.localStorage.getItem("kria_wizard_complete") === "true";

      if (wizardAlreadyCompleted) {
        setShowWizard(false);
        setWizardLoading(false);
        return;
      }

      const state = await provisioningStore.loadState();
      if (state && state.current_step === "complete") {
        window.localStorage.setItem("kria_wizard_complete", "true");
        setShowWizard(false);
      } else if (state && state.current_step !== "complete") {
        setShowWizard(true);
      }
      setWizardLoading(false);
    })();

    appStore.loadHealth();
    appStore.loadMcpServers();
    appStore.loadAlerts();
  });

  onCleanup(() => {
    document.removeEventListener("keydown", handleGlobalKeydown);
  });

  return (
    <div class="app">
      <Show when={wizardLoading()}>
        <div class="setup-wizard">
          <div class="wizard-content">
            <div class="wizard-spinner-row">
              <div class="wizard-spinner" />
              <span>Loading…</span>
            </div>
          </div>
        </div>
      </Show>

      <Show when={!wizardLoading() && showWizard()}>
        <SetupWizard onComplete={() => setShowWizard(false)} />
      </Show>

      <Show when={!wizardLoading() && !showWizard()}>
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
              <div class="status-pill subtle">Routing {routingSummary()}</div>
              <div class="status-pill subtle">{connectedMcpServers()} MCP online</div>
              <div class="status-pill subtle">{appStore.alerts().length} active alerts</div>
            </div>
          </div>
          <Show when={colabDispatchWarning()}>
            <div class="startup-warning-banner">
              <strong>Colab Routing:</strong> {colabDispatchWarning()}
            </div>
          </Show>
          <Show when={ocrStartupWarning()}>
            <div class="startup-warning-banner">
              <strong>OCR Warning:</strong> {ocrStartupWarning()}
            </div>
          </Show>
          <Show when={currentEnvironment() === "assistant"}>
            <ChatView />
          </Show>
          <Show when={currentEnvironment() === "prompt_lab"}>
            <PromptLabView />
          </Show>
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
              <span>Routing: {routingSummary()}</span>
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
