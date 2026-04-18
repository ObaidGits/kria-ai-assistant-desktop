import { Component, For, Show, createEffect, createSignal, createMemo, onCleanup, untrack } from "solid-js";
import { appStore } from "../stores/app";
import MessageBubble from "./MessageBubble";
import ExportDropdown from "./ExportDropdown";

interface SlashCmd {
  name: string;
  desc: string;
  action: (args: string) => void;
}

const ChatView: Component = () => {
  let messagesEnd: HTMLDivElement | undefined;
  let fileInput: HTMLInputElement | undefined;
  let textareaRef: HTMLTextAreaElement | undefined;
  const {
    messages,
    isThinking,
    inputText,
    setInputText,
    sendMessage,
    sendImageMessage,
    toggleVoice,
    voiceActive,
    voiceState,
    toolChoiceRequest,
    submitToolChoice,
    dismissToolChoice,
    currentSession,
    sessions,
    isSwapping,
    degradationLevel,
  } = appStore;

  // Derive the title of the current session for exports
  const currentSessionTitle = () => {
    const id = currentSession();
    if (!id) return null;
    return sessions().find((s) => s.id === id)?.title ?? null;
  };

  const [pendingImage, setPendingImage] = createSignal<{ data: Uint8Array; mime: string; preview: string } | null>(null);
  const [isDragOver, setIsDragOver] = createSignal(false);
  const [showSlash, setShowSlash] = createSignal(false);
  const [slashIndex, setSlashIndex] = createSignal(0);

  const starterPrompts = [
    "Plan my day and prioritize deep work blocks.",
    "Audit my TODO list and suggest what to ship today.",
    "Summarize my latest inbox in five bullets.",
  ];

  const clearPendingImage = () => {
    const img = pendingImage();
    if (img) {
      URL.revokeObjectURL(img.preview);
    }
    setPendingImage(null);
    if (fileInput) {
      fileInput.value = "";
    }
  };

  const slashCommands: SlashCmd[] = [
    { name: "/clear", desc: "Clear current messages", action: () => { /* handled in store if needed */ sendMessage("/clear"); } },
    { name: "/session", desc: "Create a new session", action: () => { appStore.createSession(); } },
    { name: "/voice", desc: "Toggle voice input", action: () => { toggleVoice(); } },
    { name: "/settings", desc: "Open settings", action: () => { appStore.setShowSettings(true); } },
  ];

  const filteredSlash = createMemo(() => {
    const text = inputText();
    if (!text.startsWith("/")) return [];
    const query = text.toLowerCase();
    return slashCommands.filter((c) => c.name.startsWith(query));
  });

  // Show slash menu when typing /
  createEffect(() => {
    const cmds = filteredSlash();
    setShowSlash(cmds.length > 0 && inputText().startsWith("/"));
    setSlashIndex(0);
  });

  // Auto-scroll to bottom on new messages
  createEffect(() => {
    messages(); // track
    messagesEnd?.scrollIntoView({ behavior: "smooth" });
  });

  // Reset pending image when session changes to avoid stale preview/input state.
  createEffect(() => {
    currentSession();
    // Avoid tracking `pendingImage` here; otherwise selecting an image retriggers this
    // effect and clears the preview immediately.
    untrack(() => clearPendingImage());
  });

  onCleanup(() => {
    clearPendingImage();
  });

  // Auto-grow textarea
  const autoGrow = () => {
    if (textareaRef) {
      textareaRef.style.height = "auto";
      textareaRef.style.height = Math.min(textareaRef.scrollHeight, 150) + "px";
    }
  };

  const executeSlash = (cmd: SlashCmd) => {
    const args = inputText().slice(cmd.name.length).trim();
    cmd.action(args);
    setInputText("");
    setShowSlash(false);
    if (textareaRef) {
      textareaRef.style.height = "auto";
    }
  };

  const handleSubmit = (e: Event) => {
    e.preventDefault();
    if (showSlash() && filteredSlash().length > 0) {
      executeSlash(filteredSlash()[slashIndex()]);
      return;
    }
    const img = pendingImage();
    if (img) {
      const data = img.data;
      const mime = img.mime;
      clearPendingImage();
      sendImageMessage(data, mime, inputText() || undefined);
    } else {
      sendMessage(inputText());
    }
    if (textareaRef) textareaRef.style.height = "auto";
  };

  const handleKeyDown = (e: KeyboardEvent) => {
    if (showSlash() && filteredSlash().length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSlashIndex((i) => Math.min(i + 1, filteredSlash().length - 1));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setSlashIndex((i) => Math.max(i - 1, 0));
        return;
      }
      if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey)) {
        e.preventDefault();
        executeSlash(filteredSlash()[slashIndex()]);
        return;
      }
      if (e.key === "Escape") {
        setShowSlash(false);
        return;
      }
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      const img = pendingImage();
      if (img) {
        const data = img.data;
        const mime = img.mime;
        clearPendingImage();
        sendImageMessage(data, mime, inputText() || undefined);
      } else {
        sendMessage(inputText());
      }
      if (textareaRef) textareaRef.style.height = "auto";
    }
  };

  const processFile = async (file: File) => {
    if (!file.type.startsWith("image/")) return;
    const previous = pendingImage();
    if (previous) {
      URL.revokeObjectURL(previous.preview);
    }
    const buffer = await file.arrayBuffer();
    const data = new Uint8Array(buffer);
    const preview = URL.createObjectURL(file);
    setPendingImage({ data, mime: file.type, preview });
    if (fileInput) {
      fileInput.value = "";
    }
  };

  const handlePaste = async (e: ClipboardEvent) => {
    const items = e.clipboardData?.items;
    if (!items) return;
    for (const item of Array.from(items)) {
      if (item.type.startsWith("image/")) {
        e.preventDefault();
        const file = item.getAsFile();
        if (file) await processFile(file);
        return;
      }
    }
  };

  const handleDrop = async (e: DragEvent) => {
    e.preventDefault();
    setIsDragOver(false);
    const files = e.dataTransfer?.files;
    if (files && files.length > 0) {
      await processFile(files[0]);
    }
  };

  return (
    <div
      class={`chat-view ${isDragOver() ? "drag-over" : ""}`}
      onDragOver={(e) => { e.preventDefault(); setIsDragOver(true); }}
      onDragLeave={() => setIsDragOver(false)}
      onDrop={handleDrop}
    >
      <div class="chat-toolbar">
        <span class="chat-toolbar-title">
          {currentSessionTitle() ?? "New conversation"}
        </span>
        <ExportDropdown messages={messages} sessionTitle={currentSessionTitle} />
      </div>

      <div class="chat-messages">
        <Show when={messages().length === 0 && !isThinking()}>
          <div class="assistant-welcome-card">
            <div class="assistant-welcome-eyebrow">Personal Mission Control</div>
            <h2>What should we accomplish first?</h2>
            <p>
              Ask for planning, execution help, tool-driven actions, or attach an image for analysis.
            </p>
            <div class="assistant-starter-grid">
              <For each={starterPrompts}>
                {(prompt) => (
                  <button
                    class="starter-prompt"
                    type="button"
                    onClick={() => {
                      setInputText(prompt);
                      textareaRef?.focus();
                      autoGrow();
                    }}
                  >
                    {prompt}
                  </button>
                )}
              </For>
            </div>
          </div>
        </Show>

        <For each={messages()}>
          {(msg) => <MessageBubble message={msg} />}
        </For>

        {isThinking() && (
          <div class="thinking-indicator">
            <span class="dot" /><span class="dot" /><span class="dot" />
          </div>
        )}

        <div ref={messagesEnd} />
      </div>

      <Show when={isSwapping()}>
        <div class="swap-overlay">
          <div class="swap-overlay-content">
            <span class="dot" /><span class="dot" /><span class="dot" />
            <span class="swap-label">Optimizing GPU layers…</span>
          </div>
        </div>
      </Show>

      <Show when={degradationLevel() && degradationLevel() !== "Full"}>
        <div class="degradation-pill">{degradationLevel()}</div>
      </Show>

      <Show when={pendingImage()}>
        <div class="image-preview-bar">
          <img
            src={pendingImage()!.preview}
            alt="Pending upload"
            class="image-preview-thumb"
          />
          <span class="image-preview-label">Image attached</span>
          <button
            class="image-preview-remove"
            onClick={clearPendingImage}
          >✕</button>
        </div>
      </Show>

      <form class="chat-input-bar" onSubmit={handleSubmit} style={{ position: "relative" }}>
        <Show when={showSlash()}>
          <div class="slash-commands">
            {filteredSlash().map((cmd, i) => (
              <div
                class={`slash-command-item ${i === slashIndex() ? "selected" : ""}`}
                onClick={() => executeSlash(cmd)}
              >
                <span class="slash-cmd-name">{cmd.name}</span>
                <span class="slash-cmd-desc">{cmd.desc}</span>
              </div>
            ))}
          </div>
        </Show>
        <button
          type="button"
          class={`voice-btn ${voiceActive() ? "active" : ""} ${voiceActive() ? `voice-state-${voiceState()}` : ""}`}
          onClick={() => toggleVoice()}
          title={voiceActive() ? `Voice: ${voiceState()}` : "Toggle voice input"}
        >
          {voiceState() === "speaking" ? "🔊" : "🎤"}
        </button>
        <button
          type="button"
          class="attach-btn"
          onClick={() => fileInput?.click()}
          title="Attach image"
        >
          📎
        </button>
        <input
          ref={fileInput}
          type="file"
          accept="image/*"
          style={{ display: "none" }}
          onChange={async (e) => {
            const file = e.currentTarget.files?.[0];
            if (file) await processFile(file);
            e.currentTarget.value = "";
          }}
        />
        <textarea
          ref={textareaRef}
          class="chat-input"
          placeholder={isSwapping() ? "Model is swapping GPU layers…" : pendingImage() ? "Describe what you want to know about this image..." : "Ask KRIA anything… (type / for commands)"}
          value={inputText()}
          onInput={(e) => {
            setInputText(e.currentTarget.value);
            autoGrow();
          }}
          onKeyDown={handleKeyDown}
          onPaste={handlePaste}
          rows={1}
          disabled={isSwapping()}
        />
        <button type="submit" class="send-btn" disabled={isSwapping() || (!inputText().trim() && !pendingImage())}>
          Send
        </button>
      </form>

      <Show when={toolChoiceRequest()}>
        {(req) => (
          <div class="modal-overlay tool-choice-overlay">
            <div class="modal tool-choice-modal">
              <div class="modal-header">
                <h2>Choose a Tool</h2>
              </div>
              <div class="modal-body">
                <p>
                  Confidence {Math.round(req().confidence * 100)}% is below the auto-run threshold
                  ({Math.round(req().minConfidence * 100)}%). Pick the tool to continue.
                </p>
                <div class="tool-choice-list">
                  <For each={req().candidates}>
                    {(candidate) => (
                      <button
                        class="tool-choice-item"
                        type="button"
                        onClick={() => submitToolChoice(candidate.name)}
                      >
                        <span class="tool-choice-title">{candidate.label}</span>
                        <span class="tool-choice-meta">
                          {candidate.name} • {Math.round(candidate.confidence * 100)}%
                        </span>
                        <span class="tool-choice-reason">{candidate.reason}</span>
                      </button>
                    )}
                  </For>
                </div>
              </div>
              <div class="modal-footer">
                <button class="btn-secondary" type="button" onClick={dismissToolChoice}>
                  Cancel
                </button>
              </div>
            </div>
          </div>
        )}
      </Show>
    </div>
  );
};

export default ChatView;
