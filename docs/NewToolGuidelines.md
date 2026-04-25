# KRIA — New Tool Implementation Guidelines

> **Audience:** Anyone adding a new built-in tool to `kria-core`.
> **Scope:** Native Rust tools registered via `ToolRegistry`.
> For MCP-server-backed tools see [`NewMCPGuidelines.md`](./NewMCPGuidelines.md).

This document is the single source of truth for adding a tool. Follow every section in order. The tests in [`crates/kria-core/tests/test_chat_regression.rs`](../crates/kria-core/tests/test_chat_regression.rs) enforce most of these invariants — if you skip a step the regression suite will catch it.

---

## 0. The 10-second mental model

A tool in KRIA is **four** linked artifacts. If any one is missing the user-facing experience breaks (LLM hallucinates bash, "operation denied" appears, output is dropped, etc.):

| # | Artifact | File | What it does |
|---|---|---|---|
| 1 | **Handler** (`impl ToolHandler`) | `crates/kria-core/src/tools/<category>.rs` | The async function that does the work |
| 2 | **Registration** | same file, `pub fn register()` | Adds `ToolDef` + `Arc<Handler>` to the registry |
| 3 | **Policy classification** | `crates/kria-core/src/safety/policy.rs` | Risk tier (Green/Yellow/Red/Black) |
| 4 | **Router rule** (optional but recommended) | `crates/kria-core/src/agent/router.rs` | Regex that maps user prompts → this tool |

Plus:
- 5. **Regression test** in `crates/kria-core/tests/` (mandatory).

---

## 1. Naming rules (non-negotiable)

| Rule | Example |
|---|---|
| `snake_case` only. | ✅ `get_cpu_usage` ❌ `getCpuUsage` |
| Verb-first for actions, noun-first for queries. | ✅ `set_volume`, ✅ `cpu_info` ❌ `volume_set` |
| Read-only tools start with `get_`, `list_`, `read_`, `search_`, `check_`, `find_`. | `list_installed_packages` |
| Mutating tools use unambiguous verbs: `set_`, `create_`, `write_`, `delete_`, `kill_`, `install_`, `uninstall_`, `move_`, `rename_`. | `delete_file` |
| Sidecar/precognitive tools may use a domain prefix: `embeddings_`, `web_extract_`, `code_analyze_`. | `embeddings_generate` |
| MCP-bridged tools use server prefix `gw_`, `mcp_fs_`, `mcp_colab_`. | `gw_gmail_send` |
| **Never** rename an existing tool. Add a new one and deprecate the old. Renames break router cache, system prompt, and user voice aliases. | — |

---

## 2. The five required steps

### Step 1 — Implement the handler

Create or open the appropriate category file under [`crates/kria-core/src/tools/`](../crates/kria-core/src/tools). Categories:

```
system_info  file_ops      app_lifecycle  shell        internet
knowledge    system_config power          process      documents
communication interaction  disk           packages     scheduler
vision       desktop       developer      i18n         rag
proactive    precognitive  google_workspace             ...
```

If your tool doesn't fit, create a new category file and register it in [`registry.rs::build_registry_full`](../crates/kria-core/src/tools/registry.rs).

**Handler skeleton** (copy verbatim, adjust):

```rust
use crate::infra::ToolResult;
use crate::safety::RiskLevel;
use crate::tools::registry::{ToolDef, ToolHandler, ToolRegistry, ParamDef};
use async_trait::async_trait;
use std::sync::Arc;

struct GetThing;

#[async_trait]
impl ToolHandler for GetThing {
    async fn execute(&self, params: serde_json::Value) -> ToolResult {
        // 1. Validate params at the boundary, return ToolResult::err on bad input.
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => s.trim().to_string(),
            _ => return ToolResult::err("`id` is required (string)"),
        };

        // 2. Do the work. ALWAYS use tokio async APIs (no std::thread::sleep,
        //    no blocking std::fs in async context — use tokio::fs / tokio::process).
        let value = match fetch_thing(&id).await {
            Ok(v) => v,
            Err(e) => return ToolResult::err(format!("fetch failed: {e}")),
        };

        // 3. Return JSON whose top-level keys are the data the LLM should see.
        ToolResult::ok(serde_json::json!({
            "id":    id,
            "value": value,
        }))
    }
}
```

**Hard rules for handler bodies:**

- Return `ToolResult::ok(serde_json::Value)` on success, `ToolResult::err(String)` on failure.
- **Never `panic!`/`unwrap()` on user input.** Use `match`/`?` and return `ToolResult::err`.
- **Never block the async runtime.** Use `tokio::process::Command`, `tokio::fs`, `tokio::time::sleep`, `reqwest`, etc.
- **Never call `&self` fields inside `tokio::spawn`** — copy needed values to owned locals first (see `/memories/rust-tokio-notes.md`).
- Respect path policy: do NOT read/write inside `/etc`, `/boot`, `/root`, `/usr`, `/var`, `/proc`, `/sys`, `~/.ssh`, `~/.gnupg`. Default safe writable root is `/home/obaid`.
- Honor operation timeouts (set in [`config/default.toml`](../config/default.toml)). Tools longer than 30s must report milestone progress (see `kria-desktop` event bus).
- For destructive tools, accept a `dry_run: bool` param so callers can preview impact.

### Step 2 — Register the tool

In the same file's `pub fn register(reg: &ToolRegistry)`:

```rust
pub fn register(reg: &ToolRegistry) {
    let tools: Vec<(ToolDef, Arc<dyn ToolHandler>)> = vec![
        (
            ToolDef {
                name: "get_thing".into(),
                description: "Fetch a Thing by id. Returns id and value.".into(),
                category: "things".into(),
                parameters: vec![
                    ParamDef {
                        name: "id".into(),
                        param_type: "string".into(),
                        description: "Identifier of the Thing".into(),
                        required: true,
                        default: None,
                    },
                ],
                default_tier: RiskLevel::Green,
                min_tier: "lite",
            },
            Arc::new(GetThing),
        ),
    ];
    for (def, handler) in tools { reg.register(def, handler); }
}
```

**`ToolDef` field rules:**

| Field | Rule |
|---|---|
| `name` | Match what router emits and what policy lists. **One mismatch = bash hallucination.** |
| `description` | One sentence the LLM will see verbatim. Imperative, ≤140 chars. State return shape. |
| `category` | Reuse an existing category if possible. Categories drive UI grouping and `unregister_category`. |
| `parameters` | Schema becomes the JSON the LLM emits. Mark genuinely required params `required: true`. |
| `default_tier` | Must match what `policy.rs` will return (see Step 3). |
| `min_tier` | `"lite"` runs everywhere. `"standard"` needs ≥8GB RAM. `"performance"` needs GPU. `"high"` needs dedicated GPU + ≥16GB. |

If the registration is in a NEW file, also add `super::<file>::register(&reg);` to [`build_registry_full`](../crates/kria-core/src/tools/registry.rs).

### Step 3 — Classify in the safety policy

Open [`crates/kria-core/src/safety/policy.rs`](../crates/kria-core/src/safety/policy.rs) and add the tool name to **exactly one** static set:

| Tier | Set | Behavior | Use for |
|---|---|---|---|
| **Green** | `GREEN_ACTIONS` | Auto-execute, no approval | Read-only queries, status checks, search |
| **Yellow** | `YELLOW_ACTIONS` | Execute + notify the user (post-hoc), no blocking approval | Reversible writes (set_volume, write_file in safe paths, kill_process for own apps, gw_gmail_send) |
| **Red** | `RED_ACTIONS` | Block, request typed-PIN approval | Destructive (delete_file, shutdown, install_package, push to main) |
| **Black** | `BLACK_ACTIONS` | Always denied | `rm -rf /`, writing to `/etc`, reading `~/.ssh/id_rsa` |

> Per user preferences: **destructive actions ALWAYS require PIN, every time** — never cache approval. The HitlGateway flow handles this automatically once you set the tier to Red.

If your tool's tier depends on parameters (e.g. `write_file` is Green for sandbox path, Red for `/etc`), implement the path/parameter check inside `evaluate()` or `evaluate_with_modality_hint()` instead of putting it in a static set.

### Step 4 — Add a router rule (recommended)

In [`crates/kria-core/src/agent/router.rs`](../crates/kria-core/src/agent/router.rs), find `DIRECT_TOOL_RE` and add your regex tuple. **Order matters — first match wins.**

```rust
// In DIRECT_TOOL_RE:
(
    r"(?i)\b(get|fetch|show)\s+(the\s+)?thing\s+(id\s+)?\w+\b",
    "get_thing",
),
```

**Regex authoring rules:**

| Rule | Why |
|---|---|
| Use `(?i)` for case-insensitive matching. | Users speak/type in any case. |
| Anchor with `\b` word boundaries. | Avoid sub-word matches (`\binstall\b` not `install`). |
| Cover Hinglish synonyms. | E.g. `\bbattery\s+(check|level|kya|hai)\b`. |
| Include common phrasings: `get`, `show`, `list`, `view`, `check`, `what is`, `tell me`. | Wider coverage = fewer bash hallucinations. |
| **Place specific patterns BEFORE generic ones.** | `embeddings` rule must come before the broad `(text|message)\s+\w+` send_message rule, otherwise "make text embeddings" routes to send_message. |
| **Place patterns BEFORE `CONVERSATION_RE`** by being in `DIRECT_TOOL_RE`. | DIRECT_TOOL_RE is checked first — that's how "What is my CPU stats?" beats the generic "what is …" conversation pattern. |

**Verify after adding:**
1. The regex compiles (Rust will fail to start at runtime if not — `Lazy::new` panics).
2. Your prompt actually hits it: add a regression test (Step 5).
3. Earlier rules don't shadow yours: search the file for any rule whose regex is a superset of yours and reorder if needed.

### Step 5 — Add a regression test (mandatory)

Open [`crates/kria-core/tests/test_chat_regression.rs`](../crates/kria-core/tests/test_chat_regression.rs) and add:

```rust
#[test]
fn reg_things_get_thing_routes_correctly() {
    use kria_core::agent::router::{Intent, IntentRouter};
    let cases = [
        "get thing 42",
        "show me thing id abc-123",
        "fetch the thing xyz",
    ];
    for p in &cases {
        let r = IntentRouter::classify(p);
        match &r.intent {
            Intent::DirectTool(t) => assert_eq!(t, "get_thing", "'{p}' must route to get_thing, got {t}"),
            other => panic!("'{p}' must be DirectTool(get_thing), got {other:?}"),
        }
    }
}
```

Then verify the tool appears in the registry by adding its name to the `must_exist` array in `reg_tools_all_router_targets_exist_in_registry`. If the test fails with "registry is missing 'get_thing'" you forgot Step 2.

Run:
```bash
cargo test -p kria-core --test test_chat_regression
```

---

## 3. Output shape — what to return from `execute`

`ToolResult.data` becomes the LLM's view of your work. Bad shapes cause "Tool 'X' completed successfully." being shown instead of the real data (see [`commands.rs::summarize_tool_turn_for_history`](../crates/kria-desktop/src/commands.rs)).

**Recommended shapes:**

| Tool kind | Shape | Why |
|---|---|---|
| Single fact | `{ "key": "value", ... }` flat object | Summarizer picks first array-valued field or the JSON preview |
| List | `{ "total": N, "items": [...] }` or `{ "<resource>": [...] }` | Summarizer detects array → "Tool X returned N item(s)." |
| Aggregate | `{ "cpu": {...}, "memory": {...}, ... }` | Multi-section dashboards (see `check_system_health`) |
| Failure | `ToolResult::err("user-friendly reason")` | The summarizer surfaces this directly to the user; **never** include raw stack traces or internal IDs |
| Multi-step partial | `{ "step": 2, "of": 5, "status": "...", "data": {...} }` | Lets the desktop event bus emit milestone progress |

**Anti-patterns that WILL be summarized as "completed successfully" (data lost):**

- Returning `ToolResult::ok(serde_json::Value::Null)`.
- Returning `ToolResult::ok(json!({}))`.
- Returning `ToolResult::ok(json!({ "ok": true }))` with no other content.

If your tool genuinely has no data to return, use `ToolResult::ok_text("Done. <one-line context>.")`.

---

## 4. HITL (Human-In-The-Loop) integration

You generally do **not** call HITL directly — `AgentLoop` handles it based on the tier from `policy.rs`. Things to know:

- A Red-tier tool blocks the loop, surfaces an `ApprovalRequest` event, and waits for `ApprovalResponse::Approved` (with PIN), `Denied`, or `Timeout` (default 30s; install/uninstall 300s).
- The error message your handler returns in case of denial is **never** the source of "denied by user" — that string comes from `AgentLoop` itself only on `Denied`. If your tool causes "denied by user" to appear without the user denying, the cause is almost always your tool calling another tool internally that triggered HITL.
- For long Yellow operations, emit milestone events via the event bus so the UI can show progress (see [`crates/kria-core/src/infra/event_bus.rs`](../crates/kria-core/src/infra/event_bus.rs)).

---

## 5. Common failure patterns and how to avoid them

These are the patterns we already paid for in production. Each maps to a regression test in `test_chat_regression.rs`.

| ID | Symptom | Cause | Prevention |
|---|---|---|---|
| F1 | Inconsistent: tool works in one chat, refused in another | Tool registered conditionally (e.g. only when sidecar present) | Register unconditionally; if dependency missing, return `ToolResult::err("dependency X not available")` |
| F2 | "Search for notes.txt" → web_search instead of search_files | Generic web_search regex shadowed file-extension regex | Place file-extension router rule before web_search; assert in regression test |
| F3 | "Operation denied by user" but user never denied | Tool internally invoked a Red-tier sub-tool | Don't chain tools inside a handler — emit a `ComplexTask` plan instead |
| F4 | "What is my CPU stats?" → bash `top` block | Router missed prompt → LLM had no tool to call → hallucinated bash | Add router rule + REG-NO-BASH test |
| F5 | "Extract article from URL" → browser_search | No `web_extract_article` rule existed | Add rule; regression test |
| F6 | "Generate embeddings" → "no capability" | Router rule missing AND tool only registered in desktop runtime | Add to `build_default_registry` OR document sidecar-only with a routing test that pinpoints the gap |
| F7/F8 | Read-only "show X settings" → bash | No router rule | Add rule |
| F10 | List tool runs but UI shows "completed successfully." | `summarize_tool_turn_for_history` couldn't find array | Return `{ "<things>": [...] }` shape |
| F11 | Raw `mcp_call_failed { ... }` JSON shown to user | MCP error not sanitized | Catch in handler, return `ToolResult::err("Drive search is temporarily unavailable.")` |

---

## 6. Hardware tiers (`min_tier`)

Set conservatively. Users on low-end hardware see only `lite` tools.

| Tier | RAM (rough) | GPU | Examples |
|---|---|---|---|
| `lite` | ≥4GB | none | system_info, file_ops, http GETs |
| `standard` | ≥8GB | optional | git, packages, document parsing |
| `performance` | ≥16GB | small dGPU/iGPU | OCR, small LLM inference |
| `high` | ≥16GB | ≥6GB VRAM | Vision models, embeddings, audio preprocess |

Wrong tier = tool silently absent on user's machine. When in doubt, pick the **lower** tier and degrade gracefully.

---

## 7. Voice-first considerations (per user preferences)

KRIA is voice-first with Hinglish support. Your tool should:

| Concern | What to do |
|---|---|
| Latency target | Aim for <2s end-to-end. If slower, return early with `{ "status": "started", "tracking_id": "..." }` and emit milestones |
| TTS-friendly output | Return short human-readable summaries: `{ "spoken": "CPU at 23 percent", "data": { ... } }` if your tool feeds Voice |
| Hinglish prompts | Add at least one Hinglish router rule alongside English (e.g. `cpu ka haal`, `awaaz badhao`) |
| Confidential mode | Tools that surface private data must respect the chat-level `confidential` flag — if true, suppress the spoken summary; the loop already enforces this for you |
| Read-only on lock screen | Green tools work on lock screen by default. Yellow/Red tools must check lock state — handled by policy engine, no work needed in handler |

---

## 8. Testing checklist before opening a PR

```bash
# 1. Unit + integration in your category file
cargo test -p kria-core --lib --test test_<your_category>

# 2. Regression tests (chat-export floor)
cargo test -p kria-core --test test_chat_regression

# 3. Full workspace
cargo test --workspace

# 4. Desktop builds (UI surfaces tool name + description)
cargo build -p kria-desktop
```

Required green: your new test, all of `test_chat_regression`, no new failures in workspace test count vs. baseline.

---

## 9. Worked example: adding `get_thermal_state`

Suppose we want a new tool that returns CPU temperature.

**1. Handler** in [`system_info.rs`](../crates/kria-core/src/tools/system_info.rs):

```rust
struct GetThermalState;
#[async_trait]
impl ToolHandler for GetThermalState {
    async fn execute(&self, _p: serde_json::Value) -> ToolResult {
        let out = match tokio::process::Command::new("sensors")
            .arg("-j").output().await {
            Ok(o) if o.status.success() => o.stdout,
            Ok(o) => return ToolResult::err(
                format!("sensors failed: {}", String::from_utf8_lossy(&o.stderr).chars().take(200).collect::<String>())
            ),
            Err(e) => return ToolResult::err(format!("sensors not installed: {e}")),
        };
        match serde_json::from_slice::<serde_json::Value>(&out) {
            Ok(v) => ToolResult::ok(serde_json::json!({ "sensors": v })),
            Err(e) => ToolResult::err(format!("invalid sensors JSON: {e}")),
        }
    }
}
```

**2. Register** (in the same `tools` vec):

```rust
(ToolDef {
    name: "get_thermal_state".into(),
    description: "Read CPU/GPU temperature sensors. Returns JSON map by chip.".into(),
    category: "system_info".into(),
    parameters: vec![],
    default_tier: RiskLevel::Green,
    min_tier: "lite",
}, Arc::new(GetThermalState)),
```

**3. Policy** in [`policy.rs`](../crates/kria-core/src/safety/policy.rs) `GREEN_ACTIONS`:

```rust
"get_thermal_state",
```

**4. Router** in [`router.rs`](../crates/kria-core/src/agent/router.rs) `DIRECT_TOOL_RE` (near other system info rules):

```rust
(r"(?i)\b(temperature|thermal|temp|how\s+hot|garam)\b.{0,15}\b(cpu|gpu|system|laptop)?\b", "get_thermal_state"),
```

**5. Regression test** in `test_chat_regression.rs`:

```rust
#[test]
fn reg_thermal_state_routes_correctly() {
    use kria_core::agent::router::{Intent, IntentRouter};
    for p in ["What's my CPU temperature?", "system thermal state", "kitna garam hai laptop"] {
        match IntentRouter::classify(p).intent {
            Intent::DirectTool(t) => assert_eq!(t, "get_thermal_state"),
            other => panic!("{p} → {other:?}"),
        }
    }
}
```

Then add `"get_thermal_state"` to the `must_exist` array in `reg_tools_all_router_targets_exist_in_registry`.

**6. Run tests** → ship.

---

## 10. Quick reference card

```
┌────────────────────────────────────────────────────────────────────┐
│ Adding a tool to KRIA — five-step contract                         │
├────────────────────────────────────────────────────────────────────┤
│ 1. impl ToolHandler in crates/kria-core/src/tools/<category>.rs    │
│ 2. Register ToolDef in same file's pub fn register()               │
│ 3. Add name to GREEN/YELLOW/RED/BLACK in safety/policy.rs          │
│ 4. Add router regex to DIRECT_TOOL_RE in agent/router.rs           │
│ 5. Add REG-* test to tests/test_chat_regression.rs                 │
│                                                                    │
│ Verify:                                                            │
│   cargo test -p kria-core --test test_chat_regression              │
│   cargo test --workspace                                           │
│                                                                    │
│ Invariant (never violate):                                         │
│   Every name in router → must exist in registry → must have policy │
│   tier → must have a regression test                               │
└────────────────────────────────────────────────────────────────────┘
```
