import { createSignal, createMemo } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// ── Types mirroring Rust backend ────────────────────────────────────────

export type ProvisioningStep =
  | "not_started"
  | "hardware_detection"
  | "backend_choice"
  | "model_download"
  | "sidecar_setup"
  | "server_verification"
  | "complete";

export type StepStatus =
  | "pending"
  | "running"
  | "done"
  | "skipped"
  | { failed: { error: string } };

export interface BackendChoice {
  type: "local" | "external";
  url?: string;
  api_key?: string;
  model_name?: string;
}

export interface ProvisioningError {
  step: string;
  message: string;
  timestamp: string;
  retryable: boolean;
}

export interface HardwareProfile {
  os: string;
  tier: string;
  cpu_cores: number;
  total_ram_mb: number;
  vram_mb: number | null;
  gpu_name: string | null;
  hostname: string;
  gpu_vendor: string;
  arch: string;
  cuda_available: boolean;
  gpu_supported: boolean;
}

export interface DownloadProgress {
  file: string;
  downloaded_bytes: number;
  total_bytes: number;
  speed_bps: number;
}

export interface ProvisioningState {
  current_step: ProvisioningStep;
  steps: Record<string, StepStatus>;
  hardware_profile: HardwareProfile | null;
  backend_choice: BackendChoice | null;
  models_dir: string | null;
  errors: ProvisioningError[];
}

// ── Signals ─────────────────────────────────────────────────────────────

const [currentStep, setCurrentStep] = createSignal<ProvisioningStep>("not_started");
const [hardwareProfile, setHardwareProfile] = createSignal<HardwareProfile | null>(null);
const [downloadProgress, setDownloadProgress] = createSignal<Record<string, DownloadProgress>>({});
const [sidecarStatus, setSidecarStatus] = createSignal<StepStatus>("pending");
const [errors, setErrors] = createSignal<ProvisioningError[]>([]);
const [backendChoice, setBackendChoice] = createSignal<BackendChoice | null>(null);
const [steps, setSteps] = createSignal<Record<string, StepStatus>>({});
const [wizardComplete, setWizardComplete] = createSignal(false);
const [loading, setLoading] = createSignal(true);

// ── Derived ─────────────────────────────────────────────────────────────

const isComplete = createMemo(() => currentStep() === "complete");

const tierLabel = createMemo(() => {
  const p = hardwareProfile();
  if (!p) return "";
  const labels: Record<string, string> = {
    lite: "Lite",
    standard: "Standard",
    performance: "Performance",
    high: "High",
  };
  return labels[p.tier] ?? p.tier;
});

const hardwareSummary = createMemo(() => {
  const p = hardwareProfile();
  if (!p) return "";
  const gpu = p.gpu_name ?? "No GPU detected";
  const ram = Math.round(p.total_ram_mb / 1024);
  return `${gpu} + ${ram}GB RAM → ${tierLabel()} tier`;
});

// ── Actions ─────────────────────────────────────────────────────────────

function applyState(state: ProvisioningState) {
  setCurrentStep(state.current_step);
  setSteps(state.steps);
  setErrors(state.errors);
  if (state.hardware_profile) setHardwareProfile(state.hardware_profile);
  if (state.backend_choice) setBackendChoice(state.backend_choice);
  if (state.current_step === "complete") {
    setWizardComplete(true);
    localStorage.setItem("kria_wizard_complete", "true");
  }
  // derive sidecar status from steps
  const sc = state.steps["sidecar_setup"];
  if (sc) setSidecarStatus(sc);
}

async function loadState(): Promise<ProvisioningState | null> {
  try {
    setLoading(true);
    const raw = await invoke<ProvisioningState>("get_provisioning_state");
    applyState(raw);
    return raw;
  } catch (e) {
    console.error("Failed to load provisioning state:", e);
    return null;
  } finally {
    setLoading(false);
  }
}

async function startProvisioning() {
  try {
    const raw = await invoke<ProvisioningState>("start_provisioning");
    applyState(raw);
  } catch (e) {
    console.error("start_provisioning failed:", e);
  }
}

async function selectBackend(
  choice: "local" | "external",
  url?: string,
  apiKey?: string,
  modelName?: string,
) {
  try {
    const raw = await invoke<ProvisioningState>("set_provisioning_backend", {
      choiceType: choice,
      url: url ?? null,
      apiKey: apiKey ?? null,
      modelName: modelName ?? null,
    });
    applyState(raw);
  } catch (e) {
    console.error("set_provisioning_backend failed:", e);
  }
}

async function runStep(step: "model_download" | "sidecar_setup" | "server_verification") {
  try {
    const raw = await invoke<ProvisioningState>("run_provisioning_step", { step });
    applyState(raw);
  } catch (e) {
    console.error(`run_provisioning_step(${step}) failed:`, e);
  }
}

async function getDiagnostics(): Promise<string> {
  try {
    return await invoke<string>("get_provisioning_diagnostics");
  } catch (e) {
    return `Error fetching diagnostics: ${e}`;
  }
}

async function getHardwareProfile(): Promise<HardwareProfile | null> {
  try {
    const p = await invoke<HardwareProfile>("get_hardware_profile");
    setHardwareProfile(p);
    return p;
  } catch (e) {
    console.error("get_hardware_profile failed:", e);
    return null;
  }
}

// ── Event Listeners ─────────────────────────────────────────────────────

let unlisteners: UnlistenFn[] = [];

async function initListeners() {
  unlisteners.push(
    await listen<{ step: string; status: string; profile?: HardwareProfile }>(
      "provisioning:state_changed",
      (event) => {
        const { step, status, profile } = event.payload;
        if (profile) setHardwareProfile(profile);

        // Update step status
        setSteps((prev) => ({ ...prev, [step]: status as StepStatus }));

        // Map step to current wizard step if done
        if (status === "done") {
          const stepOrder: ProvisioningStep[] = [
            "hardware_detection",
            "backend_choice",
            "model_download",
            "sidecar_setup",
            "server_verification",
            "complete",
          ];
          const idx = stepOrder.indexOf(step as ProvisioningStep);
          if (idx >= 0 && idx + 1 < stepOrder.length) {
            setCurrentStep(stepOrder[idx + 1]);
          } else if (step === "complete" || step === "server_verification") {
            setCurrentStep("complete");
            setWizardComplete(true);
            localStorage.setItem("kria_wizard_complete", "true");
          }
        }
      },
    ),
  );

  unlisteners.push(
    await listen<DownloadProgress>("provisioning:progress", (event) => {
      const progress = event.payload;
      setDownloadProgress((prev) => ({ ...prev, [progress.file]: progress }));
    }),
  );
}

function destroyListeners() {
  for (const fn of unlisteners) fn();
  unlisteners = [];
}

// ── Exported Store ──────────────────────────────────────────────────────

export const provisioningStore = {
  // Signals (read-only accessors)
  currentStep,
  hardwareProfile,
  downloadProgress,
  sidecarStatus,
  errors,
  backendChoice,
  steps,
  wizardComplete,
  loading,

  // Derived
  isComplete,
  tierLabel,
  hardwareSummary,

  // Actions
  loadState,
  startProvisioning,
  selectBackend,
  runStep,
  getDiagnostics,
  getHardwareProfile,

  // Lifecycle
  initListeners,
  destroyListeners,
};
