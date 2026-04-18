import { Component, Show, For, createSignal, createMemo } from "solid-js";
import { marked } from "marked";
import hljs from "highlight.js";
import DOMPurify from "dompurify";
import { appStore, type Message, type ToolCall } from "../stores/app";

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

function parseResultObject(result: unknown): Record<string, any> | null {
  if (!result) return null;
  if (typeof result === "object") return result as Record<string, any>;
  if (typeof result === "string") {
    try {
      const parsed = JSON.parse(result);
      if (parsed && typeof parsed === "object") return parsed as Record<string, any>;
    } catch {
      return null;
    }
  }
  return null;
}

function resultToText(result: unknown): string {
  if (result == null) return "";
  if (typeof result === "string") return result;
  try {
    return JSON.stringify(result, null, 2);
  } catch {
    return String(result);
  }
}

function formatConfidence(conf?: number): string | null {
  if (typeof conf !== "number") return null;
  return `${Math.round(conf * 100)}% confidence`;
}

function formatFreshnessAge(hours?: number | null): string | null {
  if (typeof hours !== "number" || !Number.isFinite(hours)) return null;
  if (hours < 1) return `${Math.max(1, Math.round(hours * 60))}m old`;
  if (hours < 24) return `${Math.round(hours)}h old`;
  return `${Math.round(hours / 24)}d old`;
}

function normalizeRows(payload: unknown): Record<string, any>[] {
  if (!payload) return [];
  if (Array.isArray(payload)) {
    return payload.filter((row) => row && typeof row === "object") as Record<string, any>[];
  }
  if (typeof payload !== "object") return [];

  const data = payload as Record<string, any>;
  for (const key of ["results", "items", "messages", "events", "files", "forms", "rows"]) {
    if (Array.isArray(data[key])) {
      return data[key].filter((row: unknown) => row && typeof row === "object") as Record<string, any>[];
    }
  }

  return [data];
}

function pickFirstString(row: Record<string, any>, keys: string[]): string {
  for (const key of keys) {
    const value = row[key];
    if (typeof value === "string" && value.trim().length > 0) {
      return value.trim();
    }
  }
  return "";
}

const ToolCallBlock: Component<{ tc: ToolCall }> = (props) => {
  const [expanded, setExpanded] = createSignal(false);
  const resultObj = createMemo(() => parseResultObject(props.tc.result));
  const resultText = createMemo(() => resultToText(props.tc.result));

  const newsResults = createMemo(() => {
    if (props.tc.name !== "search_news") return [] as Record<string, any>[];
    const rows = resultObj()?.results;
    return Array.isArray(rows) ? (rows as Record<string, any>[]) : [];
  });

  const webResults = createMemo(() => {
    if (props.tc.name !== "searxng_search" && props.tc.name !== "web_search") {
      return [] as Record<string, any>[];
    }
    const rows = resultObj()?.results;
    if (!Array.isArray(rows)) return [];
    return rows.map((row) => {
      if (typeof row === "string") {
        return { title: row, url: "", snippet: "" };
      }
      return row as Record<string, any>;
    });
  });

  const articleResult = createMemo(() => {
    if (props.tc.name !== "fetch_article") return null;
    return resultObj();
  });

  const googleResult = createMemo(() => {
    if (!props.tc.name.startsWith("gw_")) return null;
    const obj = resultObj();
    if (!obj) return null;

    if (obj.provider === "google_workspace") {
      return {
        kind: String(obj.kind || "google_workspace"),
        data: obj.data ?? obj,
        rawText: typeof obj.raw_text === "string" ? obj.raw_text : "",
      };
    }

    return {
      kind: "google_workspace",
      data: obj,
      rawText: resultText(),
    };
  });

  const googleRows = createMemo(() => normalizeRows(googleResult()?.data));

  const googleCreateTrust = createMemo(() => {
    const payload = googleResult()?.data;
    if (!payload || typeof payload !== "object") return null;

    const row = payload as Record<string, any>;
    const status = typeof row.status === "string" ? row.status : "";
    const explicitVerified = typeof row.verified === "boolean" ? row.verified : null;
    const hasTrustSignal =
      explicitVerified !== null || status === "created_verified" || status === "created_unverified";

    if (!hasTrustSignal) return null;

    const verified = explicitVerified === true || status === "created_verified";
    const unverified = explicitVerified === false || status === "created_unverified";
    const verificationError =
      typeof row.verification_error === "string" ? row.verification_error : "";

    return {
      verified,
      unverified,
      verificationError,
    };
  });

  const canOpenGoogleLinks = createMemo(() => {
    const trust = googleCreateTrust();
    return !trust || trust.verified;
  });

  const hasGoogleMeetLink = createMemo(() => {
    if (!props.tc.name.includes("calendar")) return false;
    return googleRows().some((row) =>
      ["hangoutLink", "meetLink", "conferenceLink", "htmlLink", "url"]
        .map((key) => row[key])
        .some((value) => typeof value === "string" && /meet\.google\.com/i.test(value))
    );
  });

  const hasStructuredCards = createMemo(() =>
    newsResults().length > 0 || webResults().length > 0 || !!articleResult() || !!googleResult()
  );

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
    if (!resultText()) return "";
    return resultText().length > 200
      ? resultText().slice(0, 200) + "..."
      : resultText();
  };

  const openUrl = (url?: string) => {
    if (!url) return;
    window.open(url, "_blank", "noopener,noreferrer");
  };

  const askToFetch = (url?: string) => {
    if (!url) return;
    void appStore.sendMessage(`Fetch and summarize this article: ${url}`);
  };

  const askToCompare = (title?: string, url?: string) => {
    const subject = [title, url].filter(Boolean).join(" ").trim();
    if (!subject) return;
    void appStore.sendMessage(`Cross-check this news with multiple authentic sources and latest updates: ${subject}`);
  };

  const askToRefresh = (topic?: string) => {
    if (!topic) return;
    void appStore.sendMessage(`Get the latest live updates and key developments about: ${topic}`);
  };

  const googlePrimaryLink = (row: Record<string, any>): string => {
    return pickFirstString(row, ["url", "htmlLink", "webViewLink", "alternateLink", "hangoutLink", "meetLink"]);
  };

  const googleTitle = (row: Record<string, any>): string => {
    return (
      pickFirstString(row, ["subject", "title", "summary", "name", "displayName"]) ||
      `Google ${googleResult()?.kind || "item"}`
    );
  };

  const googleSnippet = (row: Record<string, any>): string => {
    return pickFirstString(row, ["preview", "snippet", "description", "text", "content", "body"]);
  };

  const googleMeta = (row: Record<string, any>): string[] => {
    const parts = [
      pickFirstString(row, ["sender", "from", "organizer"]),
      pickFirstString(row, ["date", "updated", "created", "start", "startTime"]),
      pickFirstString(row, ["id", "messageId", "fileId", "eventId"]),
    ].filter(Boolean);
    return parts.slice(0, 3);
  };

  const confidenceLabel = createMemo(() => formatConfidence(props.tc.metadata?.confidence));
  const freshnessLabel = createMemo(() => formatFreshnessAge(props.tc.metadata?.freshnessAgeHours));

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
        <Show when={props.tc.metadata?.sourceCount != null}>
          <span class="tool-metric-badge">{props.tc.metadata?.sourceCount} sources</span>
        </Show>
        <Show when={confidenceLabel()}>
          <span class="tool-metric-badge">{confidenceLabel()}</span>
        </Show>
        <Show when={freshnessLabel()}>
          <span class="tool-metric-badge">{freshnessLabel()}</span>
        </Show>
        <Show when={props.tc.metadata?.regionMatch === true}>
          <span class="tool-metric-badge">region match</span>
        </Show>
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

          <Show when={newsResults().length > 0}>
            <div class="tool-structured-list">
              <strong>News Results:</strong>
              <For each={newsResults().slice(0, 6)}>
                {(item) => (
                  <article class="tool-result-card">
                    <div class="tool-result-card-title">{item.title || "Untitled"}</div>
                    <div class="tool-result-card-meta">
                      <span>{item.source || "unknown source"}</span>
                      <span>{item.age || ""}</span>
                      <span>{item.trust || ""}</span>
                      <Show when={item.cross_referenced}>
                        <span>{item.cross_referenced}</span>
                      </Show>
                    </div>
                    <Show when={item.summary}>
                      <p class="tool-result-card-snippet">{String(item.summary)}</p>
                    </Show>
                    <div class="tool-result-card-actions">
                      <button class="tool-quick-action" onClick={() => openUrl(item.url)}>Open</button>
                      <button class="tool-quick-action" onClick={() => askToFetch(item.url)}>Extract</button>
                      <button class="tool-quick-action" onClick={() => askToCompare(item.title, item.url)}>Verify</button>
                      <button class="tool-quick-action" onClick={() => askToRefresh(item.title)}>Refresh</button>
                    </div>
                  </article>
                )}
              </For>
            </div>
          </Show>

          <Show when={webResults().length > 0}>
            <div class="tool-structured-list">
              <strong>Web Results:</strong>
              <For each={webResults().slice(0, 6)}>
                {(item) => (
                  <article class="tool-result-card">
                    <div class="tool-result-card-title">{String(item.title || item.url || "Web result")}</div>
                    <Show when={item.snippet || item.content}>
                      <p class="tool-result-card-snippet">{String(item.snippet || item.content)}</p>
                    </Show>
                    <div class="tool-result-card-actions">
                      <Show when={item.url}>
                        <button class="tool-quick-action" onClick={() => openUrl(String(item.url))}>Open</button>
                        <button class="tool-quick-action" onClick={() => askToFetch(String(item.url))}>Extract</button>
                      </Show>
                      <button class="tool-quick-action" onClick={() => askToRefresh(String(item.title || item.url || "this topic"))}>Refresh</button>
                    </div>
                  </article>
                )}
              </For>
            </div>
          </Show>

          <Show when={articleResult()}>
            <div class="tool-structured-list">
              <strong>Article Extraction:</strong>
              <article class="tool-result-card">
                <div class="tool-result-card-title">
                  {String(articleResult()?.metadata?.title || articleResult()?.metadata?.sitename || "Fetched article")}
                </div>
                <div class="tool-result-card-meta">
                  <Show when={articleResult()?.metadata?.author}><span>{String(articleResult()?.metadata?.author)}</span></Show>
                  <Show when={articleResult()?.metadata?.date}><span>{String(articleResult()?.metadata?.date)}</span></Show>
                  <Show when={articleResult()?.char_count}><span>{articleResult()?.char_count} chars</span></Show>
                </div>
                <Show when={articleResult()?.text}>
                  <p class="tool-result-card-snippet">{String(articleResult()?.text).slice(0, 420)}...</p>
                </Show>
                <div class="tool-result-card-actions">
                  <Show when={articleResult()?.url}>
                    <button class="tool-quick-action" onClick={() => openUrl(String(articleResult()?.url))}>Open Source</button>
                    <button class="tool-quick-action" onClick={() => askToCompare(String(articleResult()?.metadata?.title || "article"), String(articleResult()?.url))}>Verify Claim</button>
                  </Show>
                </div>
              </article>
            </div>
          </Show>

          <Show when={googleResult()}>
            <div class="tool-structured-list">
              <strong>
                Google {String(googleResult()?.kind || "workspace").replace(/_/g, " ")}:
              </strong>
              <Show when={googleCreateTrust()?.verified}>
                <div class="tool-metric-badge tool-trust-verified" style={{ width: "fit-content", "margin-bottom": "0.45rem" }}>
                  Verified create
                </div>
              </Show>
              <Show when={googleCreateTrust()?.unverified}>
                <div class="tool-metric-badge tool-trust-unverified" style={{ width: "fit-content", "margin-bottom": "0.45rem" }}>
                  Create unverified
                </div>
                <div class="tool-trust-guidance">
                  <strong>Recovery guidance:</strong>
                  <p>
                    This resource may have been created, but KRIA could not verify it yet.
                    Re-run a read check for this item, then use links only after status is verified.
                  </p>
                  <Show when={googleCreateTrust()?.verificationError}>
                    <pre>{String(googleCreateTrust()?.verificationError || "")}</pre>
                  </Show>
                </div>
              </Show>
              <Show when={hasGoogleMeetLink()}>
                <div class="tool-metric-badge" style={{ width: "fit-content", "margin-bottom": "0.45rem" }}>
                  Meet link available
                </div>
              </Show>
              <For each={googleRows().slice(0, 8)}>
                {(row) => {
                  const url = googlePrimaryLink(row);
                  const title = googleTitle(row);
                  const snippet = googleSnippet(row);
                  const meta = googleMeta(row);
                  return (
                    <article class="tool-result-card">
                      <div class="tool-result-card-title">{title}</div>
                      <Show when={meta.length > 0}>
                        <div class="tool-result-card-meta">
                          <For each={meta}>{(part) => <span>{part}</span>}</For>
                        </div>
                      </Show>
                      <Show when={snippet}>
                        <p class="tool-result-card-snippet">{snippet}</p>
                      </Show>
                      <div class="tool-result-card-actions">
                        <Show when={url && canOpenGoogleLinks()}>
                          <button class="tool-quick-action" onClick={() => openUrl(url)}>Open</button>
                        </Show>
                        <Show when={url && !canOpenGoogleLinks()}>
                          <span class="tool-link-locked">Open hidden until verification</span>
                        </Show>
                        <Show when={props.tc.name === "gw_calendar_create" && /meet\.google\.com/i.test(url || "") && canOpenGoogleLinks()}>
                          <button class="tool-quick-action" onClick={() => openUrl(url)}>Join Meet</button>
                        </Show>
                        <button class="tool-quick-action" onClick={() => askToRefresh(title)}>Follow up</button>
                      </div>
                    </article>
                  );
                }}
              </For>
              <Show when={googleRows().length === 0 && googleResult()?.rawText}>
                <div class={`tool-call-result tool-result-${props.tc.status}`}>
                  <strong>Result:</strong>
                  <pre>{String(googleResult()?.rawText || "")}</pre>
                </div>
              </Show>
            </div>
          </Show>

          <Show when={props.tc.result && !hasStructuredCards()}>
            <div class={`tool-call-result tool-result-${props.tc.status}`}>
              <strong>Result:</strong>
              <pre>{resultText()}</pre>
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
