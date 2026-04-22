# KRIA Requirements Specification

Document: KRIA_REQUIREMENTS.md  
Project: K.R.I.A. (Kernel-Responsive Intelligent Agent)  
Owner: Obaid (single-owner assistant profile)  
Date: 2026-04-19  
Status: Finalized user preference baseline

---

## 1. Objective

Define a complete, implementation-ready requirements baseline for KRIA based on explicit user preferences, safety expectations, operating constraints, and performance goals.

KRIA must behave as a true AI assistant with strong autonomy for normal tasks, strict safeguards for risky actions, and voice-first interaction as primary mode.

---

## 2. User Profile and Environment

- Primary user model: single owner only.
- OS and session: Ubuntu 24.04, X11.
- Primary interaction style: voice-first, text secondary.
- Timezone and locale preference: Asia/India, English primary and Hindi secondary.
- Language behavior: dynamic Hinglish (auto-match user language style).
- Use environment: office with moderate noise.

---

## 3. Core Product Vision

KRIA should provide practical, day-to-day total control of laptop workflows while balancing speed, intelligence, and safety.

High-level vision points:

- Hands-free operation should be possible across all major assistant capabilities.
- KRIA should act intelligently, ask clarifying questions when ambiguity exists, and avoid unnecessary friction when intent is clear.
- KRIA should offer deep system and workflow orchestration, not just chat.
- KRIA should remain safe by design for destructive/system-critical actions.

---

## 4. Interaction Model

### 4.1 Primary mode

- Voice is primary.
- Text is secondary and should be used as fallback where needed.
- Operational mode: mixed mode based on context.

### 4.2 Activation and listening

- Activation mode: wake-word only.
- Primary wake phrase: Hey Ria.
- Accepted aliases: Hey Riya, Hello Ria, Hello Riya.
- Wake sensitivity: balanced.
- Listening window: continue listening until end-of-speech (VAD-based), not fixed short timeout.
- Push-to-talk backup: enabled (Ctrl+Space).

### 4.3 Response behavior

- Response format default: voice plus short on-screen text summary.
- Persona: friendly conversational.
- Response length: context-dependent.
- Latency target (simple commands): around 1-2 seconds.
- Barge-in: always interruptible while KRIA is speaking.

### 4.4 Ambiguity and confidence handling

- If STT/intent confidence is low: ask one short confirmation question.
- If still unclear: switch to text prompt fallback.
- Low-confidence threshold: 0.50.
- For ambiguous recipient/task resolution: show top 3 choices and ask user to pick.

### 4.5 Multi-command utterances

- If one spoken sentence contains multiple commands: ask before each sub-command.

### 4.6 Sensitive action wording

- For sensitive actions, KRIA must rephrase action intent before executing.
- For non-destructive sensitive actions: explicit yes/no voice confirmation is required only when confidence is low.

---

## 5. Safety, Approval, and Control Policy

### 5.1 Baseline safety rules

- Never delete anything without approval.
- Destructive actions require typed PIN every time.
- Approval prompt style for risky actions: full detail (action, args, impact, rollback).

### 5.2 Destructive action scope

Treat at least the following as destructive/high-risk:

- File/folder deletion.
- App/package uninstall.
- Service stop/restart.
- System reboot/shutdown.
- Git push/force-push (especially protected branches).
- Firewall/network rule changes.
- Permanent Gmail delete/archive.

### 5.3 Approval timeout behavior

- Approval timeout: 15 seconds.
- On ignored prompt: auto-cancel and notify once.
- No automatic forced execution after timeout.

### 5.4 Lock-screen policy

- Lock-screen default: read-only commands only.
- Sensitive/destructive command elevation from lock screen requires typed PIN.
- For read-only lock-screen commands, speaker verification is not required.
- If lock-screen elevated action auth fails, reject and return to read-only mode.

### 5.5 Emergency stop

- Immediate stop phrase: KRIA stop now.

### 5.6 Rollback

- Rollback snapshots required only for file-destructive RED actions.

---

## 6. Security and Privacy Requirements

### 6.1 Data and privacy model

- Internet usage: allowed automatically when useful to complete requests.
- Cloud fallback for LLM: allowed only with explicit one-time approval.
- Plugin trust policy: enable only signed/trusted plugins.

### 6.2 Local data handling

- Keep transcripts and raw audio.
- Raw audio retention window: 7 days.
- After retention window: move raw audio to encrypted archive.
- Audit retention: 30 days.
- Encryption scope: encrypt credentials/tokens (not mandatory full-data encryption).

### 6.3 Memory policy

- Persistent preferences should remain fixed until manually changed.
- No automatic memory decay.
- Before using old persistent memory context: ask once per session.

### 6.4 Sensitive read-out policy

- Sensitive content may be read aloud normally (final user preference).

---

## 7. Capability Requirements (Functional)

KRIA should support the following capability groups as first-class behavior.

### 7.1 App, window, and browser control

- Open, focus, close applications.
- Window management and desktop control.
- Full web automation with confirmation.
- Browser automation domain guard: no domain restrictions.

### 7.2 Messaging and communication

- Gmail read/search/draft/send and delete/archive workflows.
- WhatsApp desktop/web support with active-platform auto-selection.
- Telegram support.
- Final-send confirmations where defined.

### 7.3 Software management

- Install/update/uninstall applications/packages.
- Package source policy: any source if package name matches.
- Download execution/install from downloaded artifacts only after explicit approval.

### 7.4 System, resources, and process control

- CPU/RAM/GPU/status monitoring.
- Process controls (kill/priority adjustments).
- Service controls.
- Power/brightness/volume/network diagnostics and controls.
- Disk cleanup and duplicate detection.

### 7.5 Notes, tasks, reminders, and scheduling

- Notes and tasks capture by voice.
- Storage options include local DB and integrated task/reminder systems.
- Notes behavior: auto-tag by topic with manual edit option.
- Default task priority: medium.
- Reminder lead time default: 15 minutes.
- Reminder escalation: keep repeating until dismissed.
- Scheduling NLP behavior: parse natural language time and proceed automatically.

### 7.6 Developer and git workflow

- Git status/diff/commit/checkout support.
- Push policy: always explicit approval for every push.
- Branch protection: never push to main/master without explicit approval.
- Pre-push checks: run lint/tests automatically if available.

### 7.7 Vision and analysis

- Image analysis and description workflows.
- Document and batch processing workflows.

### 7.8 Remote session tooling

- Preferred remote tools: RustDesk, AnyDesk, Chrome Remote Desktop, built-in VNC/RDP.
- Session start requires approval.
- Keep remote session audit metadata (start/stop/target).

---

## 8. Workflow-Specific Rules

### 8.1 Gmail rules

- Send flow: compose draft -> read summary -> explicit send approval.
- Delete/archive policy: PIN only.

### 8.2 WhatsApp rules

- Platform selection: auto-select whichever channel is currently active (desktop/web).
- Recipient ambiguity: show top 3 candidates and ask user choice.
- Send policy: always preview recipient plus message and ask final confirmation.

### 8.3 Connectivity and status checks

- Internet check method: balanced 3-host probe.
- System stats response format: short summary plus optional detailed breakdown on request.
- Ongoing operations response must include:
  - Active tasks.
  - Queued tasks.
  - Waiting approvals.
  - Progress percentages.

---

## 9. Performance and Reliability Requirements

### 9.1 Reliability

- Overall reliability target: 95%.

### 9.2 Response speed

- Target for simple command response: approximately 1-2 seconds.

### 9.3 Operation-specific timeouts

- Install/uninstall timeout: 300 seconds.
- Download timeout: 300 seconds.
- Git push timeout: 180 seconds.
- Web fetch/search timeout: 30 seconds.

### 9.4 Retry behavior

- Retry strategy: immediate retry.
- Max automatic retries: 2.
- If still failing: provide short reason plus next step.

### 9.5 Progress updates

- Long tasks should provide milestone-based updates (not fixed interval chatter).

---

## 10. Concurrency and Task Governance

### 10.1 Concurrency model

- Unlimited light tasks allowed.
- Heavy tasks are capped.

### 10.2 Heavy task definition

Heavy tasks include at least:

- Software install/uninstall.
- Vision-heavy analysis.
- Code lint/test/build.
- Document batch processing.
- Inference-heavy model tasks.

### 10.3 Heavy queue behavior

- If heavy-task cap is reached:
  - Ask user which task to replace.
  - Suggest replacing lowest-priority task by default.
- Queued heavy tasks do not expire automatically.

---

## 11. Proactivity, Notifications, and Modes

### 11.1 Proactive behavior

- Proactive mode: highly proactive with frequent nudges.
- Nudge guardrail: max 3 proactive nudges per hour.

### 11.2 Notification style

- Notifications: voice plus desktop banner.

### 11.3 DND behavior

- No fixed DND schedule.
- DND is enabled only when user explicitly asks.
- DND enable phrase: Ria do not disturb.
- DND disable phrase: Ria resume alerts.

### 11.4 Silent/confidential mode

- Support text-only confidential mode (no spoken response).
- Quick toggle voice command: Ria silent mode on/off.

---

## 12. Offline and Failure Behavior

- If internet-dependent command cannot run due to connectivity loss:
  - Fail fast.
  - Provide clear reason.
  - Offer offline alternative where possible.

---

## 13. Critical Must-Work Commands

KRIA should be highly reliable for the following status-critical commands:

1. What is the System Stats?
2. Are you connected to Internet?
3. Is there any ongoing Operation you are doing?

Additionally, day-one must-work workflow set includes:

1. Send or fetch mail from Gmail.
2. Install or uninstall Chromium app.
3. Perform web search for requested topic.
4. Describe image after analysis.
5. Report laptop resource status (RAM, CPU, GPU).

---

## 14. Remote and Meeting Defaults

- Remote session start: approval required.
- Meeting auto-join: never auto-join; ask each time.
- Maintenance/update tasks: ask every time before execution.

---

## 15. Acceptance Criteria

KRIA implementation should be considered aligned when all of the following are true:

1. Voice-first interaction is default and stable in moderate-noise environment.
2. Safety policies enforce typed PIN for destructive actions consistently.
3. Lock-screen behavior remains read-only unless explicit PIN elevation succeeds.
4. Gmail and WhatsApp flows respect preview/approval rules exactly.
5. Git push always requires explicit approval and protects main/master.
6. Connectivity/system-status commands are reliable and fast.
7. Timeouts, retries, and heavy-task governance follow this document.
8. Data handling, retention, audit, and cloud fallback follow the declared privacy model.
9. Proactive behavior remains useful but bounded by nudge limit.
10. No conflicting defaults remain between voice, safety, and task orchestration layers.

---

## 16. Change Management

Any future changes to KRIA behavior should be applied by updating this document first, then reflecting the same policy in:

- Runtime config defaults.
- Safety policy and approval workflows.
- Voice pipeline settings.
- Tool orchestration and capability guards.
- UI prompts and confirmation flows.

This file is the canonical user-intent source for KRIA personalization and operational policy.
