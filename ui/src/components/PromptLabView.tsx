import { Component, For, Show, createMemo, createSignal, onMount } from "solid-js";
import { appStore } from "../stores/app";
import MessageBubble from "./MessageBubble";

type ToolOption = { value: string; label: string };

const TOOL_OPTIONS_BY_APP: Record<string, ToolOption[]> = {
  gmail: [
    { value: "", label: "Auto within Gmail" },
    { value: "gw_gmail_inbox", label: "gw_gmail_inbox" },
    { value: "gw_gmail_search", label: "gw_gmail_search" },
    { value: "gw_gmail_read", label: "gw_gmail_read" },
    { value: "gw_gmail_send", label: "gw_gmail_send" },
    { value: "gw_gmail_delete", label: "gw_gmail_delete" },
  ],
  drive: [
    { value: "", label: "Auto within Drive" },
    { value: "gw_drive_list", label: "gw_drive_list" },
    { value: "gw_drive_search", label: "gw_drive_search" },
    { value: "gw_drive_read", label: "gw_drive_read" },
    { value: "gw_drive_delete", label: "gw_drive_delete" },
  ],
  docs: [
    { value: "", label: "Auto within Docs" },
    { value: "gw_docs_read", label: "gw_docs_read" },
    { value: "gw_docs_create", label: "gw_docs_create" },
    { value: "gw_docs_edit", label: "gw_docs_edit" },
  ],
  sheets: [
    { value: "", label: "Auto within Sheets" },
    { value: "gw_sheets_read", label: "gw_sheets_read" },
    { value: "gw_sheets_create", label: "gw_sheets_create" },
    { value: "gw_sheets_edit", label: "gw_sheets_edit" },
  ],
  calendar: [
    { value: "", label: "Auto within Calendar" },
    { value: "gw_calendar_today", label: "gw_calendar_today" },
    { value: "gw_calendar_search", label: "gw_calendar_search" },
    { value: "gw_calendar_create", label: "gw_calendar_create" },
    { value: "gw_calendar_delete", label: "gw_calendar_delete" },
  ],
  slides: [
    { value: "", label: "Auto within Slides" },
    { value: "gw_slides_read", label: "gw_slides_read" },
    { value: "gw_slides_create", label: "gw_slides_create" },
  ],
  forms: [
    { value: "", label: "Auto within Forms" },
    { value: "gw_forms_list", label: "gw_forms_list" },
    { value: "gw_forms_create", label: "gw_forms_create" },
  ],
};

const APP_OPTIONS: Array<{ value: string; label: string }> = [
  { value: "colab", label: "Google Colab" },
  { value: "gmail", label: "Gmail" },
  { value: "drive", label: "Drive" },
  { value: "docs", label: "Docs" },
  { value: "sheets", label: "Sheets" },
  { value: "calendar", label: "Calendar" },
  { value: "slides", label: "Slides" },
  { value: "forms", label: "Forms" },
];

const PromptLabView: Component = () => {
  const {
    messages,
    isThinking,
    inputText,
    setInputText,
    sendLabMessage,
    colabStatus,
    loadColabStatus,
  } = appStore;
  const [selectedAppLock, setSelectedAppLock] = createSignal<string>("gmail");
  const [selectedToolLock, setSelectedToolLock] = createSignal<string>("");
  const [strategy, setStrategy] = createSignal<"direct" | "routed_within_lock">("routed_within_lock");

  onMount(() => {
    void loadColabStatus();
  });

  const colabToolOptions = createMemo<ToolOption[]>(() => {
    const discovered = colabStatus()?.capabilities?.discovered_tools ?? [];
    const seen = new Set<string>();
    const dynamic = discovered
      .map((tool) => ({
        value: tool.name,
        label: tool.operation ? `${tool.operation} (${tool.name})` : tool.name,
      }))
      .filter((tool) => {
        if (!tool.value || seen.has(tool.value)) return false;
        seen.add(tool.value);
        return true;
      });

    return [{ value: "", label: "Auto within Colab" }, ...dynamic];
  });

  const toolOptions = createMemo(() => {
    if (selectedAppLock() === "colab") {
      return colabToolOptions();
    }
    return TOOL_OPTIONS_BY_APP[selectedAppLock()] ?? [{ value: "", label: "Auto" }];
  });

  const handleSubmit = (e: Event) => {
    e.preventDefault();
    const text = inputText();
    if (!text.trim()) return;

    void sendLabMessage(text, {
      appLock: selectedAppLock(),
      toolLock: selectedToolLock() || null,
      strategy: strategy(),
    });
  };

  return (
    <div class="chat-view prompt-lab-view">
      <div class="chat-toolbar">
        <span class="chat-toolbar-title">Tool-Locked Prompt Lab</span>
      </div>

      <div class="prompt-lab-controls">
        <label>
          App Lock
          <select
            value={selectedAppLock()}
            onChange={(e) => {
              const next = e.currentTarget.value;
              setSelectedAppLock(next);
              setSelectedToolLock("");
              if (next === "colab") {
                void loadColabStatus();
              }
            }}
          >
            <For each={APP_OPTIONS}>{(item) => <option value={item.value}>{item.label}</option>}</For>
          </select>
        </label>

        <label>
          Tool Lock
          <select
            value={selectedToolLock()}
            onChange={(e) => setSelectedToolLock(e.currentTarget.value)}
          >
            <For each={toolOptions()}>{(item) => <option value={item.value}>{item.label}</option>}</For>
          </select>
        </label>

        <label>
          Strategy
          <select
            value={strategy()}
            onChange={(e) => setStrategy(e.currentTarget.value as "direct" | "routed_within_lock")}
          >
            <option value="routed_within_lock">Routed within lock</option>
            <option value="direct">Direct locked tool</option>
          </select>
        </label>
      </div>

      <Show when={selectedAppLock() === "colab"}>
        <div class="prompt-lab-trace">
          <strong>Colab:</strong>{" "}
          {colabStatus()?.ready_for_cloud_task
            ? "ready"
            : `not ready (${colabStatus()?.runtime_state ?? "unknown"})`}
          <Show when={colabStatus()?.warnings?.length}>
            <span> | {colabStatus()?.warnings?.[0]}</span>
          </Show>
        </div>
      </Show>

      <div class="prompt-lab-trace">
        <strong>Trace:</strong> app={selectedAppLock()} | tool={selectedToolLock() || "auto"} | mode={strategy()}
      </div>

      <div class="chat-messages">
        <Show when={messages().length === 0 && !isThinking()}>
          <div class="assistant-welcome-card">
            <div class="assistant-welcome-eyebrow">Prompt Lab</div>
            <h2>Test tools without normal tool-guessing.</h2>
            <p>Select app lock, optionally lock one tool, then test with prompts.</p>
          </div>
        </Show>

        <For each={messages()}>{(msg) => <MessageBubble message={msg} />}</For>

        {isThinking() && (
          <div class="thinking-indicator">
            <span class="dot" />
            <span class="dot" />
            <span class="dot" />
          </div>
        )}
      </div>

      <form class="chat-input-bar" onSubmit={handleSubmit}>
        <textarea
          class="chat-input"
          placeholder="Type a prompt to test selected lock..."
          value={inputText()}
          onInput={(e) => setInputText(e.currentTarget.value)}
          rows={1}
        />
        <button type="submit" class="send-btn" disabled={!inputText().trim()}>
          Run
        </button>
      </form>
    </div>
  );
};

export default PromptLabView;
