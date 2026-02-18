Target architecture: **Agent Runtime Core** (reusable) + **Adapters** (LLM/tools/UI/channels). The core should be agent-agnostic; only adapters change.

## Core concepts (portable)

### 1) Event-sourced memory

- Persist everything as **append-only events** (JSONL).
- “Short-term memory” = tail of events.
- “Long-term memory” = **summary events** produced by a compactor.
- Index metadata separately (small JSON), never as hidden model state.

Reusable pieces:

- `TranscriptStore` (append/read/repair)
- `SessionStore` (routing → transcript pointer)
- `Compactor` (policy + summarizer + rewrite)

### 2) Deterministic routing

- Route inbound input to a `SessionKey` via pure rules (channel/thread/user/agent id).
- Do not let the LLM decide session boundaries.
- This makes behavior testable and debuggable.

Reusable pieces:

- `SessionRouter` trait + routing rules

### 3) Serialized runs per session

- Only **one active run per sessionKey** to avoid interleaving tool calls / prompt state.
- Global parallelism cap is separate.
- Followups are buffered and drained after the run completes.

Reusable pieces:

- `QueueScheduler` with “lanes”
- `FollowupBuffer` policy (coalesce window, merge vs separate runs)

### 4) Run engine = a pure state machine

Single orchestration pipeline:

1. `get_or_create_session(sessionKey)`
2. `append(user_event)`
3. `history = read_context(sessionId)`
4. `prompt = build(system + skills + history)`
5. `llm_stream = llm.run(prompt, tools)`
6. while stream:
   - text → accumulate assistant message
   - tool_call → execute tool → append tool_call + tool_result → feed back to LLM

7. append final assistant
8. maybe compact
9. emit outbound message(s)

Reusable pieces:

- `RunEngine`
- `PromptBuilder`
- `ToolExecutor`
- `LlmClient`

### 5) Capability-driven tools (enforcement)

- “Skills/markdown” = human guidance.
- Tool schemas/policy = enforcement.
- Keep a strict boundary: tools are typed and allowlisted.

Reusable pieces:

- `ToolRegistry` + `ToolSpec` (schema)
- `ToolPolicy` (allow/deny, per-user/session/device scoping)
- `ToolExecutor` (timeouts, output caps, audit)

### 6) Control plane as an API

- Runtime exposes a local API for all frontends:
  - CLI
  - TUI
  - Web UI
  - Webhooks ingress

- Use HTTP + WebSocket.

Reusable pieces:

- `api/http` + `api/ws`
- DTOs stable across frontends

---

## “Targeted architecture” as reusable crates

If you want this reusable across many agents, split into crates:

```text
crabbot-runtime/
  events/            // TranscriptEvent, InboundEvent
  storage/           // SessionStore, TranscriptStore, locks
  routing/           // SessionRouter
  queue/             // lanes + followups
  run/               // RunEngine traits + state machine
  prompt/            // PromptBuilder trait, skills loader optional
  tools/             // registry, policy, executor traits
  llm/               // LlmClient trait, OpenAI-compat impl optional
  compaction/        // policy + summarizer trait + rewrite

  main.rs
  api/               // external api towards cli frontend and hooks
  tools/             // m87 wrappers, fs rules, etc
  prompt/            // your system prompt, skills location, etc
```

Your “agent flavors” differ only in:

- system prompt & skills bundles
- tool set + policy
- channel adapters / UI branding
- optional domain objects (e.g. “devices”, “repos”, “tickets”)

---

## The minimal trait set to keep generic

- `SessionRouter`
- `SessionStore`
- `TranscriptStore`
- `QueueScheduler`
- `PromptBuilder`
- `LlmClient`
- `ToolRegistry` / `ToolHandler`
- `Compactor` (or `Summarizer` + `CompactionPolicy`)
- `OutboundSink` (send replies to a channel/UI)

We keep these stable, so we can swap:

- different LLMs
- different tool ecosystems (m87 vs Kubernetes vs GitHub)
- different UIs/channels
  without touching the engine.

---

## What changes per agent, concretely

- Tools: names, schemas, policy boundaries
- Skills: how you instruct tool usage
- Routing: session key shape (per-user vs per-room vs per-task)
- Compaction: summary style and thresholds
- Frontends: CLI commands, UI views

Everything else should remain reusable.
