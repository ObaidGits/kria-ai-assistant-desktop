import { Component, For, Show, createEffect, createSignal, createMemo } from "solid-js";
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
  const { messages, isThinking, inputText, setInputText, sendMessage, sendImageMessage, toggleVoice, voiceActive, voiceState, currentSession, sessions } = appStore;

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
      sendImageMessage(img.data, img.mime, inputText() || undefined);
      setPendingImage(null);
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
        sendImageMessage(img.data, img.mime, inputText() || undefined);
        setPendingImage(null);
      } else {
        sendMessage(inputText());
      }
      if (textareaRef) textareaRef.style.height = "auto";
    }
  };

  const processFile = async (file: File) => {
    if (!file.type.startsWith("image/")) return;
    const buffer = await file.arrayBuffer();
    const data = new Uint8Array(buffer);
    const preview = URL.createObjectURL(file);
    setPendingImage({ data, mime: file.type, preview });
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
            onClick={() => {
              const img = pendingImage();
              if (img) URL.revokeObjectURL(img.preview);
              setPendingImage(null);
            }}
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
          placeholder={pendingImage() ? "Describe what you want to know about this image..." : "Ask KRIA anything… (type / for commands)"}
          value={inputText()}
          onInput={(e) => {
            setInputText(e.currentTarget.value);
            autoGrow();
          }}
          onKeyDown={handleKeyDown}
          onPaste={handlePaste}
          rows={1}
        />
        <button type="submit" class="send-btn" disabled={!inputText().trim() && !pendingImage()}>
          Send
        </button>
      </form>
    </div>
  );
};

export default ChatView;
