import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import hljsDarkThemeUrl from "highlight.js/styles/github-dark.css?url";
import hljsLightThemeUrl from "highlight.js/styles/github.css?url";

// --- Signals ---
const [messages, setMessages] = createSignal<Message[]>([]);
const [sessions, setSessions] = createSignal<Session[]>([]);
const [currentSession, setCurrentSession] = createSignal<string | null>(null);
const [isThinking, setIsThinking] = createSignal(false);
const [showSettings, setShowSettings] = createSignal(false);
const [showHitl, setShowHitl] = createSignal(false);
const [hitlRequest, setHitlRequest] = createSignal<HitlRequest | null>(null);
const [voiceActive, setVoiceActive] = createSignal(false);
const [voiceState, setVoiceState] = createSignal<"idle" | "listening" | "processing" | "speaking">("idle");
const [voiceLiveTranscript, setVoiceLiveTranscript] = createSignal("");
const [inputText, setInputText] = createSignal("");
const [settings, setSettings] = createSignal<Record<string, any> | null>(null);
const [models, setModels] = createSignal<any[]>([]);
const resolveInitialTheme = (): "dark" | "light" => {
  if (typeof window === "undefined") return "dark";
  const saved = window.localStorage.getItem("kria_theme");
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

export interface McpServer {
  name: string;
  command: string;
  args: string[];
  enabled: boolean;
  trust_level: string;
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

// --- Actions ---
async function sendMessage(text: string) {
  if (!text.trim()) return;

  const userMsg: Message = {
    id: crypto.randomUUID(),
    role: "user",
    content: text,
    timestamp: Date.now(),
  };
  setMessages((prev) => [...prev, userMsg]);
  setInputText("");
  setIsThinking(true);

  try {
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
    setMessages((prev) => [...prev, errMsg]);
    setIsThinking(false);
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
  setMessages((prev) => [...prev, userMsg]);
  setInputText("");
  setIsThinking(true);

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
    setMessages((prev) => [...prev, errMsg]);
    setIsThinking(false);
  }
}

async function approveAction(requestId: string) {
  await invoke("approve_action", { requestId });
  setShowHitl(false);
  setHitlRequest(null);
}

async function denyAction(requestId: string, reason?: string) {
  await invoke("deny_action", { requestId, reason: reason ?? null });
  setShowHitl(false);
  setHitlRequest(null);
}

async function toggleVoice() {
  if (voiceActive()) {
    await invoke("stop_voice");
    setVoiceActive(false);
    setVoiceState("idle");
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
      setMessages((prev) => [...prev, errMsg]);
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
  try {
    const result = await invoke<Record<string, any>>("get_health");
    setHealthInfo(result);
  } catch (e) {
    console.error("Failed to load health:", e);
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
export interface GoogleWorkspaceStatus {
  connected: boolean;
  account: string;
  credentials_configured: boolean;
}

const [googleStatus, setGoogleStatus] = createSignal<GoogleWorkspaceStatus | null>(null);

async function loadGoogleStatus(account?: string) {
  try {
    const result = await invoke<GoogleWorkspaceStatus>("get_google_workspace_status", { account: account ?? null });
    setGoogleStatus(result);
  } catch (e) {
    console.error("Failed to load Google status:", e);
  }
}

async function connectGoogle(account?: string): Promise<{ status: string; message: string; account: string }> {
  const result = await invoke<{ status: string; message: string; account: string }>(
    "connect_google_workspace",
    { account: account ?? null }
  );
  return result;
}

async function disconnectGoogle(account?: string) {
  await invoke("disconnect_google_workspace", { account: account ?? null });
  await loadGoogleStatus(account);
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
    window.localStorage.setItem("kria_theme", t);
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
    setCurrentSession(result.session_id);
    setMessages([]);
    await loadSessions();
  } catch (e) {
    console.error("Failed to create session:", e);
  }
}

async function switchSession(sessionId: string) {
  try {
    await invoke("switch_session", { sessionId });
    setCurrentSession(sessionId);
    // Load history for this session
    const history = await invoke<{ role: string; content: string; timestamp: string }[]>(
      "get_session_history",
      { sessionId }
    );
    const mapped: Message[] = history.map((t) => ({
      id: crypto.randomUUID(),
      role: t.role as Message["role"],
      content: t.content,
      timestamp: new Date(t.timestamp).getTime() || Date.now(),
    }));
    setMessages(mapped);
  } catch (e) {
    console.error("Failed to switch session:", e);
  }
}

async function deleteSession(sessionId: string) {
  try {
    await invoke("delete_session", { sessionId });
    if (currentSession() === sessionId) {
      setCurrentSession(null);
      setMessages([]);
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
  listen<{ text: string }>("agent:token", (event) => {
    // Append streaming token to last assistant message
    setMessages((prev) => {
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

  listen<{ status?: string; plan?: string }>("agent:thinking", (event) => {
    setIsThinking(true);
  });

  listen("agent:done", () => {
    setIsThinking(false);
    loadSessions(); // Refresh sidebar after response completes
    loadHealth();
  });

  listen<HitlRequest>("agent:approval_required", (event) => {
    setHitlRequest(event.payload);
    setShowHitl(true);
  });

  // Tool call started — add to last assistant message's toolCalls
  listen<{ name: string; params: Record<string, unknown> }>("agent:tool_call", (event) => {
    const { name, params } = event.payload;
    setMessages((prev) => {
      const last = prev[prev.length - 1];
      if (last?.role === "assistant") {
        const tc: ToolCall = { name, args: params, status: "running" };
        return [
          ...prev.slice(0, -1),
          { ...last, toolCalls: [...(last.toolCalls || []), tc] },
        ];
      }
      // If no assistant message yet, create a placeholder
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

  // Tool result — update the matching tool call status and result
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
  }>("agent:tool_result", (event) => {
    const { name, result, success, metadata } = event.payload;
    setMessages((prev) => {
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

  // Approval result — update denied tool calls
  listen<{ action: string; approved: boolean }>("agent:approval_result", (event) => {
    if (!event.payload.approved) {
      setMessages((prev) => {
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

  listen("tray:toggle-voice", () => toggleVoice());
  listen("tray:open-settings", () => setShowSettings(true));

  // Voice pipeline events
  listen<{ state: "idle" | "listening" | "processing" | "speaking" }>("voice:state", (event) => {
    setVoiceState(event.payload.state);
    setVoiceActive(event.payload.state !== "idle");
  });

  listen<{ text: string }>("voice:partial_transcript", (event) => {
    setVoiceLiveTranscript(event.payload.text);
  });

  listen<{ text: string }>("voice:transcript", (event) => {
    // Clear partial transcript on final result
    setVoiceLiveTranscript("");
    // Show the transcript as a user message
    const userMsg: Message = {
      id: crypto.randomUUID(),
      role: "user",
      content: `🎤 ${event.payload.text}`,
      timestamp: Date.now(),
    };
    setMessages((prev) => [...prev, userMsg]);
  });

  listen<{ error: string }>("voice:error", (event) => {
    console.error("Voice error:", event.payload.error);
  });
}

// Initialize listeners on import
initListeners();
// Initialize theme before first render to avoid color/theme flash.
applyTheme(theme());
// Load existing sessions on startup
loadSessions();
// Load settings on startup
loadSettings();
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
  voiceActive,
  voiceState,
  voiceLiveTranscript,
  inputText,
  setInputText,
  settings,
  models,
  theme,
  sendMessage,
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
  connectGoogle,
  disconnectGoogle,
};
