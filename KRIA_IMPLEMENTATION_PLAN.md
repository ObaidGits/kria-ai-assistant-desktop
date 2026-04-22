# KRIA Implementation Plan and Checklist

Document: KRIA_IMPLEMENTATION_PLAN.md  
Source of truth: KRIA_REQUIREMENTS.md  
Prepared for: Obaid  
Date: 2026-04-19  
Status: Execution-ready roadmap

---

## 1. Planning Principles

- Voice-first is primary; text is fallback and support channel.
- Safety is mandatory: destructive actions always require typed PIN.
- Lock-screen default is read-only; elevation requires typed PIN.
- Reliability target is 95 percent with simple command response around 1-2 seconds.
- Every phase must ship with tests (unit + integration + API/E2E where relevant).
- No phase is done until exit criteria and test gate pass.

---

## 2. Phase Overview

| Phase | Name | Main Outcome | Est. Duration | Gate |
|---|---|---|---|---|
| 0 | Foundation and Traceability | Requirement mapping, config contracts, test scaffolding | 3-5 days | Test skeletons merged |
| 1 | Core Orchestration and Safety Enforcement | Agent loop is the only execution path with policy checks | 1-2 weeks | Tool execution path fully gated |
| 2 | Voice-First Runtime | Wake-word, barge-in, VAD-based listen-until-stop, text fallback | 1-2 weeks | End-to-end voice loop stable |
| 3 | Lock Screen, PIN, Approval UX | Read-only lock screen and secure elevation model | 1 week | Security acceptance tests pass |
| 4 | Must-Work Status Commands | Critical commands never-fail baseline | 4-6 days | 95 percent pass over soak tests |
| 5 | Messaging Workflows | Gmail and WhatsApp flow with confirmations | 1-2 weeks | Human-like, safe send behavior |
| 6 | Software and Download Controls | Install/uninstall/download governance and source policy | 1 week | Destructive and provenance policy verified |
| 7 | Git and Developer Controls | Explicit push approval, branch protection, pre-push checks | 1 week | Git safety and quality checks pass |
| 8 | Notes, Tasks, Scheduling, Reminders | Hybrid capture, tagging, escalation, scheduling NLP | 1-2 weeks | Task/reminder reliability gate |
| 9 | Concurrency, Proactivity, DND, Silent Mode | Heavy task governance and assistant behavior controls | 1 week | Behavior policy compliance pass |
| 10 | Privacy, Data Retention, Cloud/Plugin Trust | Retention, archive, explicit cloud fallback, signed plugins | 1 week | Compliance and audit gates pass |
| 11 | Performance and Reliability Hardening | Timeout tuning, retries, latency and stability | 1-2 weeks | SLO gate: latency and reliability |
| 12 | UAT, Release, and Operational Runbook | Final acceptance, docs, rollback runbook | 4-6 days | Production readiness sign-off |

---

## 3. Test Layers to Use in Every Phase

- Rust unit/feature tests: `crates/kria-core/tests/*.rs`
- Rust server integration tests: `crates/kria-server/tests/*.rs`
- Playwright API tests: `tests/e2e/tests/*.api.spec.ts`
- Playwright E2E tests: `tests/e2e/tests/*.e2e.spec.ts`

Standard commands:

```bash
cargo test --workspace
cargo test -p kria-core --test <test_file>
cargo test -p kria-server --test integration_api
cargo test -p kria-server --test integration_ws
cd tests/e2e && npx playwright test --project=api-integration
cd tests/e2e && npx playwright test --project=e2e-chromium
```

---

## 4. Detailed Phase Plan

## Phase 0 - Foundation and Traceability

### Implementation checklist

- [ ] Build requirement-to-module traceability table from KRIA_REQUIREMENTS.md.
- [ ] Add config keys for all finalized policies not yet represented in runtime config.
- [ ] Freeze default values for timeouts and thresholds:
- [ ] Install/uninstall: 300s.
- [ ] Download: 300s.
- [ ] Git push: 180s.
- [ ] Web fetch/search: 30s.
- [ ] Confidence threshold: 0.50.
- [ ] Add explicit contracts for lock-screen read-only and PIN elevation states.
- [ ] Add policy constants for DND phrases and silent mode command phrase.
- [ ] Add test file placeholders by phase under `crates/kria-core/tests/` and `tests/e2e/tests/`.

### Detailed tests

| Test ID | Type | Scenario | Expected Result |
|---|---|---|---|
| P0-T01 | Unit | Load default config | All finalized defaults resolve and deserialize correctly |
| P0-T02 | Unit | Missing key fallback | Runtime falls back to expected default without panic |
| P0-T03 | Unit | Invalid key values | Invalid values are rejected or clamped with clear error |
| P0-T04 | Integration | Runtime init uses frozen defaults | Startup logs and runtime state show expected constants |
| P0-T05 | API | Config endpoint contract | Returned config includes all required policy fields |

### Exit criteria

- Config and policy constants exist and are test-covered.
- No unresolved requirement without mapped owner module.

---

## Phase 1 - Core Orchestration and Safety Enforcement

### Implementation checklist

- [ ] Ensure all user requests route through agent loop (remove direct bypass paths).
- [ ] Enforce policy evaluation before every tool call.
- [ ] Enforce approval handling for sensitive/destructive actions.
- [ ] Enforce action rephrase event before sensitive execution.
- [ ] Ensure approval timeout behavior is auto-cancel and notify once.
- [ ] Ensure rollback trigger only for file-destructive RED actions.
- [ ] Persist audit events for decision + action + outcome.

### Detailed tests

| Test ID | Type | Scenario | Expected Result |
|---|---|---|---|
| P1-T01 | Unit | Tool call with GREEN action | Executes without approval and logs AUTO_EXECUTED |
| P1-T02 | Unit | Tool call with RED action | Emits approval-required event before execution |
| P1-T03 | Unit | Approval ignored for 15s | Action auto-cancels and user is notified once |
| P1-T04 | Unit | Sensitive action | Rephrase event emitted before approve/execute |
| P1-T05 | Unit | File-destructive RED action | Rollback snapshot is created |
| P1-T06 | Unit | Non-file-destructive RED action | No rollback snapshot created |
| P1-T07 | Integration | End-to-end tool action lifecycle | Event sequence is ordered and complete |
| P1-T08 | Integration | Audit record integrity | Decision, actor, timing, result are persisted |

### Exit criteria

- No tool execution path bypasses policy engine.
- Approval and audit behavior is deterministic and repeatable.

---

## Phase 2 - Voice-First Runtime

### Implementation checklist

- [ ] Implement wake-word only activation with alias support.
- [ ] Implement VAD end-of-speech pipeline (listen until user stops speaking).
- [ ] Implement barge-in (interrupt assistant TTS while user speaks).
- [ ] Keep push-to-talk backup enabled (Ctrl+Space).
- [ ] Implement confidence flow: one short clarification question, then text prompt fallback.
- [ ] Implement dynamic Hinglish response behavior.
- [ ] Emit short on-screen text summaries alongside voice responses.

### Detailed tests

| Test ID | Type | Scenario | Expected Result |
|---|---|---|---|
| P2-T01 | Unit | Wake phrase primary and aliases | All configured phrases activate correctly |
| P2-T02 | Unit | VAD end-of-speech detection | Listening stops only when speech ends |
| P2-T03 | Unit | Barge-in while TTS active | TTS is interrupted and capture restarts |
| P2-T04 | Unit | Confidence below threshold | Assistant asks one short clarification |
| P2-T05 | Unit | Clarification still low confidence | Switches to text prompt fallback |
| P2-T06 | Integration | Voice command to tool execution | Voice->intent->tool->voice summary loop completes |
| P2-T07 | E2E | Real microphone moderate-noise run | Command success rate meets baseline target |
| P2-T08 | E2E | Text fallback UX | Fallback prompt appears without deadlock |

### Exit criteria

- Voice path is stable and primary.
- Fallback behavior is reliable and user-visible.

---

## Phase 3 - Lock Screen, PIN, and Approval UX

### Implementation checklist

- [ ] Enforce lock-screen read-only policy.
- [ ] Implement typed PIN elevation for sensitive/destructive actions.
- [ ] On PIN failure, reject action and return to read-only lock-screen mode.
- [ ] Ensure read-only lock-screen commands execute without speaker verification requirement.
- [ ] Standardize approval dialogs to full detail format (action, args, impact, rollback).

### Detailed tests

| Test ID | Type | Scenario | Expected Result |
|---|---|---|---|
| P3-T01 | Unit | Lock-screen read-only command | Executes without PIN |
| P3-T02 | Unit | Lock-screen destructive command | Requires typed PIN before execution |
| P3-T03 | Unit | Wrong PIN entered | Action blocked and mode resets to read-only |
| P3-T04 | Integration | Approval dialog content | Contains all required detail fields |
| P3-T05 | E2E | Locked device workflow | Read-only succeeds, destructive blocked until PIN |

### Exit criteria

- Lock-screen policy cannot be bypassed by normal command flow.

---

## Phase 4 - Must-Work Status Commands

### Implementation checklist

- [ ] Implement robust system stats command path with summary + optional detail mode.
- [ ] Implement balanced 3-host internet probe and final status output.
- [ ] Implement ongoing-operations report (active + queued + waiting approvals + percentages).
- [ ] Add health telemetry for status command reliability tracking.

### Detailed tests

| Test ID | Type | Scenario | Expected Result |
|---|---|---|---|
| P4-T01 | Unit | System stats command | Returns CPU, RAM, GPU summary in expected schema |
| P4-T02 | Unit | Internet probe normal | 3-host probe returns connected when majority succeeds |
| P4-T03 | Unit | Internet probe degraded | Returns disconnected/degraded with reason |
| P4-T04 | Unit | Ongoing operations query | Returns active, queued, approval-waiting with progress |
| P4-T05 | Integration | Must-work command latency | Each command meets target in normal load |
| P4-T06 | Soak | 500 repeated status calls | Reliability >= 95 percent |

### Exit criteria

- Critical must-work command reliability reaches target.

---

## Phase 5 - Messaging (Gmail and WhatsApp)

### Implementation checklist

- [ ] Gmail send flow: compose draft, read summary, explicit send approval, typed PIN, then send.
- [ ] Gmail direct-send path is disabled; `sendGmailDraft` is allowed only after approval + PIN gate.
- [ ] Gmail delete/archive: enforce PIN-only policy.
- [ ] WhatsApp platform auto-select (desktop/web active target).
- [ ] Recipient ambiguity handling: top 3 candidates and user selection.
- [ ] Always preview recipient + message and require final send confirmation.
- [ ] Add message safety confirmations for low-confidence non-destructive sensitive actions.

### Detailed tests

| Test ID | Type | Scenario | Expected Result |
|---|---|---|---|
| P5-T01 | Unit | Gmail compose request | Draft generated with summary output |
| P5-T02 | Unit | Gmail send request | Requires explicit send approval and typed PIN before `sendGmailDraft` |
| P5-T03 | Unit | Gmail delete/archive | Requires PIN |
| P5-T04 | Unit | WhatsApp active platform detection | Selects active desktop/web correctly |
| P5-T05 | Unit | Recipient ambiguity | Returns top 3 and waits for user pick |
| P5-T06 | Unit | WhatsApp send action | Preview shown and final confirm required |
| P5-T07 | Integration | End-to-end Gmail flow | Draft->summary->approve->PIN->send succeeds |
| P5-T08 | E2E | End-to-end WhatsApp flow | Active platform message send succeeds safely |
| P5-T09 | Unit | Gmail send with wrong PIN | Send is blocked, draft remains unsent, rejection is logged |
| P5-T10 | Unit | Gmail send approval canceled | Send is not executed, draft remains available |

### Exit criteria

- Messaging actions are safe, explicit, and predictable.

---

## Phase 6 - Software and Download Controls

### Implementation checklist

- [ ] Enforce install/uninstall timeout at 300s.
- [ ] Enforce download timeout at 300s.
- [ ] Enforce explicit approval before executing downloaded artifacts.
- [ ] Support package source policy as defined by user preference.
- [ ] Improve install/uninstall result verification and error guidance.

### Detailed tests

| Test ID | Type | Scenario | Expected Result |
|---|---|---|---|
| P6-T01 | Unit | Install hits timeout | Operation aborts cleanly at 300s |
| P6-T02 | Unit | Download hits timeout | Operation aborts cleanly at 300s |
| P6-T03 | Unit | Download then execute attempt | Execution blocked until explicit approval |
| P6-T04 | Integration | Install package success path | Search/check/install/verify flow completes |
| P6-T05 | Integration | Uninstall path | Approval + completion + verification behavior correct |
| P6-T06 | E2E | User-level install flow | UI events and confirmations match policy |

### Exit criteria

- Software management is policy-compliant and deterministic.

---

## Phase 7 - Git and Developer Controls

### Implementation checklist

- [ ] Enforce explicit approval for every push.
- [ ] Block push to main/master without explicit approval (branch protection policy).
- [ ] Enforce git push timeout at 180s.
- [ ] Run lint/tests automatically before push when available.
- [ ] Improve git error messages: short reason + next step.

### Detailed tests

| Test ID | Type | Scenario | Expected Result |
|---|---|---|---|
| P7-T01 | Unit | Push command on feature branch | Requests explicit approval |
| P7-T02 | Unit | Push command on main/master | Hard gate requires explicit approval |
| P7-T03 | Unit | Push timeout | Aborts at 180s with actionable error |
| P7-T04 | Unit | Pre-push checks available | Lint/tests are triggered before push |
| P7-T05 | Integration | Failed pre-push check | Push blocked with clear next step |
| P7-T06 | Integration | Successful pre-push check | Push proceeds after approval |

### Exit criteria

- No push occurs without policy-mandated approval path.

---

## Phase 8 - Notes, Tasks, Scheduling, and Reminders

### Implementation checklist

- [ ] Implement hybrid note capture (free text or structured auto-detect).
- [ ] Enable auto-tagging with manual edit option.
- [ ] Default task priority set to medium.
- [ ] Default reminder lead time set to 15 minutes.
- [ ] Reminder escalation repeats until dismissed.
- [ ] Scheduling NLP parses natural language and proceeds automatically.

### Detailed tests

| Test ID | Type | Scenario | Expected Result |
|---|---|---|---|
| P8-T01 | Unit | Voice note free-text input | Stored correctly with inferred structure |
| P8-T02 | Unit | Auto-tagging | Topic tags assigned and editable |
| P8-T03 | Unit | New task without priority | Priority defaults to medium |
| P8-T04 | Unit | Reminder creation | Lead time defaults to 15 minutes |
| P8-T05 | Unit | Reminder ignored | Repeats until user dismisses |
| P8-T06 | Unit | Natural language schedule | Parsed and scheduled without extra prompt |
| P8-T07 | Integration | End-to-end reminder lifecycle | Create->notify->repeat->dismiss works |

### Exit criteria

- Productivity workflow is reliable for voice-first daily use.

---

## Phase 9 - Concurrency, Proactivity, DND, and Silent Mode

### Implementation checklist

- [ ] Implement unlimited light-task concurrency.
- [ ] Implement heavy-task cap and replacement decision flow.
- [ ] Suggest replacing lowest-priority heavy task on overflow.
- [ ] Keep heavy queue items non-expiring by default.
- [ ] Implement proactive nudge cap: max 3 nudges per hour.
- [ ] Implement DND phrases:
- [ ] Enable phrase: Ria do not disturb.
- [ ] Disable phrase: Ria resume alerts.
- [ ] Implement silent mode quick toggle: Ria silent mode on/off.

### Detailed tests

| Test ID | Type | Scenario | Expected Result |
|---|---|---|---|
| P9-T01 | Unit | Submit light tasks at volume | Light tasks run concurrently without cap blockage |
| P9-T02 | Unit | Submit heavy task over cap | Assistant asks replacement decision |
| P9-T03 | Unit | Overflow recommendation | Suggests lowest-priority queued task |
| P9-T04 | Unit | Nudge frequency stress test | Never exceeds 3 proactive nudges per hour |
| P9-T05 | Unit | DND enable/disable phrases | Mode toggles correctly |
| P9-T06 | Unit | Silent mode toggle phrase | Voice output suppressed when enabled |
| P9-T07 | Integration | Operation status query | Includes active, queued, approval-waiting, percentages |

### Exit criteria

- Behavior controls are stable and policy-compliant under load.

---

## Phase 10 - Privacy, Retention, Cloud Fallback, Plugin Trust

### Implementation checklist

- [ ] Enforce transcript and raw audio retention policies.
- [ ] Move raw audio older than 7 days to encrypted archive.
- [ ] Enforce audit retention at 30 days.
- [ ] Encrypt credentials and tokens at rest.
- [ ] Enforce plugin activation to signed/trusted plugins only.
- [ ] Enforce cloud fallback as explicit one-time approval only.
- [ ] Implement old-memory usage consent prompt once per session.

### Detailed tests

| Test ID | Type | Scenario | Expected Result |
|---|---|---|---|
| P10-T01 | Unit | Raw audio retention job | Files older than 7 days archived encrypted |
| P10-T02 | Unit | Audit retention job | Entries older than 30 days pruned |
| P10-T03 | Unit | Credentials/tokens storage | Stored encrypted and decryptable by runtime |
| P10-T04 | Unit | Unsigned plugin load | Rejected with clear error |
| P10-T05 | Unit | Cloud fallback request | Requires explicit one-time user approval |
| P10-T06 | Integration | Session memory-consent behavior | Prompt appears once per session |

### Exit criteria

- Privacy and trust controls are enforceable and auditable.

---

## Phase 11 - Performance and Reliability Hardening

### Implementation checklist

- [ ] Tune speech and tool pipeline to hit latency goals for simple commands.
- [ ] Validate immediate retry behavior with max 2 retries.
- [ ] Improve errors to short reason + next step consistently.
- [ ] Add reliability telemetry dashboards for key command classes.
- [ ] Run soak tests and fault-injection tests.

### Detailed tests

| Test ID | Type | Scenario | Expected Result |
|---|---|---|---|
| P11-T01 | Perf | Simple voice command latency | P50/P90 in target range under nominal load |
| P11-T02 | Reliability | 1000 mixed command run | Success ratio >= 95 percent |
| P11-T03 | Fault | External dependency transient failure | Immediate retries (max 2) then actionable failure |
| P11-T04 | Perf | Concurrent light tasks + capped heavy tasks | No starvation or deadlock |
| P11-T05 | E2E | Must-work command regression pack | All critical commands pass |

### Exit criteria

- Measured reliability and latency meet target goals.

---

## Phase 12 - UAT, Release, and Operational Runbook

### Implementation checklist

- [ ] Create UAT scripts from KRIA_REQUIREMENTS acceptance criteria.
- [ ] Finalize rollback and incident-response runbook.
- [ ] Finalize release checklist and post-release validation checklist.
- [ ] Document known limitations and fallback flows.

### Detailed tests

| Test ID | Type | Scenario | Expected Result |
|---|---|---|---|
| P12-T01 | UAT | Voice-first daily scenario pack | User signs off on core workflows |
| P12-T02 | UAT | Safety and lock-screen scenario pack | All security policies verified manually |
| P12-T03 | UAT | Messaging and git protected flows | Confirmation and approvals verified |
| P12-T04 | Release | Smoke test after packaging | Core health and must-work commands pass |
| P12-T05 | Ops | Rollback drill | Rollback runbook executes successfully |

### Exit criteria

- User acceptance complete and release readiness signed.

---

## 5. Master Checklist (Cross-Phase)

- [ ] Every requirement in KRIA_REQUIREMENTS.md mapped to at least one implementation task.
- [ ] Every phase has automated tests and explicit pass/fail gate.
- [ ] Every destructive path includes typed PIN and audit trace.
- [ ] Lock-screen behavior is policy-correct and non-bypassable.
- [ ] Cloud fallback is never silent; always explicit one-time approval.
- [ ] Must-work commands have dedicated regression suite.
- [ ] Release runbook, rollback runbook, and post-release checks are complete.

---

## 6. Suggested Test File Additions

Rust core tests:

- `crates/kria-core/tests/phase6_software_policy_tests.rs`
- `crates/kria-core/tests/phase7_git_policy_tests.rs`
- `crates/kria-core/tests/phase8_productivity_tests.rs`
- `crates/kria-core/tests/phase9_behavioral_governance_tests.rs`
- `crates/kria-core/tests/phase10_privacy_trust_tests.rs`
- `crates/kria-core/tests/phase11_reliability_tests.rs`

Server integration tests:

- `crates/kria-server/tests/integration_approvals.rs`
- `crates/kria-server/tests/integration_lock_screen.rs`

Playwright API tests:

- `tests/e2e/tests/policy.api.spec.ts`
- `tests/e2e/tests/messaging.api.spec.ts`

Playwright E2E tests:

- `tests/e2e/tests/voice_first.e2e.spec.ts`
- `tests/e2e/tests/lockscreen_security.e2e.spec.ts`
- `tests/e2e/tests/messaging_confirmations.e2e.spec.ts`
- `tests/e2e/tests/proactivity_modes.e2e.spec.ts`

---

## 7. Definition of Done per Phase

A phase is complete only if all are true:

1. All checklist items for the phase are implemented.
2. All phase tests pass in local and CI runs.
3. No critical regression in previous phase suites.
4. Performance and safety gates for that phase are met.
5. Documentation updates are merged with code.

This plan is the execution checklist derived from KRIA_REQUIREMENTS.md.
