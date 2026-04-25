# KRIA — New MCP Server Integration Guidelines

> **Audience:** Anyone adding a new **Model Context Protocol (MCP) server** to KRIA — either by enabling an existing public MCP server (filesystem, Google Workspace, Colab, Slack, etc.) or by writing a new one.
> **Scope:** External processes that expose tools over JSON-RPC and are bridged into the `ToolRegistry` at runtime.
> For native Rust tools see [`NewToolGuidelines.md`](./NewToolGuidelines.md).

This document mirrors the structure of `NewToolGuidelines.md`. Read both — MCP integration is "native tool registration **plus** an external process you don't fully control."

---

## 0. The 10-second mental model

```
┌──────────────────────┐  stdio   ┌────────────────────┐
│   MCP server proc    │ <──JSON─►│   McpClient        │
│  (npx / uvx / bin)   │  -RPC    │  (kria-core)       │
└──────────────────────┘          └─────────┬──────────┘
                                            │ for each remote tool:
                                            ▼
                                  ┌────────────────────┐
                                  │  McpToolHandler    │
                                  │  (impl ToolHandler)│
                                  └─────────┬──────────┘
                                            │ register
                                            ▼
                                  ┌────────────────────┐
                                  │   ToolRegistry     │
                                  └────────────────────┘
                                            │ same path as native tools
                                            ▼
                                  Router → Policy → AgentLoop
```

Key files (read these first):

| File | Role |
|---|---|
| [`config/mcp_servers.json`](../config/mcp_servers.json) | Declarative list of servers KRIA will start |
| [`crates/kria-core/src/config.rs`](../crates/kria-core/src/config.rs) (`McpServerConfig`) | Schema of one server entry |
| [`crates/kria-core/src/mcp/server_manager.rs`](../crates/kria-core/src/mcp/server_manager.rs) | Lifecycle (start/stop/health/restart) |
| [`crates/kria-core/src/mcp/client.rs`](../crates/kria-core/src/mcp/client.rs) | JSON-RPC stdio client |
| [`crates/kria-core/src/mcp/protocol.rs`](../crates/kria-core/src/mcp/protocol.rs) | MCP protocol structs |
| [`crates/kria-core/src/mcp/tool_bridge.rs`](../crates/kria-core/src/mcp/tool_bridge.rs) | `McpToolHandler` adapter |
| [`crates/kria-core/src/mcp/payload_shaper.rs`](../crates/kria-core/src/mcp/payload_shaper.rs) | Reduces large MCP responses to LLM-friendly shape |
| [`crates/kria-core/src/mcp/capability_discovery.rs`](../crates/kria-core/src/mcp/capability_discovery.rs) | Discovers tool list at startup |

---

## 1. The two integration paths

### Path A — Add an existing public MCP server (most common)

You only edit JSON. No Rust changes needed unless the server has special requirements (auth, payload quirks, name collisions).

### Path B — Write your own MCP server

Implement the JSON-RPC protocol yourself in any language (Python, TypeScript, Rust, Go), then add it via Path A.

This document covers Path A in depth (Section 2-7) and Path B at a glance (Section 8).

---

## 2. Path A — registering an existing MCP server

### Step 1 — Declare the server in `config/mcp_servers.json`

Append a new entry under `"servers"`:

```jsonc
{
  "name":         "myserver",                       // unique, [a-z0-9_-]
  "transport":    "stdio",                          // only stdio is supported today
  "command":      "uvx",                            // launcher binary on $PATH
  "args":         ["my-mcp-package", "--flag"],     // argv after the binary
  "env": {
    "MY_API_TOKEN": "${env:MY_API_TOKEN}"           // env-var passthrough
  },
  "enabled":      true,
  "trust_level":  "YELLOW",                         // GREEN | YELLOW | RED | BLACK
  "tool_overrides": {
    "deleteAnything": "RED",                        // per-tool tier overrides
    "writeAnything":  "YELLOW"
  }
}
```

**Field rules:**

| Field | Rule |
|---|---|
| `name` | Becomes the **prefix** for every discovered tool (`myserver_<remoteTool>`). Choose carefully — renames break router rules and user voice aliases. |
| `command` / `args` | Must run **non-interactively** and speak JSON-RPC on stdin/stdout. Any prompt-for-input behavior will hang KRIA's startup. |
| `env` | Inherits KRIA's environment plus these overrides. **Never** put secrets here in committed JSON; use `${env:NAME}` and document the env var in `setup.sh`. |
| `enabled` | Set `false` to keep the entry but skip launch. Used for opt-in servers. |
| `trust_level` | Default tier for **all** tools from this server. Read-only servers → `GREEN`. Mutating servers → `YELLOW`. Servers that can delete user data → keep `YELLOW` and use `tool_overrides` to escalate the destructive tools to `RED`. |
| `tool_overrides` | Map remote tool name (as the server reports it, **before** the prefix) → tier. Per [`policy.rs`], use `RED` for any irreversible action (delete*, drop*, push*, send*). |

**Worked examples** from the live config:

```jsonc
// Read-only Filesystem MCP (allowed roots passed as args).
{ "name": "fs", "command": "mcp-server-filesystem",
  "args": ["/home", "/media", "/tmp"],
  "trust_level": "YELLOW",
  "tool_overrides": { "move_file": "RED", "write_file": "YELLOW", "edit_file": "YELLOW" }
},

// Google Workspace MCP — auth via dedicated config dir.
{ "name": "gworkspace", "command": "npx",
  "args": ["-y", "google-workspace-mcp", "serve"],
  "env":  { "GOOGLE_MCP_CONFIG_DIR": "/home/obaid/.google-mcp" },
  "trust_level": "YELLOW",
  "tool_overrides": {
    "deleteGmailMessage":  "RED",
    "deleteCalendarEvent": "RED",
    "deleteFile":          "RED"
  }
}
```

### Step 2 — Verify discovery

Restart KRIA and check the logs:

```
[MCP] start_all: 3 configured, 3 enabled — launching in parallel
[MCP] starting server 'myserver' (command='uvx' args=["my-mcp-package", "--flag"])
[MCP] server 'myserver' started — 7 tool(s) discovered
```

If you see `0 tool(s) discovered`, the server is up but didn't return tools — see Section 5 (troubleshooting). If startup fails twice, the server is auto-disabled and the loop continues without those tools.

### Step 3 — Confirm the tools are in the registry

After startup the registry will contain `myserver_tool1`, `myserver_tool2`, etc. Test:

```bash
cargo test -p kria-core --test test_chat_regression -- --nocapture
```

Or run an ad-hoc check from a Rust test:

```rust
let reg = build_default_registry();
// (after McpServerManager::start_all has run on it)
assert!(reg.get_def("myserver_search").is_some());
```

### Step 4 — Add policy entries IF you want non-default tiering

The `trust_level` + `tool_overrides` from JSON are applied by `McpServerManager` when registering each tool. **You usually don't need to touch [`policy.rs`].** Only add a name there if:
- The bridge couldn't infer the tier (rare — check logs).
- You want a stronger guarantee than JSON config.
- The MCP tool's policy depends on its parameters (e.g. `fs_write` is Yellow for `/home/obaid` but Red for `/etc`). Add a parameter-aware branch in `evaluate()` keyed on the prefixed tool name.

### Step 5 — Add router rules for the most common prompts

If users will say things like "search my Drive for X", add to [`agent/router.rs`] `DIRECT_TOOL_RE`:

```rust
(r"(?i)\b(search|find)\s+(my\s+)?(google\s+)?drive\s+for\b", "gw_drive_search"),
```

Same rules as native tools (see `NewToolGuidelines.md` §2 step 4):
- **Specific patterns first.** Place MCP-specific rules above generic `web_search`/`open_url` rules.
- Cover Hinglish synonyms.
- Add a regression test (Section 4).

### Step 6 — Sanitize errors (REG-F11)

MCP servers return raw JSON-RPC errors that look like:

```
mcp_call_failed: {"code": -32603, "message": "Drive: insufficient scope", "data": {...}}
```

The bridge in [`tool_bridge.rs`] surfaces this as the `error` field of `ToolResult`. **Never let this raw text reach the user.** Two options:

1. **Recommended:** Wrap the MCP tool with a thin native tool that calls the bridge and rewrites errors:

   ```rust
   // crates/kria-core/src/tools/google_workspace.rs
   match mcp_handler.execute(params).await {
       r if r.success => r,
       r => ToolResult::err(humanize_gw_error(r.error.as_deref().unwrap_or(""))),
   }
   ```

2. **Quick fix:** Centralize sanitization in `summarize_tool_turn_for_history` ([`crates/kria-desktop/src/commands.rs`](../crates/kria-desktop/src/commands.rs)) — strip `mcp_call_failed:` prefix and JSON tail before showing.

Add a regression test:

```rust
#[test]
fn reg_myserver_errors_are_user_friendly() {
    // craft a failing call, assert the surfaced message has no JSON braces / no -32603
}
```

### Step 7 — Document the server

Add a short section to [`docs/HOW_TO_RUN.md`](./HOW_TO_RUN.md):
- Required env vars / auth setup.
- Install command (`npm i -g …` or `uv tool install …`).
- One sentence on what users can ask for.

---

## 3. Tool name prefixing & collisions

`McpServerManager` registers each remote tool as `<server_name>_<remote_tool_name>` to prevent collisions across servers. Examples:

| Server | Remote name | Registered as |
|---|---|---|
| `fs` | `read_file` | `mcp_fs_read_file` (or `fs_read_file` per current server_manager logic — check there) |
| `gworkspace` | `searchGmail` | `gw_searchGmail` (then snake-cased to `gw_gmail_search` if a contract wrapper exists) |
| `colab-mcp` | `execute_cell` | `mcp_colab-mcp_execute_cell` |

**Collision handling:**
- If two servers expose the same remote name, both register fine (different prefixes).
- If a remote tool collides with a **native** tool name, the **native tool wins** (registry insertion order in `build_registry_full` precedes MCP startup). Watch the startup log for `tool 'X' already registered, skipping`.

**Naming hygiene:**
- Don't pick `name: "fs"` if you want filesystem behavior different from the existing `fs` server — pick a different name.
- If the remote server uses camelCase, leave it as-is in the registry (the bridge does not auto-snake_case). The router and policy will need the camelCase form: `gw_searchGmail`. Prefer servers that already use snake_case.

---

## 4. Required tests for any new MCP server

Add to [`crates/kria-core/tests/test_chat_regression.rs`](../crates/kria-core/tests/test_chat_regression.rs):

```rust
// 1. Routing — user prompts hit the MCP tools
#[test]
fn reg_myserver_routes_correctly() {
    for (prompt, tool) in [
        ("search my drive for budget",   "gw_drive_search"),
        ("create a colab notebook foo",  "gw_drive_create"),
    ] {
        match IntentRouter::classify(prompt).intent {
            Intent::DirectTool(t) => assert_eq!(t, tool, "{prompt}"),
            other => panic!("{prompt} → {other:?}"),
        }
    }
}

// 2. Policy — tiers behave as configured
#[test]
fn reg_myserver_policy_tiers_match_config() {
    let p = PolicyEngine::new();
    let read = p.evaluate("myserver_search", &json!({}));
    assert!(matches!(read.risk_level, RiskLevel::Green | RiskLevel::Yellow));
    let destroy = p.evaluate("myserver_delete", &json!({}));
    assert_eq!(destroy.risk_level, RiskLevel::Red);
}
```

For Path B (custom server), add an end-to-end smoke test that starts the server in a tokio subprocess, lists tools, and calls one:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn smoke_myserver_lists_tools() {
    let cfg = McpServerConfig { /* ... */ };
    let mut mgr = McpServerManager::new(vec![cfg]);
    let reg = ToolRegistry::new();
    mgr.start_all(&reg).await;
    assert!(reg.get_def("myserver_ping").is_some(), "myserver_ping not registered");
}
```

---

## 5. Lifecycle, health, and failure modes

`McpServerManager` runs the following loop for every enabled server:

1. **Start** with up to 2 retries; if both fail, mark the server disabled for this session (logged but non-fatal).
2. **Discover** tools via the MCP `tools/list` call.
3. **Register** each tool with the right tier (server `trust_level` ⊕ per-tool `tool_overrides`).
4. **Health-check** with `ping` periodically. After `MAX_PING_FAILURES = 3` consecutive failures, the server is restarted; tools are unregistered first via `unregister_category(server_name)` and re-registered after restart.
5. **Reconcile** when config reloads: starts new servers, stops removed ones, restarts changed ones (`McpReconcileReport`).

**Implications for tool authors:**

| Concern | What this means |
|---|---|
| First-run latency | Tools may not be registered for ~1-2s after KRIA starts. The router cache (`RouterCacheEvent::ToolsChanged`) is invalidated when tools change so prompts retry against the new set. |
| Server crash mid-call | The in-flight call returns `ToolResult::err`; the agent loop shows it as a tool failure and the LLM may retry. **Make your handler idempotent if possible.** |
| Server hang | The bridge enforces a default 30s timeout per call (180s for `git push`-style calls per user prefs). Long operations must use a different async pattern (job ID + poll). |
| Restart during call | Calls that were in-flight when restart happens fail-fast; UI shows "tool X is restarting, retry shortly." |

---

## 6. Authentication patterns

| Auth model | Pattern | Where secrets live |
|---|---|---|
| **API key in env var** | Reference in `env`: `"MY_TOKEN": "${env:MY_TOKEN}"` | User's shell env; documented in `setup.sh` |
| **OAuth (Google, Slack, …)** | Server stores its own credential file in a fixed dir; pass that dir via `env` | `~/.google-mcp`, `~/.config/slack-mcp`, etc. **Per user prefs: tokens MUST be encrypted on disk.** |
| **Per-account injection** | See `inject_gworkspace_account` in [`tool_bridge.rs`] for the pattern: bridge auto-adds an `account` param so the LLM doesn't have to. Add similar logic for any multi-account server. | Env var `KRIA_<SERVER>_ACCOUNT` |
| **PIN-gated** | Set tier to `RED` in `tool_overrides`; HitlGateway will require typed PIN every call (per user prefs no caching). | n/a |

**Never:**
- Commit credentials in `mcp_servers.json`.
- Log raw token values; the bridge already redacts known patterns — keep your additions consistent.
- Use plaintext storage for OAuth refresh tokens.

---

## 7. Payload shaping and large responses

MCP servers often return huge JSON (full Gmail thread, Drive file with binary preview, etc.). The bridge passes responses through [`payload_shaper.rs::shape_for_llm`](../crates/kria-core/src/mcp/payload_shaper.rs), which:

- **Keeps** identity/meta fields (`id`, `name`, `subject`, `from`, `date`, `mimeType`, `count`, …).
- **Drops** large body fields (`raw`, `body`, `html`, `attachments`, `payload`, …).
- **Truncates** long strings with a `head + "…N chars elided…" + tail` pattern.
- **Caps** array length to fit the LLM context budget; appends `__shape` metadata so the LLM knows more data exists.

**For your new server:**

- If the server returns a field that should NOT be elided (e.g. a field named `body` that's actually a 50-char message), add it to `KEEP_KEYS` in `payload_shaper.rs`.
- If the server returns a noisy field that should always be dropped, add it to `DROP_KEYS`.
- For binary/base64 content, **never** include it in the JSON the LLM sees. Instead, save to disk and return `{ "saved_to": "/tmp/...", "bytes": N, "mime": "..." }`.

---

## 8. Path B — Writing your own MCP server (overview)

If no public MCP server fits your need, write one. KRIA only requires that your process speak the standard MCP protocol over stdio.

**Minimum protocol:**

| Method | Purpose |
|---|---|
| `initialize` | Handshake, declare server name + version |
| `tools/list` | Return all tools with JSON Schema for inputs |
| `tools/call` | Execute a tool, return `content[]` |
| `ping` | Lightweight liveness check (KRIA uses this for health) |

**Recommended stack:**
- **Python:** [`mcp`](https://pypi.org/project/mcp/) SDK from Anthropic — minimal boilerplate.
- **TypeScript:** [`@modelcontextprotocol/sdk`](https://www.npmjs.com/package/@modelcontextprotocol/sdk).
- **Rust:** Implement against the schemas in [`crates/kria-core/src/mcp/protocol.rs`](../crates/kria-core/src/mcp/protocol.rs); good fit if your tools are CPU-bound.

**Server-author rules:**

| Rule | Why |
|---|---|
| Stdout = JSON-RPC ONLY. All logs to stderr. | One stray `print()` corrupts the JSON-RPC stream and the server is killed. |
| Read each request fully before replying. | Pipelined clients break otherwise. |
| Reply to `ping` in <100ms. | Health monitor declares unhealthy after 3 missed pings. |
| Return `isError: true` on tool failure with a friendly `content[].text`. | This text is what reaches the user via the bridge. |
| Tool names: snake_case, ≤40 chars. | The bridge prefixes them; long names blow past LLM context. |
| Idempotent semantics where possible. | Restarts will retry. |
| Never block stdin reads. Use async I/O. | Long ops must run in a task and stream progress. |
| Validate inputs against your declared schema. | The LLM will eventually emit malformed args. |
| Include a SemVer in `initialize` response. | Future-proofs schema changes. |

**Testing your custom server:**

1. Run it standalone and pipe a hand-written `tools/list` request:
   ```bash
   echo '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' | python -m my_mcp
   ```
2. Add to `config/mcp_servers.json` with `enabled: true`.
3. Run KRIA, watch for `started — N tool(s) discovered`.
4. Add the regression tests from Section 4.

---

## 9. Common failure patterns and how to avoid them

| ID | Symptom | Cause | Prevention |
|---|---|---|---|
| MCP-1 | `0 tool(s) discovered` | Server returned no tools, OR your `tools/list` response is malformed | Validate the response shape against [`protocol.rs`]. Test the server standalone first. |
| MCP-2 | Server starts then dies in <5s | `print()` to stdout corrupted JSON-RPC | Route ALL logs to stderr. Use the SDK's logger. |
| MCP-3 | `mcp_call_failed: {...}` shown to user | Raw bridge error not sanitized | Wrap with native tool (Section 2 Step 6) OR sanitize in `summarize_tool_turn_for_history` |
| MCP-4 | Tools work first chat, gone after some time | Server crashed; auto-restart not happening (>3 ping failures) | Check logs; ensure your server replies to `ping` quickly |
| MCP-5 | "Operation denied by user" without user denial | Tool registered as RED but server returned generic auth error → loop interpreted as denial | Sanitize errors; ensure tier is correct in `trust_level` |
| MCP-6 | Tool exists but router never picks it | No router rule; LLM has 60+ tools and missed yours | Add `DIRECT_TOOL_RE` rule + REG-* test |
| MCP-7 | Massive responses cause LLM to loop | Payload shaper isn't keeping/dropping the right fields | Add field names to `KEEP_KEYS` / `DROP_KEYS` in `payload_shaper.rs` |
| MCP-8 | Two servers expose `read_file`, wrong one runs | Prefix collision or registration order | Use unique server names; rely on the prefix and update router rules to use the prefixed form |
| MCP-9 | Long-running call times out at 30s | MCP server holds connection open for the duration | Implement async job pattern in the server: return `{ "job_id": "..." }` immediately and a separate `get_job` tool |
| MCP-10 | OAuth token expired silently | Server returned the failure but user-facing error was generic | Map known auth errors to a clear message and a hint to re-auth |

---

## 10. Security checklist

Before enabling a new MCP server in production:

- [ ] Server runs as the user (not root). Verify with `ps aux | grep <server>`.
- [ ] Allowed paths are explicit (filesystem-style servers must take roots as args, not `/`).
- [ ] No secrets in `mcp_servers.json`; all secrets via env vars or external credential dirs.
- [ ] Credential dirs are mode `0700` and owned by the user.
- [ ] Destructive tools are tagged `RED` so they require typed PIN every call.
- [ ] Network-bound servers use TLS for any remote calls.
- [ ] Server source is pinned (specific version / commit), not `latest`. Per user prefs: only signed/trusted plugins.
- [ ] You've tested behavior when the server is offline — KRIA must fail fast with a clear message, never hang.
- [ ] No logs include credential values; verify with `grep -E '(token|password|secret)' logs/*.log` after a test run.
- [ ] Per user prefs: any cloud fallback path requires explicit one-time approval — handle in the wrapper tool.

---

## 11. Quick reference card

```
┌────────────────────────────────────────────────────────────────────┐
│ Adding an MCP server to KRIA — six-step contract                   │
├────────────────────────────────────────────────────────────────────┤
│ 1. Add entry to config/mcp_servers.json (name, command, env)       │
│ 2. Set trust_level + tool_overrides for risky tools                │
│ 3. Restart KRIA, confirm 'N tool(s) discovered' in logs            │
│ 4. (optional) Add router regexes in agent/router.rs                │
│ 5. (optional) Wrap with native tool to sanitize errors             │
│ 6. Add REG-* tests to tests/test_chat_regression.rs                │
│                                                                    │
│ Verify:                                                            │
│   cargo test -p kria-core --test test_chat_regression              │
│   cargo test --workspace                                           │
│   tail -f logs/kria.log | grep '\[MCP\]'                           │
│                                                                    │
│ Invariant (never violate):                                         │
│   No raw mcp_call_failed JSON ever reaches the user.               │
│   Destructive remote tools are RED tier with PIN every call.       │
│   Server stdout = JSON-RPC only; logs go to stderr.                │
└────────────────────────────────────────────────────────────────────┘
```

---

## 12. Cross-references

- Native tool guidelines: [`NewToolGuidelines.md`](./NewToolGuidelines.md)
- Risk tier rules: [`crates/kria-core/src/safety/policy.rs`](../crates/kria-core/src/safety/policy.rs)
- Live MCP config: [`config/mcp_servers.json`](../config/mcp_servers.json)
- Hardware orchestration: [`docs/HARDWARE_ORCHESTRATION.md`](./HARDWARE_ORCHESTRATION.md)
- How to run + auth setup: [`docs/HOW_TO_RUN.md`](./HOW_TO_RUN.md)
- Regression test floor: [`crates/kria-core/tests/test_chat_regression.rs`](../crates/kria-core/tests/test_chat_regression.rs)
