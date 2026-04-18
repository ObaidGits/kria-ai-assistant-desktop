import {
  Component,
  Show,
  For,
  createSignal,
  createMemo,
  onMount,
  onCleanup,
  Switch,
  Match,
} from "solid-js";
import {
  provisioningStore,
  type ProvisioningStep,
  type DownloadProgress,
} from "../stores/provisioning";

// ── Helpers ──────────────────────────────────────────────────────────────

function formatBytes(bytes: number): string {
  if (bytes === 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  return `${(bytes / Math.pow(1024, i)).toFixed(1)} ${units[i]}`;
}

function formatSpeed(bps: number): string {
  return `${formatBytes(bps)}/s`;
}

function formatEta(remaining: number, speedBps: number): string {
  if (speedBps <= 0) return "—";
  const secs = Math.ceil(remaining / speedBps);
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.ceil(secs / 60)}m`;
  return `${Math.floor(secs / 3600)}h ${Math.ceil((secs % 3600) / 60)}m`;
}

// ── Screen index mapping ─────────────────────────────────────────────────

const STEP_TO_SCREEN: Record<ProvisioningStep, number> = {
  not_started: 0,
  hardware_detection: 1,
  backend_choice: 2,
  model_download: 3,
  sidecar_setup: 4,
  server_verification: 5,
  complete: 5,
};

const SCREEN_LABELS = [
  "Welcome",
  "Hardware",
  "Backend",
  "Models",
  "Processors",
  "Ready",
];

// ── Main Component ───────────────────────────────────────────────────────

const SetupWizard: Component<{ onComplete: () => void }> = (props) => {
  const [screen, setScreen] = createSignal(0);
  const [hwDone, setHwDone] = createSignal(false);

  // Backend choice local state
  const [localBackend, setLocalBackend] = createSignal(true);
  const [externalUrl, setExternalUrl] = createSignal("http://localhost:11434/v1");
  const [externalKey, setExternalKey] = createSignal("");
  const [connectionTested, setConnectionTested] = createSignal<null | boolean>(null);
  const [testingConnection, setTestingConnection] = createSignal(false);

  // Model download
  const [downloadStarted, setDownloadStarted] = createSignal(false);

  // Sidecar
  const [sidecarStarted, setSidecarStarted] = createSignal(false);

  // Verification
  const [verificationStarted, setVerificationStarted] = createSignal(false);

  // Diagnostics
  const [diagnostics, setDiagnostics] = createSignal("");

  // Register event listeners synchronously — per SolidJS lifecycle rules
  onMount(() => {
    provisioningStore.initListeners();
  });

  onCleanup(() => {
    provisioningStore.destroyListeners();
  });

  // Auto-advance screen when backend step changes from events
  const currentStep = provisioningStore.currentStep;

  // ── Screen 0: Welcome ──────────────────────────────────────────────────

  const WelcomeScreen: Component = () => (
    <div class="wizard-screen">
      <div class="wizard-hero">
        <div class="wizard-logo">K.R.I.A.</div>
        <h2>Welcome to K.R.I.A.</h2>
        <p class="wizard-subtitle">
          Your adaptive AI workspace assistant. Let's get everything set up — it
          only takes a few minutes.
        </p>
      </div>
      <div class="wizard-actions">
        <button class="wizard-btn primary" onClick={() => { setScreen(1); startHardwareDetection(); }}>
          Get Started
        </button>
      </div>
    </div>
  );

  // ── Screen 1: Hardware Detection ───────────────────────────────────────

  async function startHardwareDetection() {
    setHwDone(false);
    await provisioningStore.startProvisioning();
    setHwDone(true);
  }

  const HardwareScreen: Component = () => {
    const profile = provisioningStore.hardwareProfile;
    const summary = provisioningStore.hardwareSummary;
    const tier = provisioningStore.tierLabel;

    return (
      <div class="wizard-screen">
        <h2>Detecting Your Hardware</h2>
        <p class="wizard-subtitle">Scanning system capabilities…</p>

        <div class="hw-detection-card">
          <Show when={hwDone() && profile()} fallback={
            <div class="wizard-spinner-row">
              <div class="wizard-spinner" />
              <span>Detecting hardware…</span>
            </div>
          }>
            <div class="hw-results">
              <div class="hw-row">
                <span class="hw-check">✓</span>
                <span>{summary()}</span>
              </div>
              <Show when={profile()}>
                <div class="hw-detail-grid">
                  <div class="hw-detail">
                    <span class="hw-label">OS</span>
                    <span>{profile()!.os}</span>
                  </div>
                  <div class="hw-detail">
                    <span class="hw-label">CPU Cores</span>
                    <span>{profile()!.cpu_cores}</span>
                  </div>
                  <div class="hw-detail">
                    <span class="hw-label">RAM</span>
                    <span>{Math.round(profile()!.total_ram_mb / 1024)} GB</span>
                  </div>
                  <div class="hw-detail">
                    <span class="hw-label">GPU</span>
                    <span>{profile()!.gpu_name ?? "None"}</span>
                  </div>
                  <Show when={profile()!.vram_mb}>
                    <div class="hw-detail">
                      <span class="hw-label">VRAM</span>
                      <span>{Math.round(profile()!.vram_mb! / 1024)} GB</span>
                    </div>
                  </Show>
                  <div class="hw-detail">
                    <span class="hw-label">Tier</span>
                    <span class="hw-tier-badge">{tier()}</span>
                  </div>
                </div>
              </Show>
            </div>
          </Show>
        </div>

        <div class="wizard-actions">
          <button class="wizard-btn secondary" onClick={() => setScreen(0)}>Back</button>
          <button
            class="wizard-btn primary"
            disabled={!hwDone()}
            onClick={() => setScreen(2)}
          >
            Continue
          </button>
        </div>
      </div>
    );
  };

  // ── Screen 2: Backend Choice ───────────────────────────────────────────

  async function testConnection() {
    setTestingConnection(true);
    setConnectionTested(null);
    try {
      // Try a lightweight fetch to the external endpoint
      const url = externalUrl().replace(/\/$/, "");
      const resp = await fetch(`${url}/models`, {
        headers: externalKey() ? { Authorization: `Bearer ${externalKey()}` } : {},
        signal: AbortSignal.timeout(5000),
      });
      setConnectionTested(resp.ok);
    } catch {
      setConnectionTested(false);
    } finally {
      setTestingConnection(false);
    }
  }

  async function confirmBackend() {
    if (localBackend()) {
      await provisioningStore.selectBackend("local");
      setScreen(3);
    } else {
      await provisioningStore.selectBackend(
        "external",
        externalUrl(),
        externalKey() || undefined,
      );
      // Skip model download + server verification → go to sidecar
      setScreen(4);
    }
  }

  const BackendScreen: Component = () => (
    <div class="wizard-screen">
      <h2>Choose Your LLM Backend</h2>
      <p class="wizard-subtitle">
        Run AI locally or connect to an existing server.
      </p>

      <div class="backend-cards">
        <div
          class={`backend-card ${localBackend() ? "selected" : ""}`}
          onClick={() => setLocalBackend(true)}
        >
          <div class="backend-card-icon">🖥️</div>
          <h3>Run Locally</h3>
          <p>
            Download AI models and run everything on your machine. Best privacy,
            no internet needed after setup.
          </p>
        </div>
        <div
          class={`backend-card ${!localBackend() ? "selected" : ""}`}
          onClick={() => setLocalBackend(false)}
        >
          <div class="backend-card-icon">🌐</div>
          <h3>Connect to Server</h3>
          <p>
            Already running Ollama, LM Studio, or another OpenAI-compatible
            server? Connect to it instead.
          </p>
        </div>
      </div>

      <Show when={!localBackend()}>
        <div class="external-config">
          <label class="wizard-label">
            Server URL
            <input
              class="wizard-input"
              type="text"
              value={externalUrl()}
              onInput={(e) => setExternalUrl(e.currentTarget.value)}
              placeholder="http://localhost:11434/v1"
            />
          </label>
          <label class="wizard-label">
            API Key (optional)
            <input
              class="wizard-input"
              type="password"
              value={externalKey()}
              onInput={(e) => setExternalKey(e.currentTarget.value)}
              placeholder="sk-…"
            />
          </label>
          <div class="connection-test-row">
            <button
              class="wizard-btn secondary"
              onClick={testConnection}
              disabled={testingConnection()}
            >
              {testingConnection() ? "Testing…" : "Test Connection"}
            </button>
            <Show when={connectionTested() !== null}>
              <span class={connectionTested() ? "test-pass" : "test-fail"}>
                {connectionTested() ? "✓ Connected" : "✗ Connection failed"}
              </span>
            </Show>
          </div>
        </div>
      </Show>

      <div class="wizard-actions">
        <button class="wizard-btn secondary" onClick={() => setScreen(1)}>Back</button>
        <button
          class="wizard-btn primary"
          disabled={!localBackend() && connectionTested() !== true}
          onClick={confirmBackend}
        >
          Continue
        </button>
      </div>
    </div>
  );

  // ── Screen 3: Model Download ───────────────────────────────────────────

  async function startDownload() {
    setDownloadStarted(true);
    await provisioningStore.runStep("model_download");
    setScreen(4);
  }

  const downloadEntries = createMemo(() =>
    Object.values(provisioningStore.downloadProgress()),
  );

  const totalProgress = createMemo(() => {
    const entries = downloadEntries();
    if (entries.length === 0) return { downloaded: 0, total: 0, pct: 0 };
    const downloaded = entries.reduce((s, e) => s + e.downloaded_bytes, 0);
    const total = entries.reduce((s, e) => s + e.total_bytes, 0);
    return { downloaded, total, pct: total > 0 ? (downloaded / total) * 100 : 0 };
  });

  const ModelDownloadScreen: Component = () => (
    <div class="wizard-screen">
      <h2>Download AI Models</h2>
      <p class="wizard-subtitle">
        Models are required for local inference. This may take a few minutes
        depending on your connection.
      </p>

      <Show when={!downloadStarted()}>
        <div class="wizard-actions">
          <button class="wizard-btn secondary" onClick={() => setScreen(2)}>Back</button>
          <button class="wizard-btn primary" onClick={startDownload}>
            Start Download
          </button>
        </div>
      </Show>

      <Show when={downloadStarted()}>
        <div class="download-progress-section">
          <div class="download-overall">
            <div class="progress-bar-outer">
              <div
                class="progress-bar-inner"
                style={{ width: `${totalProgress().pct}%` }}
              />
            </div>
            <div class="progress-stats">
              <span>
                {formatBytes(totalProgress().downloaded)} / {formatBytes(totalProgress().total)}
              </span>
              <span>{totalProgress().pct.toFixed(0)}%</span>
            </div>
          </div>
          <For each={downloadEntries()}>
            {(entry: DownloadProgress) => (
              <div class="download-file-row">
                <span class="download-filename">{entry.file}</span>
                <div class="progress-bar-outer small">
                  <div
                    class="progress-bar-inner"
                    style={{
                      width: `${entry.total_bytes > 0 ? (entry.downloaded_bytes / entry.total_bytes) * 100 : 0}%`,
                    }}
                  />
                </div>
                <span class="download-speed">{formatSpeed(entry.speed_bps)}</span>
                <span class="download-eta">
                  {formatEta(entry.total_bytes - entry.downloaded_bytes, entry.speed_bps)}
                </span>
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );

  // ── Screen 4: Sidecar (AI Processors) ─────────────────────────────────

  async function startSidecar() {
    setSidecarStarted(true);
    await provisioningStore.runStep("sidecar_setup");
  }

  const sidecarDone = createMemo(() => {
    const s = provisioningStore.sidecarStatus();
    return s === "done" || s === "skipped" || (typeof s === "object" && "failed" in s);
  });

  const sidecarFailed = createMemo(() => {
    const s = provisioningStore.sidecarStatus();
    return typeof s === "object" && "failed" in s;
  });

  const SidecarScreen: Component = () => {
    // Auto-start sidecar setup on first view
    onMount(() => {
      if (!sidecarStarted()) {
        setSidecarStarted(true);
        void provisioningStore.runStep("sidecar_setup");
      }
    });

    return (
      <div class="wizard-screen">
        <h2>AI Processors Setup</h2>
        <p class="wizard-subtitle">
          Setting up vision and document processors…
        </p>

        <div class="sidecar-progress">
          <Show when={!sidecarDone()}>
            <div class="wizard-spinner-row">
              <div class="wizard-spinner" />
              <span>Creating environment &amp; installing packages…</span>
            </div>
          </Show>

          <Show when={sidecarDone() && !sidecarFailed()}>
            <div class="hw-row">
              <span class="hw-check">✓</span>
              <span>AI processors ready</span>
            </div>
          </Show>

          <Show when={sidecarFailed()}>
            <div class="sidecar-warning">
              <strong>⚠ Processors unavailable</strong>
              <p>
                Vision and document processing couldn't be set up. Text chat
                will work normally.
              </p>
              <button class="wizard-btn secondary" onClick={() => {
                setSidecarStarted(false);
                void startSidecar();
              }}>
                Retry
              </button>
            </div>
          </Show>
        </div>

        <div class="wizard-actions">
          <Show when={localBackend()}>
            <button class="wizard-btn secondary" onClick={() => setScreen(3)}>Back</button>
          </Show>
          <button
            class="wizard-btn primary"
            disabled={!sidecarDone()}
            onClick={() => { setScreen(5); startVerification(); }}
          >
            Continue
          </button>
          <Show when={!sidecarDone()}>
            <button class="wizard-btn ghost" onClick={() => { setScreen(5); startVerification(); }}>
              Skip
            </button>
          </Show>
        </div>
      </div>
    );
  };

  // ── Screen 5: Verification & Done ──────────────────────────────────────

  const [verifyDone, setVerifyDone] = createSignal(false);
  const [verifyError, setVerifyError] = createSignal<string | null>(null);

  async function startVerification() {
    setVerificationStarted(true);
    setVerifyError(null);
    try {
      if (localBackend()) {
        await provisioningStore.runStep("server_verification");
      }
      setVerifyDone(true);
    } catch (e) {
      setVerifyError(String(e));
    }
  }

  async function copyDiagnostics() {
    const text = await provisioningStore.getDiagnostics();
    setDiagnostics(text);
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // fallback: just show it
    }
  }

  function finishWizard() {
    localStorage.setItem("kria_wizard_complete", "true");
    props.onComplete();
  }

  const VerificationScreen: Component = () => (
    <div class="wizard-screen">
      <h2>
        <Show when={verifyDone()} fallback="Verifying Setup…">
          K.R.I.A. is Ready
        </Show>
      </h2>

      <div class="verify-section">
        <Show when={!verifyDone() && !verifyError()}>
          <div class="wizard-spinner-row">
            <div class="wizard-spinner" />
            <span>
              {localBackend()
                ? "Testing local AI server…"
                : "Verifying external connection…"}
            </span>
          </div>
        </Show>

        <Show when={verifyDone()}>
          <div class="verify-results">
            <div class="hw-row">
              <span class="hw-check">✓</span>
              <span>
                {localBackend() ? "Local AI server responding" : "External server connected"}
              </span>
            </div>
            <div class="hw-row">
              <span class={sidecarFailed() ? "hw-warn" : "hw-check"}>
                {sidecarFailed() ? "⚠" : "✓"}
              </span>
              <span>
                AI processors: {sidecarFailed() ? "Unavailable (text-only mode)" : "Ready"}
              </span>
            </div>
          </div>
        </Show>

        <Show when={verifyError()}>
          <div class="sidecar-warning">
            <strong>Verification failed</strong>
            <p>{verifyError()}</p>
            <button class="wizard-btn secondary" onClick={startVerification}>
              Retry
            </button>
          </div>
        </Show>
      </div>

      <Show when={verifyDone()}>
        <div class="wizard-actions">
          <button class="wizard-btn primary large" onClick={finishWizard}>
            Start Chatting
          </button>
        </div>
        <button class="wizard-diag-link" onClick={copyDiagnostics}>
          Copy diagnostic info
        </button>
        <Show when={diagnostics()}>
          <pre class="diag-output">{diagnostics()}</pre>
        </Show>
      </Show>
    </div>
  );

  // ── Stepper bar ────────────────────────────────────────────────────────

  const StepperBar: Component = () => (
    <div class="wizard-stepper">
      <For each={SCREEN_LABELS}>
        {(label, i) => (
          <div
            class={`stepper-step ${i() < screen() ? "done" : ""} ${i() === screen() ? "active" : ""}`}
          >
            <div class="stepper-dot">
              <Show when={i() < screen()} fallback={<span>{i() + 1}</span>}>
                <span>✓</span>
              </Show>
            </div>
            <span class="stepper-label">{label}</span>
          </div>
        )}
      </For>
    </div>
  );

  // ── Render ─────────────────────────────────────────────────────────────

  return (
    <div class="setup-wizard">
      <StepperBar />
      <div class="wizard-content">
        <Switch>
          <Match when={screen() === 0}><WelcomeScreen /></Match>
          <Match when={screen() === 1}><HardwareScreen /></Match>
          <Match when={screen() === 2}><BackendScreen /></Match>
          <Match when={screen() === 3}><ModelDownloadScreen /></Match>
          <Match when={screen() === 4}><SidecarScreen /></Match>
          <Match when={screen() === 5}><VerificationScreen /></Match>
        </Switch>
      </div>
    </div>
  );
};

export default SetupWizard;
