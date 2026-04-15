import { Component, Show, For, createSignal, createMemo } from "solid-js";
import { marked } from "marked";
import hljs from "highlight.js";
import DOMPurify from "dompurify";
import type { Message, ToolCall } from "../stores/app";

// Configure marked with highlight.js
marked.setOptions({
  breaks: true,
  gfm: true,
});

const renderer = new marked.Renderer();

renderer.code = function ({ text, lang }: { text: string; lang?: string; escaped?: boolean }) {
  const language = lang && hljs.getLanguage(lang) ? lang : "plaintext";
  const highlighted = hljs.highlight(text, { language }).value;
  const langLabel = lang || "";
  return `<div class="code-block-header"><span>${langLabel}</span><button class="copy-code-btn" onclick="navigator.clipboard.writeText(this.closest('.code-block-header').nextElementSibling.textContent)">Copy</button></div><pre><code class="hljs language-${language}">${highlighted}</code></pre>`;
};

renderer.codespan = function ({ text }: { text: string }) {
  return `<code>${text}</code>`;
};

marked.use({ renderer });

function renderMarkdown(content: string): string {
  const raw = marked.parse(content) as string;
  return DOMPurify.sanitize(raw, {
    ADD_ATTR: ["onclick"],
    ALLOWED_TAGS: [
      "p", "br", "strong", "em", "del", "a", "code", "pre", "div",
      "h1", "h2", "h3", "h4", "h5", "h6",
      "ul", "ol", "li", "blockquote", "table", "thead", "tbody",
      "tr", "th", "td", "hr", "span", "button", "img",
    ],
    ALLOWED_ATTR: ["href", "target", "rel", "class", "onclick", "src", "alt"],
  });
}

interface Props {
  message: Message;
}

const ToolCallBlock: Component<{ tc: ToolCall }> = (props) => {
  const [expanded, setExpanded] = createSignal(false);

  const statusIcon = () => {
    switch (props.tc.status) {
      case "running": return "⏳";
      case "done": return "✅";
      case "error": return "❌";
      case "denied": return "🚫";
      default: return "⏱";
    }
  };

  const statusClass = () => `tool-call tool-call-${props.tc.status}`;

  const truncatedResult = () => {
    if (!props.tc.result) return "";
    return props.tc.result.length > 200
      ? props.tc.result.slice(0, 200) + "..."
      : props.tc.result;
  };

  return (
    <div class={statusClass()}>
      <div
        class="tool-call-header"
        onClick={() => setExpanded((v) => !v)}
        style={{ cursor: "pointer", "user-select": "none" }}
      >
        <span class="tool-status-icon">{statusIcon()}</span>
        <span class="tool-name">{props.tc.name}</span>
        <span class="tool-call-args-preview">
          {Object.keys(props.tc.args).length > 0
            ? `(${Object.keys(props.tc.args).join(", ")})`
            : "()"
          }
        </span>
        <span class="tool-expand">{expanded() ? "▼" : "▶"}</span>
      </div>
      <Show when={expanded()}>
        <div class="tool-call-details">
          <Show when={Object.keys(props.tc.args).length > 0}>
            <div class="tool-call-params">
              <strong>Parameters:</strong>
              <pre>{JSON.stringify(props.tc.args, null, 2)}</pre>
            </div>
          </Show>
          <Show when={props.tc.result}>
            <div class={`tool-call-result tool-result-${props.tc.status}`}>
              <strong>Result:</strong>
              <pre>{props.tc.result}</pre>
            </div>
          </Show>
        </div>
      </Show>
      <Show when={!expanded() && props.tc.result}>
        <div class="tool-call-preview">
          <span class="tool-result-preview">{truncatedResult()}</span>
        </div>
      </Show>
    </div>
  );
};

const MessageBubble: Component<Props> = (props) => {
  const roleClass = () => `message message-${props.message.role}`;

  const htmlContent = createMemo(() => {
    if (!props.message.content) return "";
    // Only render markdown for assistant messages
    if (props.message.role === "assistant") {
      return renderMarkdown(props.message.content);
    }
    return "";
  });

  return (
    <div class={roleClass()}>
      <div class="message-header">
        <span class="message-role">
          {props.message.role === "assistant" ? "KRIA" : props.message.role}
        </span>
        <span class="message-time">
          {new Date(props.message.timestamp).toLocaleTimeString()}
        </span>
      </div>

      <Show when={props.message.imageUrl}>
        <div class="message-image">
          <img src={props.message.imageUrl} alt="Attached image" class="message-image-thumb" />
        </div>
      </Show>

      <Show when={props.message.toolCalls?.length}>
        <div class="tool-calls">
          <For each={props.message.toolCalls}>
            {(tc) => <ToolCallBlock tc={tc} />}
          </For>
        </div>
      </Show>

      <Show when={props.message.content}>
        <div class="message-content">
          {props.message.role === "assistant"
            ? <div innerHTML={htmlContent()} />
            : props.message.content
          }
        </div>
      </Show>
    </div>
  );
};

export default MessageBubble;
