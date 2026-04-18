import { Component, createSignal, onCleanup } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { Message } from "../stores/app";

interface Props {
  messages: () => Message[];
  sessionTitle?: () => string | null;
}

/** Format a timestamp to a readable date/time string. */
const formatTime = (ts: number) =>
  new Date(ts).toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });

/** Role label for export formats */
const roleLabel = (role: Message["role"]) => {
  switch (role) {
    case "user": return "User";
    case "assistant": return "Assistant";
    case "system": return "System";
    case "tool": return "Tool";
    default: return role;
  }
};

const toolResultText = (result: unknown): string => {
  if (result == null) return "";
  if (typeof result === "string") return result;
  try {
    return JSON.stringify(result, null, 2);
  } catch {
    return String(result);
  }
};

/** Build plain-text export content */
const buildText = (msgs: Message[], title: string): string => {
  const lines: string[] = [
    `KRIA Chat Export — ${title}`,
    `Exported: ${new Date().toLocaleString()}`,
    "═".repeat(60),
    "",
  ];
  for (const msg of msgs) {
    lines.push(`[${formatTime(msg.timestamp)}] ${roleLabel(msg.role).toUpperCase()}`);
    lines.push(msg.content);
    if (msg.toolCalls && msg.toolCalls.length > 0) {
      for (const tc of msg.toolCalls) {
        lines.push(`  ▸ Tool: ${tc.name} [${tc.status}]`);
        if (tc.result) {
          const resultText = toolResultText(tc.result);
          const preview = resultText.length > 200 ? resultText.slice(0, 200) + "…" : resultText;
          lines.push(`    Result: ${preview}`);
        }
      }
    }
    lines.push("");
  }
  return lines.join("\n");
};

/** Build Markdown export content */
const buildMarkdown = (msgs: Message[], title: string): string => {
  const lines: string[] = [
    `# KRIA Chat Export — ${title}`,
    ``,
    `> Exported: ${new Date().toLocaleString()}`,
    ``,
    `---`,
    ``,
  ];
  for (const msg of msgs) {
    const heading = `### ${roleLabel(msg.role)} · ${formatTime(msg.timestamp)}`;
    lines.push(heading);
    lines.push("");
    // Preserve code blocks from assistant messages as-is
    lines.push(msg.content);
    lines.push("");
    if (msg.toolCalls && msg.toolCalls.length > 0) {
      for (const tc of msg.toolCalls) {
        const icon = tc.status === "done" ? "✅" : tc.status === "error" ? "❌" : tc.status === "denied" ? "🚫" : "⏳";
        lines.push(`<details><summary>${icon} Tool: <code>${tc.name}</code></summary>`);
        lines.push("");
        lines.push("**Arguments:**");
        lines.push("```json");
        lines.push(JSON.stringify(tc.args, null, 2));
        lines.push("```");
        if (tc.result) {
          lines.push("");
          lines.push("**Result:**");
          lines.push("```");
          const resultText = toolResultText(tc.result);
          const preview = resultText.length > 500 ? resultText.slice(0, 500) + "\n…(truncated)" : resultText;
          lines.push(preview);
          lines.push("```");
        }
        lines.push("</details>");
        lines.push("");
      }
    }
    lines.push("---");
    lines.push("");
  }
  return lines.join("\n");
};



/** Build a safe filename from title + date */
const makeFilename = (title: string, ext: string): string => {
  const safe = title.replace(/[^a-z0-9]/gi, "_").replace(/_+/g, "_").slice(0, 40);
  const date = new Date().toISOString().slice(0, 10);
  return `kria_${safe}_${date}.${ext}`;
};

/** Build the HTML string used for PDF generation via print dialog */
const buildHtml = (msgs: Message[], title: string): string => {
  const escHtml = (s: string) =>
    s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

  const rows = msgs.map((msg) => {
    const roleClass = `role-${msg.role}`;
    let toolHtml = "";
    if (msg.toolCalls && msg.toolCalls.length > 0) {
      const items = msg.toolCalls
        .map((tc) => {
          const resultText = toolResultText(tc.result);
          const result = tc.result
            ? `<div class="tc-result"><strong>Result:</strong><pre>${escHtml(
                resultText.length > 300 ? resultText.slice(0, 300) + "…" : resultText
              )}</pre></div>`
            : "";
          return `<div class="tc"><span class="tc-name">${escHtml(tc.name)}</span> <span class="tc-status">[${tc.status}]</span>${result}</div>`;
        })
        .join("");
      toolHtml = `<div class="tool-calls">${items}</div>`;
    }
    return `
      <div class="msg ${roleClass}">
        <div class="msg-header">
          <span class="msg-role">${escHtml(roleLabel(msg.role))}</span>
          <span class="msg-time">${formatTime(msg.timestamp)}</span>
        </div>
        <div class="msg-body">${escHtml(msg.content)}</div>
        ${toolHtml}
      </div>`;
  }).join("\n");

  return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>KRIA Chat — ${escHtml(title)}</title>
<style>
  body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Arial, sans-serif;
         color: #1a1a1a; background: #fff; margin: 0; padding: 24px 32px; }
  h1 { font-size: 20px; margin-bottom: 4px; }
  .meta { font-size: 12px; color: #888; margin-bottom: 24px; }
  .msg { margin-bottom: 16px; padding: 12px 16px; border-radius: 8px; break-inside: avoid; }
  .msg-header { display: flex; justify-content: space-between; margin-bottom: 6px; font-size: 11px; font-weight: 700; text-transform: uppercase; letter-spacing: 0.5px; }
  .msg-time { font-weight: 400; color: #888; text-transform: none; letter-spacing: 0; }
  .msg-body { font-size: 14px; line-height: 1.6; white-space: pre-wrap; word-break: break-word; }
  .role-user { background: #e8f0fe; border-left: 4px solid #4285f4; }
  .role-assistant { background: #f8f9fa; border-left: 4px solid #34a853; }
  .role-system { background: #fce8e6; border-left: 4px solid #ea4335; }
  .role-tool { background: #fef9e7; border-left: 4px solid #fbbc04; }
  .role-user .msg-header { color: #1967d2; }
  .role-assistant .msg-header { color: #1e8e3e; }
  .tool-calls { margin-top: 8px; font-size: 12px; }
  .tc { padding: 4px 8px; margin-top: 4px; background: rgba(0,0,0,0.04); border-radius: 4px; }
  .tc-name { font-family: monospace; font-weight: 700; }
  .tc-status { color: #888; }
  .tc-result pre { margin: 4px 0 0; white-space: pre-wrap; word-break: break-word; font-size: 11px; color: #555; }
  @media print { body { padding: 0; } }
</style>
</head>
<body>
<h1>KRIA Chat Export — ${escHtml(title)}</h1>
<div class="meta">Exported ${new Date().toLocaleString()} · ${msgs.length} messages</div>
${rows}
</body>
</html>`;
};

const ExportDropdown: Component<Props> = (props) => {
  const [open, setOpen] = createSignal(false);
  const [exporting, setExporting] = createSignal<"text" | "md" | "pdf" | null>(null);

  const title = () => {
    const raw = props.sessionTitle?.() ?? null;
    return raw && raw.trim() ? raw : "Chat";
  };

  // Close on outside click
  const handleOutside = (e: MouseEvent) => {
    const target = e.target as HTMLElement;
    if (!target.closest(".export-dropdown")) setOpen(false);
  };

  document.addEventListener("mousedown", handleOutside);
  onCleanup(() => document.removeEventListener("mousedown", handleOutside));

  const exportText = async () => {
    const msgs = props.messages();
    if (msgs.length === 0 || exporting() !== null) return;
    setExporting("text");
    setOpen(false);
    try {
      await invoke<string | null>("save_export_file", {
        content: buildText(msgs, title()),
        defaultName: makeFilename(title(), "txt"),
        filterName: "Text Files",
        extensions: ["txt"],
      });
    } catch (e) {
      console.error("Export text failed:", e);
    } finally {
      setExporting(null);
    }
  };

  const exportMarkdown = async () => {
    const msgs = props.messages();
    if (msgs.length === 0 || exporting() !== null) return;
    setExporting("md");
    setOpen(false);
    try {
      await invoke<string | null>("save_export_file", {
        content: buildMarkdown(msgs, title()),
        defaultName: makeFilename(title(), "md"),
        filterName: "Markdown Files",
        extensions: ["md"],
      });
    } catch (e) {
      console.error("Export markdown failed:", e);
    } finally {
      setExporting(null);
    }
  };

  const exportPdf = async () => {
    const msgs = props.messages();
    if (msgs.length === 0 || exporting() !== null) return;
    setExporting("pdf");
    setOpen(false);
    try {
      await invoke("open_html_for_print", {
        html: buildHtml(msgs, title()),
        filename: makeFilename(title(), "html"),
      });
    } catch (e) {
      console.error("Export PDF failed:", e);
    } finally {
      setExporting(null);
    }
  };

  return (
    <div class="export-dropdown">
      <button
        class="export-btn"
        title="Export chat"
        disabled={exporting() !== null}
        onClick={() => setOpen((v) => !v)}
      >
        {exporting() !== null ? "Exporting…" : "⬇ Export"}
      </button>
      <div class={`export-menu ${open() ? "open" : ""}`}>
        <button class="export-menu-item" disabled={exporting() !== null} onClick={exportText}>
          <span class="export-icon">📄</span>
          <span>
            <strong>Plain Text</strong>
            <small>.txt — simple readable format</small>
          </span>
        </button>
        <button class="export-menu-item" disabled={exporting() !== null} onClick={exportMarkdown}>
          <span class="export-icon">📝</span>
          <span>
            <strong>Markdown</strong>
            <small>.md — preserves formatting</small>
          </span>
        </button>
        <button class="export-menu-item" disabled={exporting() !== null} onClick={exportPdf}>
          <span class="export-icon">📑</span>
          <span>
            <strong>PDF</strong>
            <small>Print-to-PDF via browser</small>
          </span>
        </button>
      </div>
    </div>
  );
};

export default ExportDropdown;
