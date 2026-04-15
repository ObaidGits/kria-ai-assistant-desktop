import { Component, For, createEffect } from "solid-js";
import { appStore } from "../stores/app";
import MessageBubble from "./MessageBubble";

const ChatView: Component = () => {
  let messagesEnd: HTMLDivElement | undefined;
  const { messages, isThinking, inputText, setInputText, sendMessage, toggleVoice, voiceActive } = appStore;

  // Auto-scroll to bottom on new messages
  createEffect(() => {
    messages(); // track
    messagesEnd?.scrollIntoView({ behavior: "smooth" });
  });

  const handleSubmit = (e: Event) => {
    e.preventDefault();
    sendMessage(inputText());
  };

  const handleKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      sendMessage(inputText());
    }
  };

  return (
    <div class="chat-view">
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

      <form class="chat-input-bar" onSubmit={handleSubmit}>
        <button
          type="button"
          class={`voice-btn ${voiceActive() ? "active" : ""}`}
          onClick={() => toggleVoice()}
          title="Toggle voice input"
        >
          🎤
        </button>
        <textarea
          class="chat-input"
          placeholder="Ask KRIA anything..."
          value={inputText()}
          onInput={(e) => setInputText(e.currentTarget.value)}
          onKeyDown={handleKeyDown}
          rows={1}
        />
        <button type="submit" class="send-btn" disabled={!inputText().trim()}>
          Send
        </button>
      </form>
    </div>
  );
};

export default ChatView;
