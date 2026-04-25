import { Component, Show, For, createSignal, createMemo, createEffect, onCleanup, untrack } from "solid-js";
import { marked } from "marked";
import hljs from "highlight.js";
import DOMPurify from "dompurify";
import { invoke } from "@tauri-apps/api/core";
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

function extractLocalImagePaths(text: string): string[] {
  const paths = new Set<string>();
  const regex = /(\/[^\s"'`]+?\.(?:png|jpe?g|webp|gif))(?=$|[\s"'`),.!?;:\]])/gi;

  for (const match of text.matchAll(regex)) {
    const candidate = (match[1] || "").trim().replace(/[),.!?;:]+$/g, "");
    if (candidate.startsWith("/")) {
      paths.add(candidate);
    }
  }

  return Array.from(paths);
}

function extractStructuredGenerateImagePaths(toolCalls?: ToolCall[]): string[] {
  const paths = new Set<string>();
  for (const tc of toolCalls || []) {
    if (tc.name !== "generate_image") continue;
    const obj = parseResultObject(tc.result);
    if (!obj) continue;

    const directImages = Array.isArray(obj.images) ? obj.images : null;
    const dataImages =
      obj.data && typeof obj.data === "object" && !Array.isArray(obj.data) && Array.isArray((obj.data as Record<string, unknown>).images)
        ? ((obj.data as Record<string, unknown>).images as unknown[])
        : null;
    const nestedResultImages =
      obj.result && typeof obj.result === "object" && !Array.isArray(obj.result) && Array.isArray((obj.result as Record<string, unknown>).images)
        ? ((obj.result as Record<string, unknown>).images as unknown[])
        : null;

    const images = directImages ?? dataImages ?? nestedResultImages ?? [];
    images.forEach((img: any) => {
      const path = typeof img?.path === "string" ? img.path.trim() : "";
      if (path.startsWith("/")) {
        paths.add(path);
      }
    });
  }
  return Array.from(paths);
}

function extractGenerateImagePathsFromToolText(toolCalls?: ToolCall[]): string[] {
  const paths = new Set<string>();
  for (const tc of toolCalls || []) {
    if (tc.name !== "generate_image") continue;
    const text = resultToText(tc.result);
    for (const path of extractLocalImagePaths(text)) {
      paths.add(path);
    }
  }
  return Array.from(paths);
}

function imageFileNameFromPath(path: string, fallback = "kria-generated-image.jpg"): string {
  const name = path.split("/").pop()?.trim() || "";
  return /\.[a-z0-9]+$/i.test(name) ? name : fallback;
}

// ── Copy-to-clipboard helper ────────────────────────────────────────────
function useCopyButton() {
  const [copied, setCopied] = createSignal(false);
  const copy = (text: string) => {
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  };
  return { copied, copy };
}

const ToolCallBlock: Component<{
  tc: ToolCall;
  onOpenImage?: (src: string, options?: { alt?: string; downloadName?: string }) => void;
}> = (props) => {
  const [expanded, setExpanded] = createSignal(false);
  const resultObj = createMemo(() => parseResultObject(props.tc.result));
  const resultText = createMemo(() => resultToText(props.tc.result));

  // Map from image path → base64 data URL (loaded lazily via Tauri command)
  const [imageDataUrls, setImageDataUrls] = createSignal<Record<string, string>>({});
  const [imageLoadAttempts, setImageLoadAttempts] = createSignal<Record<string, number>>({});
  const inFlightImageLoads = new Set<string>();
  const retryTimers = new Map<string, number>();

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

  // Image generation results
  const imageResults = createMemo<Array<{
    path: string;
    width: number;
    height: number;
    style: string;
    provenance: string;
    seed?: number;
    quality?: string;
    steps?: number;
    sampler?: string;
    cfg_scale?: number;
    enhance_mode?: string;
    final_prompt?: string;
  }>>(() => {
    if (props.tc.name !== "generate_image") return [];
    const obj = resultObj();
    if (!obj) return [];

    const directImages = Array.isArray(obj.images) ? obj.images : null;
    const dataImages =
      obj.data && typeof obj.data === "object" && !Array.isArray(obj.data) && Array.isArray((obj.data as Record<string, unknown>).images)
        ? ((obj.data as Record<string, unknown>).images as unknown[])
        : null;
    const nestedResultImages =
      obj.result && typeof obj.result === "object" && !Array.isArray(obj.result) && Array.isArray((obj.result as Record<string, unknown>).images)
        ? ((obj.result as Record<string, unknown>).images as unknown[])
        : null;

    const imgs = directImages ?? dataImages ?? nestedResultImages ?? [];
    return imgs.filter((img: any) => img && typeof img.path === "string") as any[];
  });

  const hasStructuredCards = createMemo(() =>
    newsResults().length > 0 || webResults().length > 0 || !!articleResult() || !!googleResult() || imageResults().length > 0
  );

  const scheduleImageRetry = (path: string) => {
    if (retryTimers.has(path)) return;
    const attempts = untrack(() => imageLoadAttempts()[path] ?? 0);
    if (attempts >= 5) return;
    const delayMs = Math.min(300 * Math.pow(2, attempts), 3000);
    const timerId = window.setTimeout(() => {
      retryTimers.delete(path);
      setImageLoadAttempts((prev) => ({ ...prev, [path]: (prev[path] ?? 0) + 1 }));
    }, delayMs);
    retryTimers.set(path, timerId);
  };

  const imageLoadFailed = (path: string) => {
    const attempts = imageLoadAttempts()[path] ?? 0;
    return attempts >= 5 && !imageDataUrls()[path];
  };

  onCleanup(() => {
    for (const timerId of retryTimers.values()) {
      window.clearTimeout(timerId);
    }
    retryTimers.clear();
    inFlightImageLoads.clear();
  });

  // Lazily load each image as a base64 data URL via Tauri (avoids broken asset:// URLs)
  // untrack() the imageDataUrls read so this effect doesn't re-run every time an image loads —
  // only when imageResults() itself changes (new images added to the tool call).
  createEffect(() => {
    imageLoadAttempts();
    const imgs = imageResults();
    imgs.forEach((img) => {
      const path = String(img.path);
      if (untrack(() => imageDataUrls()[path])) return; // already loaded — skip without dep
      if (inFlightImageLoads.has(path)) return;

      inFlightImageLoads.add(path);
      invoke<string>("read_local_image", { path: img.path })
        .then((dataUrl) => {
          setImageDataUrls((prev) => ({ ...prev, [path]: dataUrl }));
        })
        .catch(() => {
          // Generated files can appear a moment after tool_result; retry with capped backoff.
          scheduleImageRetry(path);
        })
        .finally(() => {
          inFlightImageLoads.delete(path);
        });
    });
  });

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

          {/* Image generation failure card */}
          <Show when={props.tc.name === "generate_image" && props.tc.status === "error"}>
            {(() => {
              const rawData = props.tc.result as any;
              const report = rawData?.failure_report ?? rawData?.data?.failure_report;
              const stage = report?.stage ?? "Unknown";
              const message = report?.message ?? (typeof props.tc.result === "string" ? props.tc.result : "Image generation failed");
              const hint = report?.hint ?? "";
              return (
                <div class="tool-image-failure-card">
                  <div class="tool-image-failure-header">
                    <span class="tool-image-failure-icon">⚠️</span>
                    <span class="tool-image-failure-stage">{stage}</span>
                  </div>
                  <p class="tool-image-failure-message">{message}</p>
                  {hint && <p class="tool-image-failure-hint">{hint}</p>}
                  <button
                    class="tool-image-btn"
                    onClick={() => void appStore.sendMessage(`Generate image: ${String((props.tc.args as any)?.prompt ?? "")} (retry)`)}
                  >
                    ↺ Retry (new seed)
                  </button>
                </div>
              );
            })()}
          </Show>

          {/* Image generation results */}
          <Show when={imageResults().length > 0}>
            <div class="tool-image-results">
              <For each={imageResults()}>
                {(img) => (
                  <div class="tool-image-card">
                    <Show
                      when={imageDataUrls()[img.path]}
                      fallback={<div class="tool-image-loading">{imageLoadFailed(img.path) ? "Unable to load image." : "Loading image..."}</div>}
                    >
                      <img
                        src={imageDataUrls()[img.path]}
                        alt={`Generated ${img.style} image`}
                        class="tool-image-thumb"
                        loading="lazy"
                        onClick={() => {
                          const src = imageDataUrls()[img.path];
                          if (!src) return;
                          props.onOpenImage?.(src, {
                            alt: `Generated ${img.style} image`,
                            downloadName: imageFileNameFromPath(img.path, `kria-${img.style || "generated"}-image.jpg`),
                          });
                        }}
                      />
                    </Show>
                    <div class="tool-image-meta">
                      <span class="tool-image-badge">{img.style}</span>
                      <span class="tool-image-badge">{img.width}×{img.height}</span>
                      {img.quality && <span class="tool-image-badge">{img.quality}</span>}
                      {img.seed != null && <span class="tool-image-badge">seed: {img.seed}</span>}
                      {img.steps != null && <span class="tool-image-badge">{img.steps}s / {img.sampler ?? "euler"}</span>}
                      <span class="tool-image-badge tool-image-provenance">{img.provenance}</span>
                    </div>
                    <div class="tool-image-actions">
                      <Show when={imageDataUrls()[img.path]}>
                        <a
                          href={imageDataUrls()[img.path]}
                          download={`kria-${img.style || "generated"}-image.jpg`}
                          class="tool-image-btn"
                          title="Download image"
                        >
                          ↓ Download
                        </a>
                        <button
                          class="tool-image-btn"
                          title="Open preview"
                          onClick={() => {
                            const src = imageDataUrls()[img.path];
                            if (!src) return;
                            if (props.onOpenImage) {
                              props.onOpenImage(src, {
                                alt: `Generated ${img.style} image`,
                                downloadName: imageFileNameFromPath(img.path, `kria-${img.style || "generated"}-image.jpg`),
                              });
                              return;
                            }

                            const win = window.open("", "_blank");
                            if (!win) return;
                            win.document.write(`<img src="${src}" style="max-width:100%">`);
                            win.document.title = "KRIA Generated Image";
                          }}
                        >
                          ↗ Open
                        </button>
                      </Show>
                    </div>
                  </div>
                )}
              </For>
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
  const { copy: copyMsg, copied: msgCopied } = useCopyButton();
  const isUser = () => props.message.role === "user";
  const isAssistant = () => props.message.role === "assistant";

  const [inlineImageDataUrls, setInlineImageDataUrls] = createSignal<Record<string, string>>({});
  const [inlineImageLoadAttempts, setInlineImageLoadAttempts] = createSignal<Record<string, number>>({});
  const inlineInFlightImageLoads = new Set<string>();
  const inlineRetryTimers = new Map<string, number>();
  const [imagePreview, setImagePreview] = createSignal<{
    src: string;
    alt: string;
    downloadName: string;
  } | null>(null);

  const openImagePreview = (src: string, options?: { alt?: string; downloadName?: string }) => {
    setImagePreview({
      src,
      alt: options?.alt || "Generated image",
      downloadName: options?.downloadName || "kria-generated-image.jpg",
    });
  };

  const closeImagePreview = () => {
    setImagePreview(null);
  };

  createEffect(() => {
    const preview = imagePreview();
    if (!preview) return;

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        closeImagePreview();
      }
    };

    window.addEventListener("keydown", onKeyDown);
    onCleanup(() => {
      window.removeEventListener("keydown", onKeyDown);
    });
  });

  const htmlContent = createMemo(() => {
    if (!props.message.content || !isAssistant()) return "";
    return renderMarkdown(props.message.content);
  });

  const timeLabel = () =>
    new Date(props.message.timestamp).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

  const roleLabel = () => {
    switch (props.message.role) {
      case "assistant": return "KRIA";
      case "user": return "You";
      case "system": return "System";
      case "tool": return "Tool";
      default: return props.message.role;
    }
  };

  const inlineGeneratedImagePaths = createMemo(() => {
    if (!isAssistant()) return [] as string[];
    const contentPaths = props.message.content ? extractLocalImagePaths(props.message.content) : [];
    const toolTextPaths = extractGenerateImagePathsFromToolText(props.message.toolCalls);
    const fallbackPaths = new Set<string>([...contentPaths, ...toolTextPaths]);

    // Avoid duplicate cards when structured generate_image payload is already renderable.
    const structuredPaths = new Set<string>(extractStructuredGenerateImagePaths(props.message.toolCalls));
    for (const path of structuredPaths) {
      fallbackPaths.delete(path);
    }

    return Array.from(fallbackPaths);
  });

  const scheduleInlineImageRetry = (path: string) => {
    if (inlineRetryTimers.has(path)) return;
    const attempts = untrack(() => inlineImageLoadAttempts()[path] ?? 0);
    if (attempts >= 5) return;
    const delayMs = Math.min(300 * Math.pow(2, attempts), 3000);
    const timerId = window.setTimeout(() => {
      inlineRetryTimers.delete(path);
      setInlineImageLoadAttempts((prev) => ({
        ...prev,
        [path]: (prev[path] ?? 0) + 1,
      }));
    }, delayMs);
    inlineRetryTimers.set(path, timerId);
  };

  const inlineImageLoadFailed = (path: string) => {
    const attempts = inlineImageLoadAttempts()[path] ?? 0;
    return attempts >= 5 && !inlineImageDataUrls()[path];
  };

  createEffect(() => {
    inlineImageLoadAttempts();
    const paths = inlineGeneratedImagePaths();

    paths.forEach((path) => {
      if (untrack(() => inlineImageDataUrls()[path])) return;
      if (inlineInFlightImageLoads.has(path)) return;

      inlineInFlightImageLoads.add(path);
      invoke<string>("read_local_image", { path })
        .then((dataUrl) => {
          setInlineImageDataUrls((prev) => ({ ...prev, [path]: dataUrl }));
        })
        .catch(() => {
          scheduleInlineImageRetry(path);
        })
        .finally(() => {
          inlineInFlightImageLoads.delete(path);
        });
    });
  });

  onCleanup(() => {
    for (const timerId of inlineRetryTimers.values()) {
      window.clearTimeout(timerId);
    }
    inlineRetryTimers.clear();
    inlineInFlightImageLoads.clear();
  });

  return (
    <div class={`msg-row msg-row-${props.message.role}`}>
      {/* Avatar — only for assistant */}
      <Show when={isAssistant()}>
        <div class="msg-avatar msg-avatar-assistant" aria-hidden="true">K</div>
      </Show>

      <div class={`msg-bubble msg-bubble-${props.message.role}`}>
        {/* Bubble header */}
        <div class="msg-bubble-header">
          <span class="msg-label">{roleLabel()}</span>
          <span class="msg-time">{timeLabel()}</span>
          <Show when={props.message.content}>
            <button
              class={`msg-copy-btn ${msgCopied() ? "copied" : ""}`}
              onClick={() => copyMsg(props.message.content)}
              title="Copy"
            >
              {msgCopied() ? "✓" : "⎘"}
            </button>
          </Show>
        </div>

        {/* Attached image (user upload) */}
        <Show when={props.message.imageUrl}>
          <div class="msg-image-wrap">
            <img
              src={props.message.imageUrl}
              alt="Attached image"
              class="msg-image"
              onClick={() => window.open(props.message.imageUrl, "_blank")}
            />
          </div>
        </Show>

        <Show when={inlineGeneratedImagePaths().length > 0}>
          <div class="msg-inline-generated-images">
            <For each={inlineGeneratedImagePaths()}>
              {(path) => (
                <div class="msg-inline-generated-card">
                  <Show
                    when={inlineImageDataUrls()[path]}
                    fallback={
                      <div class="msg-inline-generated-loading">
                        {inlineImageLoadFailed(path) ? "Unable to load generated image." : "Loading generated image..."}
                      </div>
                    }
                  >
                    <img
                      src={inlineImageDataUrls()[path]}
                      alt="Generated image"
                      class="msg-inline-generated-image"
                      loading="lazy"
                      onClick={() => openImagePreview(inlineImageDataUrls()[path], {
                        alt: "Generated image",
                        downloadName: imageFileNameFromPath(path),
                      })}
                    />
                  </Show>
                  <Show when={inlineImageDataUrls()[path]}>
                    <div class="msg-inline-generated-actions">
                      <button
                        type="button"
                        class="msg-inline-generated-btn"
                        onClick={() => openImagePreview(inlineImageDataUrls()[path], {
                          alt: "Generated image",
                          downloadName: imageFileNameFromPath(path),
                        })}
                      >
                        Open
                      </button>
                      <a
                        href={inlineImageDataUrls()[path]}
                        download={imageFileNameFromPath(path)}
                        class="msg-inline-generated-download"
                      >
                        Download
                      </a>
                    </div>
                  </Show>
                </div>
              )}
            </For>
          </div>
        </Show>

        {/* Tool calls */}
        <Show when={props.message.toolCalls?.length}>
          <div class="msg-tool-calls">
            <For each={props.message.toolCalls}>
              {(tc) => <ToolCallBlock tc={tc} onOpenImage={openImagePreview} />}
            </For>
          </div>
        </Show>

        {/* Message text */}
        <Show when={props.message.content}>
          <div class="msg-text">
            {isAssistant()
              ? <div innerHTML={htmlContent()} />
              : <span>{props.message.content}</span>
            }
          </div>
        </Show>
      </div>

      {/* Avatar — only for user */}
      <Show when={isUser()}>
        <div class="msg-avatar msg-avatar-user" aria-hidden="true">U</div>
      </Show>

      <Show when={imagePreview()}>
        {(preview) => (
          <div class="msg-image-preview-overlay" onClick={closeImagePreview}>
            <div class="msg-image-preview-modal" onClick={(e) => e.stopPropagation()}>
              <div class="msg-image-preview-header">
                <button type="button" class="msg-image-preview-close" onClick={closeImagePreview}>
                  ×
                </button>
              </div>
              <img
                src={preview().src}
                alt={preview().alt}
                class="msg-image-preview-image"
              />
              <div class="msg-image-preview-actions">
                <a
                  href={preview().src}
                  download={preview().downloadName}
                  class="tool-image-btn"
                >
                  ↓ Download
                </a>
              </div>
            </div>
          </div>
        )}
      </Show>
    </div>
  );
};

export default MessageBubble;
