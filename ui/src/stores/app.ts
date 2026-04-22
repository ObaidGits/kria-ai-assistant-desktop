import { createMemo, createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import hljsDarkThemeUrl from "highlight.js/styles/github-dark.css?url";
import hljsLightThemeUrl from "highlight.js/styles/github.css?url";

const STORAGE_KEYS = {
  theme: "kria_theme",
  environment: "kria_environment",
  assistantSession: "kria_assistant_session_id",
  promptLabSession: "kria_prompt_lab_session_id",
} as const;

function readStorageValue(key: string): string | null {
  if (typeof window === "undefined") return null;
  const value = window.localStorage.getItem(key);
  return value && value.trim() ? value : null;
}

function writeStorageValue(key: string, value: string | null) {
  if (typeof window === "undefined") return;
  if (value && value.trim()) {
    window.localStorage.setItem(key, value);
  } else {
    window.localStorage.removeItem(key);
  }
}

const resolveInitialEnvironment = (): "assistant" | "prompt_lab" => {
  const saved = readStorageValue(STORAGE_KEYS.environment);
  return saved === "prompt_lab" ? "prompt_lab" : "assistant";
};

// --- Signals ---
const [assistantMessages, setAssistantMessages] = createSignal<Message[]>([]);
const [promptLabMessages, setPromptLabMessages] = createSignal<Message[]>([]);
const [sessions, setSessions] = createSignal<Session[]>([]);
const [assistantCurrentSession, setAssistantCurrentSession] = createSignal<string | null>(
  readStorageValue(STORAGE_KEYS.assistantSession)
);
const [promptLabCurrentSession, setPromptLabCurrentSession] = createSignal<string | null>(
  readStorageValue(STORAGE_KEYS.promptLabSession)
);
const [assistantIsThinking, setAssistantIsThinking] = createSignal(false);
const [promptLabIsThinking, setPromptLabIsThinking] = createSignal(false);
const [showSettings, setShowSettings] = createSignal(false);
const [assistantShowHitl, setAssistantShowHitl] = createSignal(false);
const [promptLabShowHitl, setPromptLabShowHitl] = createSignal(false);
const [assistantHitlRequest, setAssistantHitlRequest] = createSignal<HitlRequest | null>(null);
const [promptLabHitlRequest, setPromptLabHitlRequest] = createSignal<HitlRequest | null>(null);
const [assistantToolChoiceRequest, setAssistantToolChoiceRequest] = createSignal<ToolChoiceRequest | null>(null);
const [promptLabToolChoiceRequest, setPromptLabToolChoiceRequest] = createSignal<ToolChoiceRequest | null>(null);
const [voiceActive, setVoiceActive] = createSignal(false);
const [voiceState, setVoiceState] = createSignal<"idle" | "listening" | "processing" | "speaking">("idle");
const [voiceLiveTranscript, setVoiceLiveTranscript] = createSignal("");
const [voiceLiveConfidence, setVoiceLiveConfidence] = createSignal<number | null>(null);
const [voiceLiveLanguage, setVoiceLiveLanguage] = createSignal("auto");
const [voiceLiveStability, setVoiceLiveStability] = createSignal<number | null>(null);
const [inputText, setInputText] = createSignal("");
const [settings, setSettings] = createSignal<Record<string, any> | null>(null);
const [models, setModels] = createSignal<any[]>([]);
const [audioDevices, setAudioDevices] = createSignal<AudioDevicesData | null>(null);
const resolveInitialTheme = (): "dark" | "light" => {
  if (typeof window === "undefined") return "dark";
  const saved = readStorageValue(STORAGE_KEYS.theme);
  return saved === "light" ? "light" : "dark";
};

const [theme, setTheme] = createSignal<"dark" | "light">(resolveInitialTheme());
const [mcpServers, setMcpServers] = createSignal<McpServer[]>([]);
const [healthInfo, setHealthInfo] = createSignal<Record<string, any> | null>(null);
const [scheduledTasks, setScheduledTasks] = createSignal<ScheduledTask[]>([]);
const [macros, setMacros] = createSignal<MacroInfo[]>([]);
const [workflows, setWorkflows] = createSignal<WorkflowInfo[]>([]);
const [hardwareInfo, setHardwareInfo] = createSignal<HardwareInfoData | null>(null);
const [knowledgeBase, setKnowledgeBase] = createSignal<KnowledgeDoc[]>([]);
const [alerts, setAlerts] = createSignal<ProactiveAlert[]>([]);

// Orchestrator swap state
const [isSwapping, setIsSwapping] = createSignal(false);
const [degradationLevel, setDegradationLevel] = createSignal<string | null>(null);
const [currentEnvironment, setCurrentEnvironmentSignal] = createSignal<"assistant" | "prompt_lab">(
  resolveInitialEnvironment()
);
const [lastPromptLabProfile, setLastPromptLabProfile] = createSignal<PromptLabProfile | undefined>(undefined);
const [latestAgentStage, setLatestAgentStage] = createSignal<AgentStageEvent | null>(null);
const [colabDispatchWarning, setColabDispatchWarning] = createSignal<string | null>(null);

const currentSession = createMemo<string | null>(() =>
  currentEnvironment() === "prompt_lab" ? promptLabCurrentSession() : assistantCurrentSession()
);

const messages = createMemo<Message[]>(() =>
  currentEnvironment() === "prompt_lab" ? promptLabMessages() : assistantMessages()
);

const isThinking = createMemo<boolean>(() =>
  currentEnvironment() === "prompt_lab" ? promptLabIsThinking() : assistantIsThinking()
);

const showHitl = createMemo<boolean>(() =>
  currentEnvironment() === "prompt_lab" ? promptLabShowHitl() : assistantShowHitl()
);

const hitlRequest = createMemo<HitlRequest | null>(() =>
  currentEnvironment() === "prompt_lab" ? promptLabHitlRequest() : assistantHitlRequest()
);

const toolChoiceRequest = createMemo<ToolChoiceRequest | null>(() =>
  currentEnvironment() === "prompt_lab"
    ? promptLabToolChoiceRequest()
    : assistantToolChoiceRequest()
);

let healthLoadInFlight = false;
let healthLoadQueued = false;

// Telegram integration
export interface TelegramConfig {
  enabled: boolean;
  bot_token: string;
  allowed_chat_ids: string;
  auto_start: boolean;
}

export interface TelegramBotInfo {
  valid: boolean;
  bot_name: string;
  bot_username: string;
  bot_id: number;
}

const [telegramConfig, setTelegramConfig] = createSignal<TelegramConfig | null>(null);
const [telegramBotInfo, setTelegramBotInfo] = createSignal<TelegramBotInfo | null>(null);

// --- Types ---
export interface Message {
  id: string;
  role: "user" | "assistant" | "system" | "tool";
  content: string;
  timestamp: number;
  toolCalls?: ToolCall[];
  /** Base64 data URL for image messages */
  imageUrl?: string;
}

export interface ToolCall {
  name: string;
  args: Record<string, unknown>;
  result?: unknown;
  metadata?: ToolResultMetadata;
  status: "pending" | "running" | "done" | "error" | "denied";
}

export interface ToolResultMetadata {
  confidence?: number;
  sourceCount?: number;
  freshnessAgeHours?: number | null;
  regionMatch?: boolean | null;
}

export interface Session {
  id: string;
  title: string;
  updatedAt: number;
}

export interface HitlRequest {
  requestId: string;
  toolName: string;
  args: Record<string, unknown>;
  riskLevel: string;
  reason: string;
}

export interface ToolChoiceCandidate {
  name: string;
  label: string;
  reason: string;
  confidence: number;
}

export interface ToolChoiceRequest {
  query: string;
  confidence: number;
  minConfidence: number;
  candidates: ToolChoiceCandidate[];
}

export interface PromptLabProfile {
  appLock?: string | null;
  toolLock?: string | null;
  strategy?: "direct" | "routed_within_lock";
}

export interface McpServer {
  name: string;
  command: string;
  args: string[];
  enabled: boolean;
  trust_level: string;
  runtime_state?: string;
  runtime_tool_count?: number;
  runtime_error?: string | null;
}

export interface ScheduledTask {
  id: string;
  name: string;
  interval_secs: number;
  prompt: string;
  enabled: boolean;
}

export interface MacroInfo {
  name: string;
  description: string;
  step_count: number;
  created_at: string;
}

export interface WorkflowInfo {
  id: string;
  name: string;
  description: string;
  step_count: number;
  created_at: string;
}

export interface HardwareInfoData {
  tier: string;
  cpu_cores: number;
  total_ram_mb: number;
  vram_mb: number | null;
  gpu_name: string | null;
  os: string;
  hostname: string;
  package_manager: string | null;
  vision_capable: boolean;
  recommended_model: string;
  recommended_stt: string;
  context_window: number;
  gpu_layers: number;
  threads: number;
}

export interface AudioDevicesData {
  inputs: string[];
  outputs: string[];
  default_input: string | null;
  default_output: string | null;
}

export interface KnowledgeDoc {
  doc_id: string;
  name: string;
  type: string;
  chunks: number;
}

export interface ProactiveAlert {
  id: string;
  category: "alert" | "suggestion" | "info";
  title: string;
  message: string;
  suggestion: string | null;
  timestamp: string;
}

export interface AssistantStatus {
  state: "ready" | "warming" | "degraded" | "offline";
  label: string;
  detail: string;
}

export interface AgentStageEvent {
  step: string;
  message: string;
  detail?: Record<string, unknown> | null;
  ts?: string;
}

type StreamScope = "assistant" | "prompt_lab";

function scopeFromEnvironment(): StreamScope {
  return currentEnvironment() === "prompt_lab" ? "prompt_lab" : "assistant";
}

function getScopedCurrentSession(scope: StreamScope): string | null {
  return scope === "prompt_lab" ? promptLabCurrentSession() : assistantCurrentSession();
}

function setScopedCurrentSession(scope: StreamScope, sessionId: string | null) {
  if (scope === "prompt_lab") {
    setPromptLabCurrentSession(sessionId);
    writeStorageValue(STORAGE_KEYS.promptLabSession, sessionId);
  } else {
    setAssistantCurrentSession(sessionId);
    writeStorageValue(STORAGE_KEYS.assistantSession, sessionId);
  }
}

async function ensureScopedSessionActive(scope: StreamScope): Promise<string> {
  let sessionId = getScopedCurrentSession(scope);
  if (!sessionId) {
    const created = await invoke<{ session_id: string }>("create_session");
    sessionId = created.session_id;
    setScopedCurrentSession(scope, sessionId);
    await loadSessions();
  }

  await invoke("switch_session", { sessionId });
  return sessionId;
}

async function syncEnvironmentSession(environment: "assistant" | "prompt_lab") {
  const scope: StreamScope = environment === "prompt_lab" ? "prompt_lab" : "assistant";
  const sessionId = getScopedCurrentSession(scope);
  if (!sessionId) return;

  try {
    const hasMessages = scope === "prompt_lab" ? promptLabMessages().length > 0 : assistantMessages().length > 0;
    await invoke("switch_session", { sessionId });
    if (!hasMessages) {
      const mapped = await loadMappedSessionHistory(sessionId);
      updateScopedMessages(scope, () => mapped);
    }
  } catch (e) {
    console.error("Failed to sync environment session:", e);
  }
}

function setCurrentEnvironment(environment: "assistant" | "prompt_lab") {
  if (currentEnvironment() === environment) return;
  setCurrentEnvironmentSignal(environment);
  writeStorageValue(STORAGE_KEYS.environment, environment);
  void syncEnvironmentSession(environment);
}

function appendScopedMessage(scope: StreamScope, msg: Message) {
  if (scope === "prompt_lab") {
    setPromptLabMessages((prev) => [...prev, msg]);
  } else {
    setAssistantMessages((prev) => [...prev, msg]);
  }
}

function updateScopedMessages(scope: StreamScope, updater: (prev: Message[]) => Message[]) {
  if (scope === "prompt_lab") {
    setPromptLabMessages(updater);
  } else {
    setAssistantMessages(updater);
  }
}

function setScopedThinking(scope: StreamScope, value: boolean) {
  if (scope === "prompt_lab") {
    setPromptLabIsThinking(value);
  } else {
    setAssistantIsThinking(value);
  }
}

function setScopedHitl(scope: StreamScope, request: HitlRequest | null, visible: boolean) {
  if (scope === "prompt_lab") {
    setPromptLabHitlRequest(request);
    setPromptLabShowHitl(visible);
  } else {
    setAssistantHitlRequest(request);
    setAssistantShowHitl(visible);
  }
}

function setScopedToolChoice(scope: StreamScope, req: ToolChoiceRequest | null) {
  if (scope === "prompt_lab") {
    setPromptLabToolChoiceRequest(req);
  } else {
    setAssistantToolChoiceRequest(req);
  }
}

function formatColabDispatchWarning(stage: AgentStageEvent): string {
  const detail = stage.detail && typeof stage.detail === "object" ? stage.detail : null;
  const requestedMode = typeof detail?.requested_mode === "string" ? detail.requested_mode : "colab";
  const effectiveMode = typeof detail?.effective_mode === "string" ? detail.effective_mode : requestedMode;
  const reason = typeof detail?.reason === "string" ? detail.reason : stage.message;
  const runtimeState = typeof detail?.runtime_state === "string" ? detail.runtime_state : null;

  return runtimeState
    ? `Colab routing fallback (${requestedMode} -> ${effectiveMode}): ${reason} [state=${runtimeState}]`
    : `Colab routing fallback (${requestedMode} -> ${effectiveMode}): ${reason}`;
}

// --- Actions ---
async function sendMessage(text: string) {
  if (!text.trim()) return;

  setScopedToolChoice("assistant", null);

  const userMsg: Message = {
    id: crypto.randomUUID(),
    role: "user",
    content: text,
    timestamp: Date.now(),
  };
  appendScopedMessage("assistant", userMsg);
  setInputText("");
  setScopedThinking("assistant", true);

  try {
    await ensureScopedSessionActive("assistant");
    await invoke<{ status: string }>(
      "send_message",
      { message: text }
    );
    // Response arrives asynchronously via agent:token / agent:done events
  } catch (e) {
    const errMsg: Message = {
      id: crypto.randomUUID(),
      role: "system",
      content: `Error: ${e}`,
      timestamp: Date.now(),
    };
    appendScopedMessage("assistant", errMsg);
    setScopedThinking("assistant", false);
  }
}

async function sendLabMessage(text: string, profile?: PromptLabProfile) {
  if (!text.trim()) return;

  setScopedToolChoice("prompt_lab", null);

  const userMsg: Message = {
    id: crypto.randomUUID(),
    role: "user",
    content: text,
    timestamp: Date.now(),
  };
  appendScopedMessage("prompt_lab", userMsg);
  setInputText("");
  setScopedThinking("prompt_lab", true);
  setLastPromptLabProfile(profile);

  const payload = {
    message: text,
    profile: {
      app_lock: profile?.appLock ?? null,
      tool_lock: profile?.toolLock ?? null,
      strategy: profile?.strategy ?? "routed_within_lock",
    },
  };

  try {
    await ensureScopedSessionActive("prompt_lab");
    await invoke<{ status: string }>("send_lab_message", payload);
  } catch (e) {
    const errMsg: Message = {
      id: crypto.randomUUID(),
      role: "system",
      content: `Error: ${e}`,
      timestamp: Date.now(),
    };
    appendScopedMessage("prompt_lab", errMsg);
    setScopedThinking("prompt_lab", false);
  }
}

function uint8ToBase64(bytes: Uint8Array): string {
  const chunkSize = 0x8000;
  let binary = "";
  for (let i = 0; i < bytes.length; i += chunkSize) {
    const chunk = bytes.subarray(i, i + chunkSize);
    binary += String.fromCharCode(...chunk);
  }
  return btoa(binary);
}

async function sendImageMessage(imageData: Uint8Array, mimeType: string, text?: string) {
  const b64 = uint8ToBase64(imageData);
  const dataUrl = `data:${mimeType};base64,${b64}`;

  const userMsg: Message = {
    id: crypto.randomUUID(),
    role: "user",
    content: text || "What's in this image?",
    timestamp: Date.now(),
    imageUrl: dataUrl,
  };
  appendScopedMessage("assistant", userMsg);
  setInputText("");
  setScopedThinking("assistant", true);

  try {
    await invoke<{ status: string; attachment: string }>(
      "send_image_message",
      { imageData: Array.from(imageData), mimeType, text: text || null }
    );
  } catch (e) {
    const errMsg: Message = {
      id: crypto.randomUUID(),
      role: "system",
      content: `Error: ${e}`,
      timestamp: Date.now(),
    };
    appendScopedMessage("assistant", errMsg);
    setScopedThinking("assistant", false);
  }
}

async function approveAction(requestId: string) {
  await invoke("approve_action", { requestId });
  setScopedHitl("assistant", null, false);
  setScopedHitl("prompt_lab", null, false);
}

async function denyAction(requestId: string, reason?: string) {
  await invoke("deny_action", { requestId, reason: reason ?? null });
  setScopedHitl("assistant", null, false);
  setScopedHitl("prompt_lab", null, false);
}

async function toggleVoice() {
  if (voiceActive()) {
    await invoke("stop_voice");
    setVoiceActive(false);
    setVoiceState("idle");
    setVoiceLiveTranscript("");
    setVoiceLiveConfidence(null);
    setVoiceLiveStability(null);
  } else {
    try {
      await invoke("start_voice");
      setVoiceActive(true);
      setVoiceState("listening");
    } catch (e: any) {
      console.error("Failed to start voice:", e);
      const errText = typeof e === "string" ? e : e?.message ?? "Unknown error starting voice";
      const errMsg: Message = {
        id: crypto.randomUUID(),
        role: "system",
        content: `⚠️ Voice Error: ${errText}`,
        timestamp: Date.now(),
        toolCalls: [],
      };
      appendScopedMessage("assistant", errMsg);
      setVoiceActive(false);
      setVoiceState("idle");
    }
  }
}

// --- MCP Server management ---
async function loadMcpServers() {
  try {
    const result = await invoke<McpServer[]>("list_mcp_servers");
    setMcpServers(result);
  } catch (e) {
    console.error("Failed to load MCP servers:", e);
  }
}

async function addMcpServer(name: string, command: string, args: string[], trustLevel?: string) {
  try {
    await invoke("add_mcp_server", { name, command, args, trustLevel: trustLevel ?? null });
    await loadMcpServers();
  } catch (e) {
    console.error("Failed to add MCP server:", e);
    throw e;
  }
}

async function removeMcpServer(name: string) {
  try {
    await invoke("remove_mcp_server", { name });
    await loadMcpServers();
  } catch (e) {
    console.error("Failed to remove MCP server:", e);
    throw e;
  }
}

async function toggleMcpServer(name: string, enabled: boolean) {
  try {
    await invoke("toggle_mcp_server", { name, enabled });
    await loadMcpServers();
  } catch (e) {
    console.error("Failed to toggle MCP server:", e);
    throw e;
  }
}

// --- Health & Automation management ---
async function loadHealth() {
  if (healthLoadInFlight) {
    healthLoadQueued = true;
    return;
  }

  healthLoadInFlight = true;
  try {
    const result = await invoke<Record<string, any>>("get_health");
    setHealthInfo(result);
  } catch (e) {
    console.error("Failed to load health:", e);
  } finally {
    healthLoadInFlight = false;
    if (healthLoadQueued) {
      healthLoadQueued = false;
      void loadHealth();
    }
  }
}

function assistantStatus(): AssistantStatus {
  const info = healthInfo();
  if (!info) {
    return {
      state: "warming",
      label: "Booting assistant",
      detail: "Running initial health checks",
    };
  }

  const services = Array.isArray(info.services) ? info.services : [];
  const modelRouter = services.find((svc: any) => svc?.name === "model_router");
  const statusRaw = String(modelRouter?.status ?? info.status ?? "unknown").toLowerCase();
  const message = String(modelRouter?.message ?? "").trim();

  if (statusRaw === "healthy") {
    return {
      state: "ready",
      label: "Assistant ready",
      detail: message || "Model routing online",
    };
  }

  if (statusRaw === "starting" || statusRaw === "unknown") {
    return {
      state: "warming",
      label: "Assistant warming up",
      detail: message || "Loading model runtime",
    };
  }

  if (statusRaw === "degraded") {
    return {
      state: "degraded",
      label: "Limited availability",
      detail: message || "Model service degraded",
    };
  }

  return {
    state: "offline",
    label: "Assistant unavailable",
    detail: message || "Model service is offline",
  };
}

async function loadScheduledTasks() {
  try {
    const result = await invoke<ScheduledTask[]>("list_scheduled_tasks");
    setScheduledTasks(result);
  } catch (e) {
    console.error("Failed to load tasks:", e);
  }
}

async function addScheduledTask(name: string, intervalSecs: number, prompt: string) {
  try {
    await invoke("add_scheduled_task", { name, intervalSecs, prompt });
    await loadScheduledTasks();
  } catch (e) {
    console.error("Failed to add task:", e);
    throw e;
  }
}

async function removeScheduledTask(taskId: string) {
  try {
    await invoke("remove_scheduled_task", { taskId });
    await loadScheduledTasks();
  } catch (e) {
    console.error("Failed to remove task:", e);
    throw e;
  }
}

async function loadMacros() {
  try {
    const result = await invoke<MacroInfo[]>("list_macros");
    setMacros(result);
  } catch (e) {
    console.error("Failed to load macros:", e);
  }
}

async function deleteMacro(name: string) {
  try {
    await invoke("delete_macro", { name });
    await loadMacros();
  } catch (e) {
    console.error("Failed to delete macro:", e);
    throw e;
  }
}

async function loadWorkflows() {
  try {
    const result = await invoke<WorkflowInfo[]>("list_workflows");
    setWorkflows(result);
  } catch (e) {
    console.error("Failed to load workflows:", e);
  }
}

async function deleteWorkflow(workflowId: string) {
  try {
    await invoke("delete_workflow", { workflowId });
    await loadWorkflows();
  } catch (e) {
    console.error("Failed to delete workflow:", e);
    throw e;
  }
}

async function loadHardwareInfo() {
  try {
    const result = await invoke<HardwareInfoData>("get_hardware_info");
    setHardwareInfo(result);
  } catch (e) {
    console.error("Failed to load hardware info:", e);
  }
}

async function loadKnowledgeBase() {
  try {
    const result = await invoke<{ documents: KnowledgeDoc[]; count: number }>("list_knowledge_base");
    setKnowledgeBase(result.documents);
  } catch (e) {
    console.error("Failed to load knowledge base:", e);
  }
}

async function loadAlerts() {
  try {
    const result = await invoke<{ alerts: ProactiveAlert[]; count: number }>("get_alerts");
    setAlerts(result.alerts);
  } catch (e) {
    console.error("Failed to load alerts:", e);
  }
}

// --- Telegram management ---
async function loadTelegramConfig() {
  try {
    const result = await invoke<TelegramConfig>("get_telegram_config");
    setTelegramConfig(result);
  } catch (e) {
    console.error("Failed to load telegram config:", e);
  }
}

async function saveTelegramConfig(config: TelegramConfig) {
  try {
    await invoke("update_telegram_config", {
      enabled: config.enabled,
      botToken: config.bot_token,
      allowedChatIds: config.allowed_chat_ids,
      autoStart: config.auto_start,
    });
    setTelegramConfig(config);
  } catch (e) {
    console.error("Failed to save telegram config:", e);
    throw e;
  }
}

async function testTelegramConnection(botToken: string): Promise<TelegramBotInfo> {
  const result = await invoke<TelegramBotInfo>("test_telegram_connection", { botToken });
  setTelegramBotInfo(result);
  return result;
}

async function startTelegramMcp() {
  try {
    const result = await invoke<{ status: string; message: string }>("start_telegram_mcp");
    await loadMcpServers();
    return result;
  } catch (e) {
    console.error("Failed to start telegram MCP:", e);
    throw e;
  }
}

async function stopTelegramMcp() {
  try {
    await invoke("stop_telegram_mcp");
    await loadMcpServers();
    await loadTelegramConfig();
  } catch (e) {
    console.error("Failed to stop telegram MCP:", e);
    throw e;
  }
}

// --- Google Workspace ---
export interface GoogleWorkspaceMcpStatus {
  configured_enabled: boolean;
  state: string;
  tool_count: number;
  error: string | null;
}

export interface GoogleWorkspaceCapabilities {
  gmail: boolean;
  drive: boolean;
  calendar: boolean;
  docs: boolean;
  sheets: boolean;
  slides: boolean;
  forms: boolean;
  meet: boolean;
  meet_via_calendar: boolean;
}

export interface GoogleWorkspaceStatus {
  connected: boolean;
  account: string;
  credentials_configured: boolean;
  token_present: boolean;
  auth_ready: boolean;
  runtime_ready: boolean;
  gw_client_wired: boolean;
  mcp: GoogleWorkspaceMcpStatus;
  capabilities: GoogleWorkspaceCapabilities;
  config_dir?: string;
  meet_support_mode: string;
  warnings: string[];
}

const [googleStatus, setGoogleStatus] = createSignal<GoogleWorkspaceStatus | null>(null);

export interface ColabMcpStatus {
  state: string;
  tool_count: number;
  error: string | null;
}

export interface ColabDiscoveredTool {
  name: string;
  operation: string;
  description: string;
  parameter_count: number;
}

export interface ColabCapabilities {
  category: string;
  tool_count: number;
  discovered_tools: ColabDiscoveredTool[];
  features: {
    notebook_discovery: boolean;
    notebook_selection: boolean;
    cell_execution: boolean;
    artifact_io: boolean;
    runtime_lifecycle: boolean;
    package_management: boolean;
    checkpointing: boolean;
  };
  ready_requirements: {
    requires: string[];
    satisfied: boolean;
    missing: string[];
  };
}

export interface ColabTierStatus {
  enabled: boolean;
  connected: boolean;
  ready_for_cloud_task: boolean;
  notebook_selection_required: boolean;
  runtime_state: string;
  selected_notebook: string | null;
  mcp_server_name: string;
  auto_escalate: boolean;
  fallback_to_local: boolean;
  connect_timeout_secs: number;
  keepalive_interval_secs: number;
  checkpoint_interval_secs: number;
  mcp: ColabMcpStatus;
  capabilities: ColabCapabilities;
  warnings: string[];
}

const [colabStatus, setColabStatus] = createSignal<ColabTierStatus | null>(null);

async function loadGoogleStatus(account?: string): Promise<GoogleWorkspaceStatus | null> {
  try {
    const result = await invoke<GoogleWorkspaceStatus>("get_google_workspace_status", { account: account ?? null });
    setGoogleStatus(result);
    return result;
  } catch (e) {
    console.error("Failed to load Google status:", e);
    return null;
  }
}

async function connectGoogle(account?: string): Promise<{ status: string; message: string; account: string }> {
  const result = await invoke<{ status: string; message: string; account: string }>(
    "connect_google_workspace",
    { account: account ?? null }
  );
  return result;
}

async function setGoogleAccount(account: string): Promise<{ account: string; updated: boolean }> {
  return invoke<{ account: string; updated: boolean }>("set_google_workspace_account", { account });
}

async function reconcileMcpRuntime() {
  return invoke<Record<string, unknown>>("reconcile_mcp_runtime");
}

async function restartMcpServerRuntime(name: string) {
  return invoke<Record<string, unknown>>("restart_mcp_server_runtime", { name });
}

async function disconnectGoogle(account?: string) {
  await invoke("disconnect_google_workspace", { account: account ?? null });
  await loadGoogleStatus(account);
}

// --- Colab Tier ---
async function loadColabStatus(): Promise<ColabTierStatus | null> {
  try {
    const result = await invoke<ColabTierStatus>("get_colab_tier_status");
    setColabStatus(result);
    return result;
  } catch (e) {
    console.error("Failed to load Colab status:", e);
    return null;
  }
}

async function connectColab(serverName?: string): Promise<ColabTierStatus | null> {
  await invoke("connect_colab_tier", { serverName: serverName ?? null });
  await loadMcpServers();
  return loadColabStatus();
}

async function disconnectColab(): Promise<ColabTierStatus | null> {
  await invoke("disconnect_colab_tier");
  await loadMcpServers();
  return loadColabStatus();
}

async function setColabNotebook(notebookId: string): Promise<ColabTierStatus | null> {
  const result = await invoke<ColabTierStatus>("set_colab_selected_notebook", { notebookId });
  setColabStatus(result);
  return result;
}

function submitToolChoice(candidateName: string) {
  const req = toolChoiceRequest();
  if (!req) return;
  const scope = scopeFromEnvironment();
  setScopedToolChoice(scope, null);

  const forcedText = `#tool:${candidateName} ${req.query}`;
  if (scope === "prompt_lab") {
    void sendLabMessage(forcedText, lastPromptLabProfile());
  } else {
    void sendMessage(forcedText);
  }
}

function dismissToolChoice() {
  setScopedToolChoice(scopeFromEnvironment(), null);
}

// --- Settings management ---
async function loadSettings() {
  try {
    const result = await invoke<Record<string, any>>("get_settings");
    setSettings(result);
    // Apply theme from loaded settings
    if (result?.ui?.theme) {
      applyTheme(result.ui.theme);
    }
  } catch (e) {
    console.error("Failed to load settings:", e);
  }
}

async function loadAudioDevices() {
  try {
    const result = await invoke<AudioDevicesData>("list_audio_devices");
    setAudioDevices(result);
  } catch (e) {
    console.error("Failed to load audio devices:", e);
    setAudioDevices({
      inputs: [],
      outputs: [],
      default_input: null,
      default_output: null,
    });
  }
}

async function saveSettings(newSettings: Record<string, any>) {
  try {
    await invoke("update_settings", { settings: newSettings });
    setSettings(newSettings);
    // Apply theme if changed
    if (newSettings?.ui?.theme) {
      applyTheme(newSettings.ui.theme);
    }
  } catch (e) {
    console.error("Failed to save settings:", e);
    throw e;
  }
}

async function loadModels() {
  try {
    const result = await invoke<any[]>("list_models");
    setModels(result);
  } catch (e) {
    console.error("Failed to load models:", e);
  }
}

function applyTheme(t: "dark" | "light") {
  setTheme(t);
  if (typeof window !== "undefined") {
    window.localStorage.setItem(STORAGE_KEYS.theme, t);
  }
  if (typeof document !== "undefined") {
    document.documentElement.setAttribute("data-theme", t);
  }
  setHighlightThemeStylesheet(t);
}

function setHighlightThemeStylesheet(t: "dark" | "light") {
  if (typeof document === "undefined") return;

  const linkId = "kria-hljs-theme";
  const href = t === "light" ? hljsLightThemeUrl : hljsDarkThemeUrl;
  const existing = document.getElementById(linkId) as HTMLLinkElement | null;

  if (existing) {
    existing.href = href;
    return;
  }

  const link = document.createElement("link");
  link.id = linkId;
  link.rel = "stylesheet";
  link.href = href;
  document.head.appendChild(link);
}

// --- Session management ---
async function loadSessions() {
  try {
    const result = await invoke<{ id: string; title: string; turn_count: number; last_active: string }[]>("list_sessions");
    const mapped: Session[] = result.map((s) => ({
      id: s.id,
      title: s.title || "Untitled",
      updatedAt: new Date(s.last_active).getTime() || Date.now(),
    }));
    setSessions(mapped);
  } catch (e) {
    console.error("Failed to load sessions:", e);
  }
}

async function createSession() {
  try {
    const result = await invoke<{ session_id: string }>("create_session");
    const scope = scopeFromEnvironment();
    setScopedCurrentSession(scope, result.session_id);
    await invoke("switch_session", { sessionId: result.session_id });
    updateScopedMessages(scope, () => []);
    setScopedToolChoice(scope, null);
    setScopedThinking(scope, false);
    await loadSessions();
  } catch (e) {
    console.error("Failed to create session:", e);
  }
}

function normalizeRole(role: string): Message["role"] {
  if (role === "user" || role === "assistant" || role === "system" || role === "tool") {
    return role;
  }
  return "assistant";
}

function parseStoredToolCall(
  toolName: string,
  rawToolResult: string | null | undefined
): ToolCall {
  let parsed: any = null;
  if (rawToolResult) {
    try {
      parsed = JSON.parse(rawToolResult);
    } catch {
      parsed = rawToolResult;
    }
  }

  const args =
    parsed &&
    typeof parsed === "object" &&
    parsed.args &&
    typeof parsed.args === "object" &&
    !Array.isArray(parsed.args)
      ? (parsed.args as Record<string, unknown>)
      : {};

  const success =
    parsed && typeof parsed === "object" && typeof parsed.success === "boolean"
      ? parsed.success
      : true;

  const result =
    parsed && typeof parsed === "object" && "result" in parsed
      ? parsed.result
      : parsed ?? null;

  const metadataRaw = parsed && typeof parsed === "object" ? parsed.metadata : null;
  const metadata: ToolResultMetadata | undefined =
    metadataRaw && typeof metadataRaw === "object"
      ? {
          confidence: typeof metadataRaw.confidence === "number" ? metadataRaw.confidence : undefined,
          sourceCount: typeof metadataRaw.source_count === "number" ? metadataRaw.source_count : undefined,
          freshnessAgeHours:
            typeof metadataRaw.freshness_age_hours === "number" || metadataRaw.freshness_age_hours === null
              ? metadataRaw.freshness_age_hours
              : undefined,
          regionMatch:
            typeof metadataRaw.region_match === "boolean" || metadataRaw.region_match === null
              ? metadataRaw.region_match
              : undefined,
        }
      : undefined;

  return {
    name: toolName,
    args,
    result,
    status: (success ? "done" : "error") as ToolCall["status"],
    metadata,
  };
}

async function loadMappedSessionHistory(sessionId: string): Promise<Message[]> {
  const history = await invoke<{
    role: string;
    content: string;
    timestamp: string;
    tool_name?: string | null;
    tool_result?: string | null;
  }[]>(
    "get_session_history",
    { sessionId }
  );

  const mapped: Message[] = [];
  for (const t of history) {
    const ts = new Date(t.timestamp).getTime() || Date.now();

    if (t.role === "tool" && t.tool_name) {
      const tc = parseStoredToolCall(t.tool_name, t.tool_result);
      const last = mapped[mapped.length - 1];

      if (last?.role === "assistant") {
        mapped[mapped.length - 1] = {
          ...last,
          toolCalls: [...(last.toolCalls || []), tc],
          timestamp: Math.max(last.timestamp, ts),
        };
      } else {
        mapped.push({
          id: crypto.randomUUID(),
          role: "assistant",
          content: "",
          timestamp: ts,
          toolCalls: [tc],
        });
      }
      continue;
    }

    mapped.push({
      id: crypto.randomUUID(),
      role: normalizeRole(t.role),
      content: t.content,
      timestamp: ts,
    });
  }

  return mapped;
}

async function switchSession(sessionId: string) {
  try {
    const scope = scopeFromEnvironment();
    await invoke("switch_session", { sessionId });
    setScopedCurrentSession(scope, sessionId);
    const mapped = await loadMappedSessionHistory(sessionId);
    updateScopedMessages(scope, () => mapped);
  } catch (e) {
    console.error("Failed to switch session:", e);
  }
}

async function deleteSession(sessionId: string) {
  try {
    await invoke("delete_session", { sessionId });
    if (assistantCurrentSession() === sessionId) {
      setScopedCurrentSession("assistant", null);
      setAssistantMessages([]);
      setAssistantIsThinking(false);
      setScopedToolChoice("assistant", null);
      setScopedHitl("assistant", null, false);
    }
    if (promptLabCurrentSession() === sessionId) {
      setScopedCurrentSession("prompt_lab", null);
      setPromptLabMessages([]);
      setPromptLabIsThinking(false);
      setScopedToolChoice("prompt_lab", null);
      setScopedHitl("prompt_lab", null, false);
    }
    await loadSessions();
  } catch (e) {
    console.error("Failed to delete session:", e);
  }
}

async function renameSession(sessionId: string, title: string) {
  try {
    await invoke("rename_session", { sessionId, title });
    await loadSessions();
  } catch (e) {
    console.error("Failed to rename session:", e);
  }
}

// --- Event listeners (set up once) ---
function initListeners() {
  const registerStreamListeners = (eventPrefix: "agent" | "prompt_lab", scope: StreamScope) => {
    listen<{ text: string }>(`${eventPrefix}:token`, (event) => {
      updateScopedMessages(scope, (prev) => {
        const last = prev[prev.length - 1];
        if (last?.role === "assistant") {
          return [
            ...prev.slice(0, -1),
            { ...last, content: last.content + event.payload.text },
          ];
        }
        return [
          ...prev,
          {
            id: crypto.randomUUID(),
            role: "assistant",
            content: event.payload.text,
            timestamp: Date.now(),
          },
        ];
      });
    });

    listen<{ status?: string; plan?: string }>(`${eventPrefix}:thinking`, () => {
      setScopedThinking(scope, true);
    });

    listen(`${eventPrefix}:done`, () => {
      setScopedThinking(scope, false);
      loadSessions();
      loadHealth();
    });

    listen<HitlRequest>(`${eventPrefix}:approval_required`, (event) => {
      setScopedHitl(scope, event.payload, true);
    });

    listen<ToolChoiceRequest>(`${eventPrefix}:tool_choice_required`, (event) => {
      setScopedToolChoice(scope, event.payload);
      setScopedThinking(scope, false);
    });

    listen<{ name: string; params: Record<string, unknown> }>(`${eventPrefix}:tool_call`, (event) => {
      const { name, params } = event.payload;
      updateScopedMessages(scope, (prev) => {
        const last = prev[prev.length - 1];
        if (last?.role === "assistant") {
          const tc: ToolCall = { name, args: params, status: "running" };
          return [
            ...prev.slice(0, -1),
            { ...last, toolCalls: [...(last.toolCalls || []), tc] },
          ];
        }
        return [
          ...prev,
          {
            id: crypto.randomUUID(),
            role: "assistant",
            content: "",
            timestamp: Date.now(),
            toolCalls: [{ name, args: params, status: "running" }],
          },
        ];
      });
    });

    listen<{
      name: string;
      result: unknown;
      success: boolean;
      metadata?: {
        confidence?: number;
        source_count?: number;
        freshness_age_hours?: number | null;
        region_match?: boolean | null;
      } | null;
    }>(`${eventPrefix}:tool_result`, (event) => {
      const { name, result, success, metadata } = event.payload;
      updateScopedMessages(scope, (prev) => {
        const last = prev[prev.length - 1];
        if (last?.role === "assistant" && last.toolCalls?.length) {
          const updated = last.toolCalls.map((tc) => {
            if (tc.name === name && tc.status === "running") {
              return {
                ...tc,
                status: (success ? "done" : "error") as ToolCall["status"],
                result,
                metadata: metadata
                  ? {
                      confidence: metadata.confidence,
                      sourceCount: metadata.source_count,
                      freshnessAgeHours: metadata.freshness_age_hours,
                      regionMatch: metadata.region_match,
                    }
                  : tc.metadata,
              };
            }
            return tc;
          });
          return [...prev.slice(0, -1), { ...last, toolCalls: updated }];
        }
        return prev;
      });
    });

    listen<{ action: string; approved: boolean }>(`${eventPrefix}:approval_result`, (event) => {
      if (!event.payload.approved) {
        updateScopedMessages(scope, (prev) => {
          const last = prev[prev.length - 1];
          if (last?.role === "assistant" && last.toolCalls?.length) {
            const updated = last.toolCalls.map((tc) => {
              if (tc.name === event.payload.action && tc.status === "running") {
                return { ...tc, status: "denied" as ToolCall["status"], result: "User denied" };
              }
              return tc;
            });
            return [...prev.slice(0, -1), { ...last, toolCalls: updated }];
          }
          return prev;
        });
      }
    });
  };

  registerStreamListeners("agent", "assistant");
  registerStreamListeners("prompt_lab", "prompt_lab");

  listen<AgentStageEvent>("agent:stage", (event) => {
    const stage = event.payload;
    setLatestAgentStage(stage);

    if (stage.step === "colab_dispatch_fallback_local" || stage.step === "colab_dispatch_blocked") {
      setColabDispatchWarning(formatColabDispatchWarning(stage));
      void loadColabStatus();
      return;
    }

    if (stage.step === "colab_dispatch_ready") {
      setColabDispatchWarning(null);
      void loadColabStatus();
    }
  });

  listen("tray:toggle-voice", () => toggleVoice());
  listen("tray:open-settings", () => setShowSettings(true));

  // Voice pipeline events
  listen<{ state: "idle" | "listening" | "processing" | "speaking" }>("voice:state", (event) => {
    setVoiceState(event.payload.state);
    setVoiceActive(event.payload.state !== "idle");
    if (event.payload.state === "idle") {
      setVoiceLiveTranscript("");
      setVoiceLiveConfidence(null);
      setVoiceLiveStability(null);
    }
  });

  listen<{ text: string; confidence?: number; language?: string; stability?: number }>("voice:partial_transcript", (event) => {
    setVoiceLiveTranscript(event.payload.text);
    setVoiceLiveConfidence(event.payload.confidence ?? null);
    setVoiceLiveLanguage(event.payload.language ?? "auto");
    setVoiceLiveStability(event.payload.stability ?? null);
  });

  listen<{ text: string; confidence?: number; language?: string; stability?: number }>("voice:transcript", (event) => {
    setVoiceLiveTranscript("");
    setVoiceLiveConfidence(event.payload.confidence ?? null);
    setVoiceLiveLanguage(event.payload.language ?? "auto");
    setVoiceLiveStability(event.payload.stability ?? null);
    const userMsg: Message = {
      id: crypto.randomUUID(),
      role: "user",
      content: `🎤 ${event.payload.text}`,
      timestamp: Date.now(),
    };
    appendScopedMessage("assistant", userMsg);
  });

  listen<{ error: string }>("voice:error", (event) => {
    console.error("Voice error:", event.payload.error);
    const errMsg: Message = {
      id: crypto.randomUUID(),
      role: "system",
      content: `⚠️ Voice Error: ${event.payload.error}`,
      timestamp: Date.now(),
    };
    appendScopedMessage("assistant", errMsg);
  });

  // Orchestrator events — track GPU swap state
  listen<{ from_ngl: number; to_ngl: number; emergency: boolean }>(
    "orchestrator:swap_started",
    () => {
      setIsSwapping(true);
    }
  );

  listen<{ new_ngl: number; new_context: number; duration_ms: number }>(
    "orchestrator:swap_completed",
    () => {
      setIsSwapping(false);
    }
  );

  listen<{ level: string }>("orchestrator:degradation_changed", (event) => {
    setDegradationLevel(event.payload.level);
  });

  listen<ColabTierStatus | null>("colab:status", (event) => {
    const payload = event.payload;
    if (payload && typeof payload === "object") {
      setColabStatus(payload as ColabTierStatus);
      if ((payload as ColabTierStatus).ready_for_cloud_task) {
        setColabDispatchWarning(null);
      }
      return;
    }

    void loadColabStatus();
  });
}

async function initializeSessionPersistence() {
  await loadSessions();
  await syncEnvironmentSession(currentEnvironment());
}

// Initialize listeners on import
initListeners();
// Initialize theme before first render to avoid color/theme flash.
applyTheme(theme());
// Load existing sessions on startup
void initializeSessionPersistence();
// Load settings on startup
loadSettings();
loadAudioDevices();
void loadColabStatus();
// Prime and refresh system health for UI status indicators.
loadHealth();
setInterval(() => {
  loadHealth();
}, 12000);

// --- Export store ---
export const appStore = {
  messages,
  sessions,
  currentSession,
  isThinking,
  showSettings,
  setShowSettings,
  showHitl,
  hitlRequest,
  toolChoiceRequest,
  voiceActive,
  voiceState,
  voiceLiveTranscript,
  voiceLiveConfidence,
  voiceLiveLanguage,
  voiceLiveStability,
  inputText,
  setInputText,
  currentEnvironment,
  setCurrentEnvironment,
  settings,
  models,
  audioDevices,
  theme,
  sendMessage,
  sendLabMessage,
  sendImageMessage,
  approveAction,
  denyAction,
  toggleVoice,
  loadSessions,
  createSession,
  switchSession,
  deleteSession,
  renameSession,
  loadSettings,
  loadAudioDevices,
  saveSettings,
  loadModels,
  applyTheme,
  mcpServers,
  loadMcpServers,
  addMcpServer,
  removeMcpServer,
  toggleMcpServer,
  healthInfo,
  loadHealth,
  assistantStatus,
  scheduledTasks,
  loadScheduledTasks,
  addScheduledTask,
  removeScheduledTask,
  macros,
  loadMacros,
  deleteMacro,
  workflows,
  loadWorkflows,
  deleteWorkflow,
  hardwareInfo,
  loadHardwareInfo,
  knowledgeBase,
  loadKnowledgeBase,
  alerts,
  loadAlerts,
  telegramConfig,
  telegramBotInfo,
  loadTelegramConfig,
  saveTelegramConfig,
  testTelegramConnection,
  startTelegramMcp,
  stopTelegramMcp,
  googleStatus,
  loadGoogleStatus,
  setGoogleAccount,
  connectGoogle,
  disconnectGoogle,
  colabStatus,
  latestAgentStage,
  colabDispatchWarning,
  loadColabStatus,
  connectColab,
  disconnectColab,
  setColabNotebook,
  reconcileMcpRuntime,
  restartMcpServerRuntime,
  submitToolChoice,
  dismissToolChoice,
  isSwapping,
  degradationLevel,
};
