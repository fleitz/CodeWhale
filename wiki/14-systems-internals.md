# Internal Runtime Systems

This document surveys five runtime systems inside the CodeWhale TUI/agent
engine: **Workrooms**, **REPL Sandbox**, **Workspace Snapshots**, **Context
Compaction**, and **Message Purging**. Each system operates at a different
layer — durable chat containers, code execution, rollback safety, and
context-window management — but they all run inside the same `codewhale`
process and share the same event loop, configuration, and error model.

---

## Workrooms (durable chat containers) ⚠️ EXPERIMENTAL

> ⚠️ **EXPERIMENTAL — not yet wired.** Workroom types are defined and
> serializable, but the full workroom manager, persistence backend, and
> TUI/mobile/chat-bridge surfaces are still in development. This section
> describes the protocol-level contract; runtime behaviour may change before
> stabilisation. See [RFC 3209](../../docs/rfcs/3209-workrooms.md).

**Source:** `crates/protocol/src/workroom.rs` (346 lines)

### Purpose

Workrooms are durable, addressable containers for threaded agent
conversations — think "persistent chat workspace." They group threads,
events, and external references (GitHub issues, PRs, commits) into a
stable surface that the TUI, mobile web page, chat bridges, and the
programmatic Runtime API can all access uniformly.

### Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         Workroom                                │
│  id: wr_<uuid>     title     workspace     visibility           │
│  owner     created_at    updated_at    repo_identity (optional) │
└──────────┬──────────────────────────────────────────────────────┘
           │
           │ 1:N
           ▼
┌──────────────────────────────────────────────────────────────────┐
│                    WorkroomThread                                │
│  id    workroom_id    title    kind    external_ref (optional)   │
│                                                                  │
│  kind: Channel | DirectMessage | AgentTask | ApprovalQueue       │
│        | ReceiptLog                                              │
│                                                                  │
│  external_ref: GitHubIssue | GitHubPullRequest                   │
│               | GitHubCommit | GitHubCheck                       │
└──────────┬───────────────────────────────────────────────────────┘
           │
           │ 1:N
           ▼
┌──────────────────────────────────────────────────────────────────┐
│                     WorkroomEvent                                │
│  id    thread_id    workroom_id    timestamp    kind             │
│  agent: AgentAttribution (provider, model, agent_id)             │
│                                                                  │
│  kind: Message | Mention | ToolCall | ToolResult                 │
│       | ApprovalRequest | ArtifactLinked | Receipt               │
│       | Failure | NeedsHuman | Resumed                           │
└──────────────────────────────────────────────────────────────────┘
```

### Key Types

| Type | Role | Lines |
|------|------|-------|
| `WorkroomId` | UUID v4 with `wr_` prefix; stable across restarts | 16–36 |
| `Workroom` | Top-level container with title, workspace path, owner, visibility | 39–49 |
| `RepoRef` | GitHub owner/name pair attached to a workroom | 52–56 |
| `WorkroomVisibility` | `Private` or `Shared { allowed_tokens }` | 59–66 |
| `WorkroomThread` | A thread inside a workroom; typed by `WorkroomThreadKind` | 69–78 |
| `WorkroomThreadKind` | `Channel`, `DirectMessage`, `AgentTask`, `ApprovalQueue`, `ReceiptLog` | 80–88 |
| `ExternalThreadRef` | Tagged enum linking to GitHub issues/PRs/commits/check-runs (metadata only — no secrets) | 91–116 |
| `WorkroomEvent` | An attributed event within a thread | 118–128 |
| `WorkroomEventKind` | Tagged enum: `Message`, `Mention`, `ToolCall`, `ToolResult`, `ApprovalRequest`, `ArtifactLinked`, `Receipt`, `Failure`, `NeedsHuman`, `Resumed` | 130–143 |
| `AgentAttribution` | `provider`, `model`, `agent_id` — which agent produced the event | 146–151 |
| `WorkroomLink` | Shareable `codewhale://workroom/...` URL; parse/serialise round-trip | 154–221 |
| `WorkroomSummary` | Projection for list/inbox views | 240–246 |
| `WorkroomListResponse` | Paginated workroom list | 248–252 |
| `WorkroomResolveResponse` | Full resolution (link + thread title + external ref + recent events) | 254–261 |

### Link Format

```
codewhale://workroom/wr_<id>
codewhale://workroom/wr_<id>/thread/<thread_id>
codewhale://workroom/wr_<id>/thread/<thread_id>/event/<event_id>
codewhale://workroom/wr_<id>/event/<event_id>
```

Parsing is strict: unknown segments, empty IDs, missing `wr_` prefix, or
trailing garbage all return `None` (lines 170–200, 298–320).

### Visibility Model

- `Private` — only the local user can access.
- `Shared { allowed_tokens }` — accessible to callers bearing one of the
  listed bearer tokens.

### Integration Points

Workrooms are designed to be the canonical addressing layer above the
existing conversation model. A `WorkroomLink` can be embedded in a chat
message, sent to a mobile page, or resolved via the Runtime API
(`/workroom/resolve`). `ExternalThreadRef` bridges to GitHub without
storing credentials — just owner/repo/number or SHA.

---

## REPL Sandbox (Python code extraction)

**Source:** `crates/tui/src/repl/sandbox.rs` (80 lines),
`crates/tui/src/repl/runtime.rs` (1,486 lines),
`crates/tui/src/repl/mod.rs` (12 lines)

### Purpose

The agent loop scans assistant text responses for ` ```repl ` fenced
code blocks and feeds them to a long-lived Python subprocess. The same
Python runtime also powers RLM turns (the "REPL-augmented LM" pattern)
with sub-LLM RPC dispatch wired through stdin/stdout.

### Sandbox: fence extraction

```
Agent response text
     │
     ▼
┌──────────────────────────────────────────┐
│  sandbox::has_repl_block(text) → bool    │  simple substring check
│  sandbox::extract_repl_blocks(text)       │  → Vec<ReplBlock>        │
│                                           │                          │
│  ReplBlock { code, start_offset,         │  byte offsets for        │
│              end_offset }                 │  position tracking       │
└──────────────────┬───────────────────────┘
                   │
                   ▼
         Vec<ReplBlock> → PythonRuntime::run_round(code)
```

`extract_repl_blocks` (lines 12–32) walks the text forward, finding each
` ```repl ` fence and the matching closing ` ``` `. It records byte
offsets so the caller knows exactly where each block sits in the original
response.

### Runtime: the Python subprocess

```
┌──────────────────────────────────────────────────────────────────┐
│                      PythonRuntime                               │
│                                                                  │
│  child: tokio::process::Child                                    │
│  stdin ──────────────────────────► Python subprocess             │
│  stdout ◄───────────────────────── (same global namespace)       │
│                                                                  │
│  session_id: UUID  (prevents sentinel collisions)                │
│  round_count: u64                                                │
│  round_timeout: Duration (180 s default)                         │
└──────────────────────────────────────────────────────────────────┘
```

- **Protocol:** Code blocks are framed by `__RLM_RUN__` / `__RLM_END__`
  sentinels on stdin. The bootstrap `exec()`s them into the same global
  namespace, so variables, imports, and file handles persist across
  rounds.
- **Sub-LLM RPC:** Python emits `__RLM_REQ_<sid>__::{json}` on stdout;
  Rust dispatches the request (e.g., `sub_query`, `sub_rlm`) and writes
  `__RLM_RESP_<sid>__::{json}` back on stdin. No HTTP sidecar, no temp
  ports — the same pipes carry both control and data (lines 9–16).
- **FINAL capture:** Python code can call `finalize(value, confidence=...)`.
  These are captured as `final_value` (string repr) and `final_json`
  (structured `serde_json::Value`) on the `ReplRound` result (lines
  48–55).

### Key Types (runtime.rs)

| Type | Role | Lines |
|------|------|-------|
| `ReplRound` | Result of one code block: stdout, stderr, has_error, final_value, final_json, rpc_count, elapsed | 39–61 |
| `RpcRequest` | Tagged enum: `Llm`, `LlmBatch`, `Rlm`, `RlmBatch` | 63–103 |
| `RpcResponse` | `Single(SingleResp)` or `Batch(BatchResp)` | 105–113 |
| `RpcDispatcher` | Trait for dispatching Python RPCs into Rust LLM/Runtime clients | 132–137 |
| `PythonRuntime` | Long-lived subprocess handle + protocol state | 155–168 |

### Two spawn paths

1. **`PythonRuntime::new()`** — no context file; used by the agent loop
   for inline `repl` blocks the model emits in regular conversation
   (line 174).
2. **`PythonRuntime::spawn_with_context(path)`** — preloads a long input
   from a file; used by the RLM turn loop (line 195).

The shared `spawn_inner` (line 199) handles python binary discovery,
bootstrap script injection, stdin/stdout pipe setup, and session-id
generation.

---

## Workspace Snapshots (pre/post-turn safety net)

**Source:** `crates/tui/src/snapshot/mod.rs` (51 lines),
`crates/tui/src/snapshot/paths.rs` (131 lines),
`crates/tui/src/snapshot/prune.rs` (93 lines),
`crates/tui/src/snapshot/repo.rs` (1,514 lines)

### Purpose

Before and after every turn, the engine snapshots the user's workspace
into a **side git repo** completely independent of the user's own `.git`.
This provides a non-fatal rollback safety net: `/restore N` (slash
command) or the `revert_turn` tool can rewind the workspace to any
recent snapshot.

### Why a side repo?

- The user's own `.git` is **never touched**. Every `git` invocation
  passes explicit `--git-dir` and `--work-tree` flags (repo.rs lines
  3–13).
- Workspaces without git still get snapshots.
- Git's content-addressed storage (object packfiles) keeps disk
  footprint tractable — typically 10–30× compression.
- `gc.auto = 0` on the side repo prevents background GC during turns.

### Directory Layout

```
~/.codewhale/snapshots/
  <project_hash>/           ← FNV-1a of canonical workspace path (stripping .worktrees/)
    <worktree_hash>/        ← FNV-1a of canonical path including worktree suffix
      .git/                 ← side repo (git init'd here)
```

- `project_hash` is derived after stripping any `.worktrees/<name>`
  suffix, so multiple worktrees of the same repo share a project
  directory (paths.rs lines 62–74).
- `worktree_hash` keeps commits isolated per worktree.

### Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                      Snapshots Module                            │
│                                                                  │
│  paths.rs        prune.rs           repo.rs                      │
│  ────────        ─────────          ───────                      │
│  snapshot_dir_for  prune_older_than  SnapshotRepo                │
│  snapshot_git_dir                    ┌──────────────────────┐    │
│  ensure_snapshot_dir                 │ open_or_init          │    │
│                                      │ snapshot(label) → SHA │    │
│                                      │ restore(SnapshotId)    │    │
│                                      │ list(limit) → Vec<Snap>│    │
│                                      │ prune_older_than(age)  │    │
│                                      │ prune_unreachable()    │    │
│                                      └──────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
```

### Key Types

| Type | Role | Source |
|------|------|--------|
| `SnapshotId(String)` | Git commit SHA inside the side repo | repo.rs:26–34 |
| `Snapshot { id, label, timestamp }` | One row from `git log` | repo.rs:37–45 |
| `SnapshotRepo { git_dir, work_tree }` | Wrapper around the side repo | repo.rs:48–51 |

### Limits and Guards

| Constant | Value | Purpose | Source |
|----------|-------|---------|--------|
| `DEFAULT_MAX_SNAPSHOTS` | 50 | Max snapshots kept per workspace | mod.rs:47 |
| `DEFAULT_MAX_AGE` | 7 days | Prune window at session start | prune.rs:14 |
| `MAX_SNAPSHOT_SIZE_MB` | 500 MB | Aggressive prune trigger on snapshot | repo.rs:58 |
| `PRUNE_TARGET_MB` | 400 MB | Prune target margin | repo.rs:62 |
| `DEFAULT_MAX_WORKSPACE_BYTES_FOR_SNAPSHOT` | 2 GB | Self-disable ceiling for giant workspaces | repo.rs:74 |
| `SIZE_WALK_MAX_ENTRIES` | 200,000 | File entry cap for size estimator | repo.rs:80 |

### Built-in Excludes

A large set of directories and extensions are excluded from snapshots to
avoid bloat: `node_modules/`, `target/`, `dist/`, `build/`, `.venv/`,
`__pycache__/`, `.git/`, and many more (repo.rs lines 119–184). Binary
artifacts (`.exe`, `.so`, `.wasm`, `.zip`, `.mp4`, etc.) are also
skipped — snapshots are source rollback checkpoints, not binary backups.

### Failure Model

Pre/post-turn snapshot calls are **non-fatal**. If `git` is missing, the
disk is full, or the filesystem is read-only, the turn proceeds and the
engine logs a warning. The snapshot is a safety net, not a correctness
gate (mod.rs lines 29–33).

### Restore

`SnapshotRepo::restore(id)` (repo.rs lines 421–437) uses
`git checkout <sha> -- :/` to restore every tracked path. It also
removes any files that existed in the current snapshot but not the
target, and prunes empty parent directories.

---

## Context Compaction

**Source:** `crates/tui/src/compaction.rs` (2,895 lines)

### Purpose

When the conversation history grows too large for the model's context
window, compaction summarises older messages via an LLM call, replacing
them with a concise summary that preserves semantic continuity while
freeing token budget.

### Configuration

```rust
// compaction.rs:28-34
pub struct CompactionConfig {
    pub enabled: bool,         // on by default (v0.8.6+)
    pub token_threshold: usize, // 800K default (80% of V4's 1M window)
    pub model: String,
    pub cache_summary: bool,   // rewrite V4 prefix cache
}
```

- **Token-only trigger** since v0.8.11: the old `message_threshold`
  field was removed because message count is a poor proxy for context
  pressure on long-window models (lines 21–27).
- Default 800K threshold targets 80% of DeepSeek V4's 1M-token window
  (lines 44–55).
- Real call sites override via
  `compaction_threshold_for_model_and_effort`.

### Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                    Compaction Flow                                │
│                                                                  │
│  Token count exceeded threshold?                                 │
│       │                                                          │
│       ▼                                                          │
│  ┌─────────────────────────────────────────────┐                 │
│  │ 1. Select working-set messages              │                 │
│  │    - Keep KEEP_RECENT_MESSAGES (4) at tail  │                 │
│  │    - Scan RECENT_WORKING_SET_WINDOW (12)    │                 │
│  │    - Extract up to MAX_WORKING_SET_PATHS    │                 │
│  │    - Build summary prompt from middle msgs  │                 │
│  └──────────────────┬──────────────────────────┘                 │
│                     │                                            │
│                     ▼                                            │
│  ┌─────────────────────────────────────────────┐                 │
│  │ 2. LLM summarisation                        │                 │
│  │    - Head/Tail snippet strategy             │                 │
│  │    - Large-context models get bigger limits │                 │
│  │    - V4 prefix cache awareness              │                 │
│  └──────────────────┬──────────────────────────┘                 │
│                     │                                            │
│                     ▼                                            │
│  ┌─────────────────────────────────────────────┐                 │
│  │ 3. Replace compacted region with summary    │                 │
│  │    - Emit CompactionCompleted event          │                 │
│  │    - Summary message + kept recent messages  │                 │
│  └─────────────────────────────────────────────┘                 │
└──────────────────────────────────────────────────────────────────┘
```

### Key Constants

| Constant | Value | Purpose | Lines |
|----------|-------|---------|-------|
| `KEEP_RECENT_MESSAGES` | 4 | Messages preserved at tail after compaction | 62 |
| `HARD_COMPACT_KEEP_RECENT` | 8 | More aggressive preservation for hard compact | 64 |
| `RECENT_WORKING_SET_WINDOW` | 12 | Window scanned for active file paths | 65 |
| `MAX_WORKING_SET_PATHS` | 24 | Max file paths carried into summary | 66 |
| `MIN_SUMMARIZE_MESSAGES` | 6 | Minimum messages before summarisation is worthwhile | 67 |
| `LARGE_CONTEXT_WINDOW_TOKENS` | 500,000 | Threshold for "large context" summary limits | 80 |
| `LARGE_CONTEXT_SUMMARY_MAX_TOKENS` | 2,048 | Max output tokens for large-context summaries | 79 |
| `CACHE_ALIGNED_SUMMARY_CONTEXT_BUDGET_PERCENT` | 85 | Context budget percentage for cache-aligned summaries | 81 |

### V4 Prefix Cache Awareness

DeepSeek V4 (and similar models) use a prefix-cache optimisation: the
API caches the prefix of the prompt that hasn't changed between
requests. Compaction **rewrites the cacheable prefix**, so it
deliberately defaults to a higher threshold (800K) than the "suggest
/compact" guidance (60%). This means automatic replacement compaction
remains opt-in for the cache benefit (lines 45–55).

### Two Compaction Entry Points

1. **`compact_messages_safe`** (line 931) — wraps `compact_messages`
   with retry logic for transient errors. Never corrupts original
   messages.
2. **`compact_messages`** (line 1088) — the core compaction logic:
   selects which messages to summarise, builds the prompt, calls the
   LLM, and returns the compacted message list.

### Summary Snippet Strategy

Two sets of limits depending on whether the model has a "large context"
(≥500K tokens):

| Parameter | Standard | Large Context |
|-----------|----------|---------------|
| Text snippet | 800 chars | 2,000 chars |
| Tool result snippet | 240 chars | 4,000 chars |
| Input max chars | 24,000 | 120,000 |
| Input head chars | 14,000 | 72,000 |
| Input tail chars | 6,000 | 36,000 |
| Max output tokens | (implicit) | 2,048 |

(compaction.rs lines 69–81)

---

## Message Purging (surgical context removal)

**Source:** `crates/tui/src/purge.rs` (922 lines)

### Purpose

Unlike compaction (which summarises old messages via LLM), purge lets
the **agent** analyse the conversation history and surgically remove or
rewrite individual messages that are no longer needed. The agent uses
the `purge_context` tool to submit a list of operations; the engine
validates and executes them.

### Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                     Purge Flow                                    │
│                                                                  │
│  Agent decides to free context space                              │
│       │                                                          │
│       ▼                                                          │
│  ┌─────────────────────────────────────────────┐                 │
│  │ build_purge_prompt(messages)                 │                 │
│  │   - Format conversation with ephemeral IDs   │                 │
│  │   - Include PURGE_INSTRUCTIONS               │                 │
│  │   - Snippets: text 60ch, tool result 80ch   │                 │
│  └──────────────────┬──────────────────────────┘                 │
│                     │                                            │
│                     ▼                                            │
│  ┌─────────────────────────────────────────────┐                 │
│  │ LLM returns JSON list of PurgeOp             │                 │
│  │   remove { msg_id }                          │                 │
│  │   replace { msg_id, block_idx, pattern, with }│                │
│  └──────────────────┬──────────────────────────┘                 │
│                     │                                            │
│                     ▼                                            │
│  ┌─────────────────────────────────────────────┐                 │
│  │ execute_purge_operations(messages, ops)      │                 │
│  │   - Cascade tool-call/result pairs           │                 │
│  │   - Apply regex replacements                 │                 │
│  │   - Return PurgeResult                       │                 │
│  └──────────────────┬──────────────────────────┘                 │
│                     │                                            │
│                     ▼                                            │
│  UI events: PurgeStarted → PurgeCompleted/PurgeFailed            │
└──────────────────────────────────────────────────────────────────┘
```

### Key Types

| Type | Role | Lines |
|------|------|-------|
| `PurgeOp` | `Remove { msg_id }` or `Replace { msg_id, block_idx, pattern, with }` | 70–81 |
| `PurgeResult` | `messages`, `removed_count`, `replaced_count` | 84–92 |

### Operations

1. **`remove`** — Delete an entire message by its 1-based ID. Tool-call/
   result pairing is automatic: removing one side of a tool interaction
   removes the other side too (lines 43–47).

2. **`replace`** — Rewrite part of a specific content block using Rust
   regex substitution. Must specify `block`, `pattern`, and `with`
   (lines 37–41).

### Prompt Strategy

`build_purge_prompt` (line 130) formats the entire conversation with
ephemeral 1-based sequential IDs. Each message is condensed to a compact
preview:

- User messages: `[N] user  Text (len chars): "snippet..."`
- Assistant messages: `[N] assistant` with per-block `[idx]` entries
- Thinking blocks are omitted (API-mandated; the agent cannot remove
  them)
- Tool use: `[idx] ToolUse (name, id=xxx, args=...)`
- Tool results: `[idx] ToolResult (id=xxx, len chars): "snippet..."`

(Purge.rs lines 134–178)

### Conservative by Design

The `PURGE_INSTRUCTIONS` constant (lines 26–64) explicitly instructs the
agent to be conservative: keep important decisions, architectural
choices, relevant file paths, and unconsumed tool outputs. Prune verbose
results, redundant confirmations, superseded file reads, and
incorporated boilerplate. "When in doubt, keep the message."

### UI Events

Three event variants keep the TUI informed:
- `PurgeStarted { message }` (line 97)
- `PurgeCompleted { messages_before, messages_after, removed_count,
  replaced_count, message }` (lines 102–119)
- `PurgeFailed { message }` (line 122)

---

## System Integration

These five systems interact across the agent/turn lifecycle:

```
┌──────────────────────────────────────────────────────────────────┐
│                        Turn Lifecycle                             │
│                                                                  │
│  1. [Snapshot] pre-turn snapshot of workspace                    │
│  2. [Compaction] check token threshold; compact if needed        │
│  3. Agent responds                                               │
│     ├── [Sandbox] extract ```repl blocks → PythonRuntime         │
│     └── [Purge] agent may call purge_context tool                │
│  4. [Snapshot] post-turn snapshot of workspace                   │
│                                                                  │
│  Persistent state:                                                │
│     [Workroom] durable chat container (threads/events/links)     │
└──────────────────────────────────────────────────────────────────┘
```

- **Snapshots** bracket every turn for rollback safety.
- **Compaction** fires before the LLM call when the context window is
  under pressure.
- **Sandbox** extracts and executes code blocks from the agent's
  response in real time.
- **Purge** gives the agent a tool to surgically free context space
  without summarisation.
- **Workrooms** sit above all of this as the durable addressing and
  organisation layer.

All systems share the same error model: failures are logged as warnings
and the turn proceeds. Snapshots, compaction, purging, and sandbox
execution are each non-fatal — the agent keeps working even if one
subsystem is temporarily unavailable.
