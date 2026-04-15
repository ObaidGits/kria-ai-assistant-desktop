import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// --- Signals ---
const [messages, setMessages] = createSignal<Message[]>([]);
const [sessions, setSessions] = createSignal<Session[]>([]);
const [currentSession, setCurrentSession] = createSignal<string | null>(null);
const [isThinking, setIsThinking] = createSignal(false);
const [showSettings, setShowSettings] = createSignal(false);
const [showHitl, setShowHitl] = createSignal(false);
const [hitlRequest, setHitlRequest] = createSignal<HitlRequest | null>(null);
const [voiceActive, setVoiceActive] = createSignal(false);
const [inputText, setInputText] = createSignal("");

// --- Types ---
export interface Message {
  id: string;
  role: "user" | "assistant" | "system" | "tool";
  content: string;
  timestamp: number;
  toolCalls?: ToolCall[];
}

export interface ToolCall {
  name: string;
  args: Record<string, unknown>;
  result?: string;
  status: "pending" | "running" | "done" | "error" | "denied";
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
  } else {
    await invoke("start_voice");
    setVoiceActive(true);
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

  listen("agent:thinking", () => setIsThinking(true));
  listen("agent:done", () => setIsThinking(false));

  listen<HitlRequest>("agent:approval_required", (event) => {
    setHitlRequest(event.payload);
    setShowHitl(true);
  });

  listen("tray:toggle-voice", () => toggleVoice());
  listen("tray:open-settings", () => setShowSettings(true));
}

// Initialize listeners on import
initListeners();

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
  inputText,
  setInputText,
  sendMessage,
  approveAction,
  denyAction,
  toggleVoice,
};
