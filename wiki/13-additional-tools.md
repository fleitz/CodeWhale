# CodeWhale Additional Tools Reference

> **Version:** v0.8.62
> **Supplement to:** `04-tool-system.md`
>
> This page documents tools that are registered in the CodeWhale tool system but either
> receive only a brief mention in the "Additional Tools" summary table of
> `04-tool-system.md` or are missing entirely. Tools already covered with full
> sections in `04-tool-system.md` are **not** repeated here.

---

## Summary Table

| # | Tool Name(s) | Source | Purpose |
|---|-------------|--------|---------|
| 1 | `finance` | `finance.rs` | Fetch live market quotes for stocks, ETFs, or crypto tickers |
| 2 | `speech` / `tts` | `speech.rs` | Generate speech/audio via configured Xiaomi MiMo TTS API |
| 3 | `web.run` | `web_run.rs` | Multi-command web browsing: search, open, click, find, screenshot |
| 4 | `diagnostics` | `diagnostics.rs` | Report workspace info, git detection, sandbox availability, and Rust toolchain |
| 5 | `fim_edit` | `fim.rs` | Fill-in-the-Middle code completion using DeepSeek FIM API |
| 6 | `automation_create` | `automation.rs` | Create a durable scheduled automation |
| 7 | `automation_list` | `automation.rs` | List durable automations with status and timestamps |
| 8 | `automation_read` | `automation.rs` | Read one durable automation plus recent run records |
| 9 | `automation_update` | `automation.rs` | Update a durable automation |
| 10 | `automation_pause` | `automation.rs` | Pause a durable automation |
| 11 | `automation_resume` | `automation.rs` | Resume a paused durable automation |
| 12 | `automation_delete` | `automation.rs` | Delete a durable automation and its run history |
| 13 | `automation_run` | `automation.rs` | Run an automation now (enqueues a normal durable task) |
| 14 | `checklist_add` | `todo.rs` | Add one checklist item on the active thread/task |
| 15 | `checklist_update` | `todo.rs` | Update one checklist item's status by id |
| 16 | `checklist_list` | `todo.rs` | List current checklist progress |
| 17 | `checklist_write` | `todo.rs` | Replace the active thread/task checklist |
| 18 | `run_verifiers` | `verifier.rs` | Run independent verifier gates in parallel across ecosystems |
| 19 | `task_create` | `tasks.rs` | Create/enqueue a durable background task |
| 20 | `task_list` | `tasks.rs` | List recent durable tasks with status and summaries |
| 21 | `task_read` | `tasks.rs` | Read durable task detail (timeline, checklist, gates, artifacts, PR attempts) |
| 22 | `task_cancel` | `tasks.rs` | Cancel a queued or running durable task |
| 23 | `task_gate_run` | `tasks.rs` | Run an approved verification gate command with structured evidence |
| 24 | `task_shell_start` | `tasks.rs` | Start a long-running shell command in the background |
| 25 | `task_shell_wait` | `tasks.rs` | Poll a background shell task; optionally records gate evidence |
| 26 | `pr_attempt_record` | `tasks.rs` | Capture git diff as a durable PR work attempt |
| 27 | `pr_attempt_list` | `tasks.rs` | List PR attempts recorded on a durable task |
| 28 | `pr_attempt_read` | `tasks.rs` | Read one recorded PR attempt and its patch artifact reference |
| 29 | `pr_attempt_preflight` | `tasks.rs` | Run `git apply --check` for a recorded attempt patch |

> **Note:** `approval_cache` (`approval_cache.rs`) is **not a tool**. It is internal
> infrastructure that fingerprints tool calls to cache approval decisions per-session.
> It is used by the approval flow, not exposed to the model as a callable tool.

---

## 1. `finance` — Live Market Quotes

**Source:** `crates/tui/src/tools/finance.rs:144`

**Purpose:** Fetch a live market quote for a stock, ETF, or crypto ticker using
Yahoo Finance-style public endpoints. Prefers Yahoo's quote endpoint and falls
back to the chart endpoint when the quote endpoint is unavailable.

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `ticker` | string | **yes*** | Ticker symbol to look up (e.g. `AAPL`, `SPY`, `BTC`) |
| `symbol` | string | no | Alias for `ticker` |
| `type` | string | no | Optional asset type hint: `equity`, `fund`, `crypto`, or `index` |
| `market` | string | no | Optional market hint (compatibility) |
| `timeout_ms` | integer | no | Request timeout in milliseconds (default: 10000, max: 60000) |

\* One of `ticker` or `symbol` required (`anyOf` constraint).

### Output

Returns a `FinanceQuoteResponse` JSON object:

| Field | Type | Description |
|-------|------|-------------|
| `requested_ticker` | string | Original ticker as provided |
| `ticker` | string | Resolved ticker symbol |
| `name` | string? | Long or short company/asset name |
| `price` | float | Current market price |
| `currency` | string? | Currency code (e.g. `USD`) |
| `change` | float? | Price change from previous close |
| `change_percent` | float? | Percentage change |
| `previous_close` | float? | Previous close price |
| `market_state` | string? | Market state (e.g. `REGULAR`, `CLOSED`) |
| `quote_type` | string? | Asset type (e.g. `EQUITY`, `CRYPTOCURRENCY`) |
| `exchange` | string? | Exchange name |
| `market_time` | integer? | Unix timestamp of last market data |
| `source` | string | `yahoo_quote` or `yahoo_chart` |
| `fallback_used` | boolean | Whether chart fallback was used |

**Capabilities:** `[ReadOnly, Network, Sandboxable]` — Approval: `Auto` — Supports parallel: **yes**

**Notable:** Crypto tickers without a `-USD` suffix and with `type: "crypto"` are
auto-resolved to `TICKER-USD`. `BTC` alone resolves to `BTC-USD`. The tool makes
up to two HTTP requests (quote then chart fallback).

---

## 2. `speech` / `tts` — Speech & Audio Generation

**Source:** `crates/tui/src/tools/speech.rs:42`

**Purpose:** Generate speech/audio directly through the configured Xiaomi MiMo
OpenAI-compatible API. Use this when the user asks for speech, TTS, narration,
read-aloud, voice design, or voice cloning.

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `text` | string | **yes** | Text to synthesize; MiMo TTS style/audio tags may be included |
| `output` | string | no | Audio file path; defaults to `speech.<format>` in `output_dir` or workspace |
| `output_dir` | string | no | Directory for the default output file (relative, stays inside workspace) |
| `model` | string | no | TTS model; defaults to `mimo-v2.5-tts`; enum: `mimo-v2.5-tts`, `mimo-v2.5-tts-voicedesign`, `mimo-v2.5-tts-voiceclone`, `mimo-v2-tts` |
| `voice` | string | no | Built-in voice ID (e.g. `mimo_default`, `冰糖`, `茉莉`, `Mia`, `Chloe`) or `data:audio/...;base64,...` URI for voice clone |
| `instruction` | string | no | Natural-language style, emotion, speed, scene, or performance instruction (not spoken) |
| `voice_prompt` | string | no | Voice design prompt; infers `mimo-v2.5-tts-voicedesign` model when model omitted |
| `clone_voice` | string | no | Path to a `.mp3` or `.wav` voice sample for cloning; infers `mimo-v2.5-tts-voiceclone` model |
| `format` | string | no | Requested audio format: `wav` (default), `mp3`, or `pcm16` |
| `stream` | boolean | no | Low-latency streaming (not yet implemented; leave `false`) |

### Output

Writes an audio file to the resolved output path. Returns `ToolResult` with the
output file path in `content`.

**Capabilities:** `[WritesFiles, Network, Sandboxable]` — Approval: `Auto`

**Notable:** Requires an active Xiaomi MiMo API client (provider `"xiaomi-mimo"`
configured with an API key). The `stream=true` flag is accepted but currently
returns an error directing the model to generate complete audio files.
`voice` and `clone_voice` are mutually exclusive.

---

## 3. `web.run` — Multi-Command Web Browsing

**Source:** `crates/tui/src/tools/web_run.rs:336`

**Purpose:** Browse the web using multiple commands in a single call
(`search_query`, `image_query`, `open`, `click`, `find`, `screenshot`). Results
include `ref_id` values for citation and follow-up operations.

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `search_query` | object[] | no | Array of search objects: `{q, recency?, max_results?, timeout_ms?, domains?}` |
| `image_query` | object[] | no | Array of image-search objects: `{q, recency?, max_results?, timeout_ms?, domains?}` |
| `open` | object[] | no | Array of open-page objects: `{ref_id, lineno?}` |
| `click` | object[] | no | Array of click-link objects: `{ref_id, id}` |
| `find` | object[] | no | Array of find-in-page objects: `{ref_id, pattern}` |
| `screenshot` | object[] | no | Array of screenshot objects: `{ref_id, pageno}` |
| `response_length` | string | no | Result verbosity: `short` (40 lines), `medium` (80, default), `long` (160) |

### Output

Returns a `WebRunOutput` JSON object with up to six result arrays, keyed by
command type:

| Key | Type | Description |
|-----|------|-------------|
| `search_query` | `SearchResult[]` | `{ref_id, query, source, count, results: [{title, url, snippet}]}` |
| `image_query` | `ImageQueryResult[]` | `{query, source, count, results: [{image, thumbnail?, title?, url?, width?, height?}]}` |
| `open` | `PageViewResult[]` | `{ref_id, url, title?, line_start, line_end, total_lines, content, links}` |
| `click` | `PageViewResult[]` | Same shape as `open` |
| `find` | `FindResult[]` | `{ref_id, pattern, count, matches: [{line, text}]}` |
| `screenshot` | `ScreenshotResult[]` | `{ref_id, pageno, total_pages, content}` |
| `warnings` | string[] | Accumulated warnings across all commands |

**Capabilities:** `[ReadOnly, Network]` — Approval: `Auto`

**Notable:** Sessions are tracked per namespace and auto-expire after 30 minutes.
Max 64 concurrent sessions, 256 pages per session. `ref_id` format:
`<namespace>turn<N><cmd><counter>`. PDF pages are auto-extracted and rendered
as text lines.

---

## 4. `diagnostics` — Workspace Diagnostics

**Source:** `crates/tui/src/tools/diagnostics.rs:19`

**Purpose:** Gather lightweight, best-effort environment information without
failing hard when optional commands are unavailable.

### Input Parameters

No parameters (empty schema).

### Output

Returns a `DiagnosticsOutput` JSON object:

| Field | Type | Description |
|-------|------|-------------|
| `workspace_root` | string | Workspace root directory path |
| `current_dir` | string? | Current working directory |
| `current_dir_error` | string? | Error from `env::current_dir()` if any |
| `git_repo` | boolean | Whether workspace is inside a git work tree |
| `git_branch` | string? | Current branch name |
| `git_error` | string? | Git detection error detail |
| `sandbox_available` | boolean | Whether platform sandbox is detected |
| `sandbox_type` | string? | Sandbox type string |
| `bwrap_available` | boolean | Whether bubblewrap is available (Linux) |
| `cgroup_version` | integer? | Cgroup version: 1 or 2 (Linux only) |
| `rustc_version` | string? | `rustc --version` output |
| `cargo_version` | string? | `cargo --version` output |
| `trusted_external_paths` | string[] | User-trusted external paths from `/trust add` |

**Capabilities:** `[ReadOnly]` — Approval: `Auto` — Supports parallel: **yes**

**Notable:** All probes are best-effort. If `git`, `rustc`, or `cargo` are
missing, the corresponding fields are `None` with error details recorded.

---

## 5. `fim_edit` — Fill-in-the-Middle Code Completion

**Source:** `crates/tui/src/tools/fim.rs`

**Purpose:** Edit a file using Fill-in-the-Middle (FIM) completion via the
DeepSeek `/beta/completions` FIM endpoint. Provide a file path, a prefix anchor
(text before the section to replace), and a suffix anchor (text after it). The
tool calls the FIM API to generate replacement content and writes it back.

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | **yes** | Path to the file to edit (relative to workspace) |
| `prefix_anchor` | string | **yes** | Text anchor marking the end of the prefix (kept as-is) |
| `suffix_anchor` | string | **yes** | Text anchor marking the start of the suffix (kept as-is) |
| `max_tokens` | integer | no | Maximum tokens to generate (default: 1024) |

### Output

Returns a `FimEditResult` JSON object:

| Field | Type | Description |
|-------|------|-------------|
| `success` | boolean | Whether the edit was applied |
| `path` | string | File path edited |
| `generated_text` | string | The LLM-generated middle content |
| `prefix_end` | integer | Byte position where the prefix ends |
| `suffix_start` | integer | Byte position where the suffix starts |
| `message` | string | Human-readable summary of the edit |

**Capabilities:** `[ReadOnly, WritesFiles, RequiresApproval]` — Approval: `Suggest`

**Notable:** Requires an active DeepSeek API client. Anchors must not overlap
(suffix must appear after the prefix). The file is read, the middle is
generated, and the file is rewritten with `prefix + generated + suffix`.

---

## 6. `automation_create` — Create Scheduled Automation

**Source:** `crates/tui/src/tools/automation.rs:16`

**Purpose:** Create a durable scheduled automation. Creation requires approval
and recurrence is constrained to supported HOURLY/WEEKLY RRULE forms. Runs
enqueue normal durable tasks.

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | **yes** | Automation name |
| `prompt` | string | **yes** | Work prompt for each run |
| `rrule` | string | **yes** | RRULE: `FREQ=HOURLY;INTERVAL=N[;BYDAY=MO,TU]` or `FREQ=WEEKLY;BYDAY=MO;BYHOUR=9;BYMINUTE=30` |
| `cwds` | string[] | no | Working directories for runs |
| `paused` | boolean | no | Start in paused state (default: false) |

**Capabilities:** `[RequiresApproval]` — Approval: `Required`

---

## 7. `automation_list` — List Automations

**Source:** `crates/tui/src/tools/automation.rs:17`

**Purpose:** List durable automations with status, next run, and last run timestamps.

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `limit` | integer | no | Max results (default: 50, min: 1, max: 100) |

**Capabilities:** `[ReadOnly]` — Approval: `Auto`

---

## 8. `automation_read` — Read Automation Detail

**Source:** `crates/tui/src/tools/automation.rs:18`

**Purpose:** Read one durable automation plus recent run records (up to 20).

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `automation_id` | string | **yes** | Automation identifier |

**Capabilities:** `[ReadOnly]` — Approval: `Auto`

---

## 9. `automation_update` — Update Automation

**Source:** `crates/tui/src/tools/automation.rs:19`

**Purpose:** Update a durable automation. Requires approval; recurrence remains
constrained to supported RRULE forms.

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `automation_id` | string | **yes** | Automation to update |
| `name` | string | no | New name |
| `prompt` | string | no | New prompt |
| `rrule` | string | no | New recurrence rule |
| `cwds` | string[] | no | New working directories |
| `status` | string | no | `active` or `paused` |

**Capabilities:** `[RequiresApproval]` — Approval: `Required`

---

## 10. `automation_pause` — Pause Automation

**Source:** `crates/tui/src/tools/automation.rs:20`

**Purpose:** Pause a durable automation. Requires approval.

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `automation_id` | string | **yes** | Automation to pause |

**Capabilities:** `[RequiresApproval]` — Approval: `Required`

---

## 11. `automation_resume` — Resume Automation

**Source:** `crates/tui/src/tools/automation.rs:21`

**Purpose:** Resume a paused durable automation. Requires approval.

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `automation_id` | string | **yes** | Automation to resume |

**Capabilities:** `[RequiresApproval]` — Approval: `Required`

---

## 12. `automation_delete` — Delete Automation

**Source:** `crates/tui/src/tools/automation.rs:22`

**Purpose:** Delete a durable automation and its run history. Requires approval.

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `automation_id` | string | **yes** | Automation to delete |

**Capabilities:** `[RequiresApproval]` — Approval: `Required`

---

## 13. `automation_run` — Run Automation Now

**Source:** `crates/tui/src/tools/automation.rs:23`

**Purpose:** Run an automation now. The run enqueues a normal durable task and
returns linked task/thread/turn ids as they become available.

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `automation_id` | string | **yes** | Automation to run |

**Capabilities:** `[RequiresApproval]` — Approval: `Required`

---

## 14–17. Checklist / Todo System

**Source:** `crates/tui/src/tools/todo.rs`

The checklist system tracks granular work progress under a durable task or
runtime thread. Four tools expose CRUD operations on a shared `TodoList`.
Legacy `todo_*` aliases exist for backward compatibility but are hidden from
the model (`model_visible() = false`).

### `checklist_add` — Add Checklist Item

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `content` | string | **yes** | The task description |
| `status` | string | no | `pending` (default), `in_progress`, or `completed` |

**Capabilities:** `[WritesFiles]` — Approval: `Auto`

### `checklist_update` — Update Checklist Item Status

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | integer | **yes** | Todo item id |
| `status` | string | **yes** | New status: `pending`, `in_progress`, `completed` |

**Capabilities:** `[WritesFiles]` — Approval: `Auto`

### `checklist_list` — List Checklist

No input parameters.

**Returns:** JSON snapshot: `{items: [{id, content, status}], completion_pct, in_progress_id}`.

**Capabilities:** `[ReadOnly]` — Approval: `Auto`

### `checklist_write` — Replace Entire Checklist

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `todos` | object[] | **yes** | Complete list of `{content: string, status: string}` items; replaces the existing list |

**Capabilities:** `[WritesFiles]` — Approval: `Auto`

**Notable:** `checklist_write` clears the existing list and rebuilds it from the
provided items. Only one item may be `in_progress` at a time; setting a new item
to `in_progress` resets the previous one to `pending`.

---

## 18. `run_verifiers` — Parallel Verifier Gates

**Source:** `crates/tui/src/tools/verifier.rs:30`

**Purpose:** Run independent verifier gates in parallel across detected Rust,
Node, Python, and Go projects. Supports explicit custom verifier commands as
`program + args` without requiring Bash. This is the agent-facing path for
"parallelize the verifier, not the generator."

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `profile` | string | no | Ecosystem set: `auto` (default), `rust`, `node`, `python`, `go` |
| `level` | string | no | `quick` (fast syntax/drift/build checks) or `full` (adds test/lint gates); default: `quick` |
| `max_python_files` | integer | no | Max Python files to syntax-parse (default: 200, max: 1000) |
| `commands` | object[] | no | Custom verifier gates: `[{name, program, args?, cwd?}]`; max 12 |
| `background` | boolean | no | Start gates as background shell jobs and return `task_id`s immediately (default: false) |

### Output

**Foreground mode** (`background: false`): Returns `RunVerifiersOutput`:

| Field | Type | Description |
|-------|------|-------------|
| `success` | boolean | All gates passed |
| `profile` | string | Profile used |
| `level` | string | Level used |
| `gate_count` | integer | Total gates run |
| `passed` | integer | Gates that passed |
| `failed` | integer | Gates that failed |
| `skipped` | integer | Gates skipped (missing toolchain) |
| `summary` | string | Human-readable summary |
| `gates` | `GateResult[]` | Per-gate: `{name, ecosystem, status, command, cwd, exit_code, duration_ms, stdout, stderr}` |

**Background mode** (`background: true`): Returns `RunVerifiersBackgroundOutput`
with `jobs[]` instead of `gates[]`, each containing `task_id` for later
inspection.

**Capabilities:** `[ExecutesCode, RequiresApproval]` — Approval: `Required`

**Notable:** Gates are run concurrently via `tokio::task::spawn_blocking`.
Stdout/stderr are truncated at 16,000 chars per gate. Custom gates run
directly as `program + args` (not through a shell); use `program: "bash"`,
`args: ["-lc", "..."]` only when Bash is intentionally needed.

---

## 19. `task_create` — Create Durable Task

**Source:** `crates/tui/src/tools/tasks.rs:45`

**Purpose:** Create/enqueue a durable background task through TaskManager.
Durable tasks are restart-aware executable work, distinct from sub-agents.

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `prompt` | string | **yes** | Work prompt for the durable task |
| `model` | string | no | Model to use |
| `workspace` | string | no | Workspace path; defaults to current workspace |
| `mode` | string | no | `agent`, `plan`, or `yolo` |
| `allow_shell` | boolean | no | Allow shell execution within the task |
| `trust_mode` | boolean | no | Trust mode for the task |
| `auto_approve` | boolean | no | Auto-approve actions within the task |

**Capabilities:** `[RequiresApproval]` — Approval: `Required`

---

## 20. `task_list` — List Durable Tasks

**Source:** `crates/tui/src/tools/tasks.rs:46`

**Purpose:** List recent durable tasks with status, linked thread/turn ids, and
concise summaries.

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `limit` | integer | no | Max tasks (default: 20, min: 1, max: 100) |

**Returns:** `{summary, tasks: [...]}`.

**Capabilities:** `[ReadOnly]` — Approval: `Auto`

---

## 21. `task_read` — Read Durable Task Detail

**Source:** `crates/tui/src/tools/tasks.rs:47`

**Purpose:** Read durable task detail including timeline, checklist, gate
evidence, artifacts, and PR attempts.

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `task_id` | string | **yes** | Full task id or unambiguous prefix |

**Capabilities:** `[ReadOnly]` — Approval: `Auto`

---

## 22. `task_cancel` — Cancel Durable Task

**Source:** `crates/tui/src/tools/tasks.rs:48`

**Purpose:** Cancel a queued or running durable task through TaskManager.
Requires approval because it changes work state.

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `task_id` | string | **yes** | Full task id or unambiguous prefix |

**Capabilities:** `[RequiresApproval]` — Approval: `Required`

---

## 23. `task_gate_run` — Run Verification Gate

**Source:** `crates/tui/src/tools/tasks.rs:49`

**Purpose:** Run an approved verification gate command and return structured
evidence. When inside a durable task, the gate result and log artifact are
attached to that task.

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `gate` | string | **yes** | Gate category: `fmt`, `check`, `clippy`, `test`, or `custom` |
| `command` | string | **yes** | Shell command to run |
| `cwd` | string | no | Working directory within workspace |
| `timeout_ms` | integer | no | Timeout (default: 120000, min: 1000, max: 600000) |

**Capabilities:** `[ExecutesCode, RequiresApproval]` — Approval: `Required`

---

## 24. `task_shell_start` — Start Background Shell Task

**Source:** `crates/tui/src/tools/tasks.rs:50`

**Purpose:** Start a long-running shell command in the background and return a
shell `task_id` immediately. Completion is tracked in the task/status surface.

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `command` | string | **yes** | Shell command to execute |
| `cwd` | string | no | Working directory within workspace |
| `timeout_ms` | integer | no | Timeout (min: 1000, max: 600000) |
| `stdin` | string | no | Optional stdin data to send |
| `tty` | boolean | no | Allocate a pseudo-terminal |

**Capabilities:** `[ExecutesCode, RequiresApproval]` — Approval: `Required`

**Notable:** Delegates to `exec_shell` with `background: true`. Sets
`starts_detached_for = true` when a command is provided. Annotates
result metadata with `background: true` and `task_shell: true`.

---

## 25. `task_shell_wait` — Poll Background Shell Task

**Source:** `crates/tui/src/tools/tasks.rs:51`

**Purpose:** Poll a background shell task without blocking the agent
indefinitely. If `gate` is supplied and the shell task has completed, records
structured gate evidence on the active durable task.

### Input Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `task_id` | string | **yes** | Background shell task id (from `task_shell_start` or `exec_shell`) |
| `wait` | boolean | no | Block until completion (default: false) |
| `timeout_ms` | integer | no | Wait timeout (min: 1000, max: 600000) |
| `gate` | string | no | Gate category for evidence: `fmt`, `check`, `clippy`, `test`, `custom` |
| `command` | string | no | Original command (used when recording gate evidence) |

**Capabilities:** `[ReadOnly]` — Approval: `Auto`

**Notable:** When the shell task has completed and a `gate` is provided, the
tool generates a `TaskGateRecord` and attaches it to the active durable task
via `task_updates` metadata. Gate output is also written as a runtime artifact.

---

## 26–29. PR Attempt Tools

**Source:** `crates/tui/src/tools/tasks.rs:562–795`

PR attempt tools capture, list, read, and preflight-check git patches associated
with durable tasks.

### `pr_attempt_record` — Record PR Attempt

**Purpose:** Capture current git diff as a durable PR work attempt with patch
artifact, changed files, and verification notes.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `summary` | string | **yes** | Summary of the attempt |
| `task_id` | string | no | Task to attach to; defaults to active task |
| `attempt_group_id` | string | no | Group identifier |
| `attempt_index` | integer | no | Index within group (min: 1) |
| `attempt_count` | integer | no | Total attempts in group (min: 1) |
| `verification` | string[] | no | Verification notes |

**Capabilities:** `[ReadOnly, RequiresApproval]` — Approval: `Required`

### `pr_attempt_list` — List PR Attempts

**Purpose:** List PR attempts recorded on a durable task.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `task_id` | string | no | Task id; defaults to active task |

**Capabilities:** `[ReadOnly]` — Approval: `Auto`

### `pr_attempt_read` — Read PR Attempt

**Purpose:** Read one recorded PR attempt and its patch artifact reference.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `task_id` | string | no | Task id; defaults to active task |
| `attempt_id` | string | **yes** | Attempt identifier |

**Capabilities:** `[ReadOnly]` — Approval: `Auto`

### `pr_attempt_preflight` — Preflight PR Attempt Patch

**Purpose:** Run `git apply --check` for a recorded attempt patch. This is a
no-mutation preflight; actual apply remains explicit and approval-gated
elsewhere.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `attempt_id` | string | **yes** | Attempt identifier |
| `task_id` | string | no | Task id; defaults to active task |

**Returns:** `{attempt_id, patch_path, would_apply: bool, exit_code, stdout_summary, stderr_summary, mutated_worktree: false}`.

**Capabilities:** `[ReadOnly]` — Approval: `Auto`

---

## Internal Infrastructure (Not a Tool)

### Approval Cache (`approval_cache.rs`)

The approval cache is **not a tool** — it does not implement `ToolSpec` and is
not registered in the tool registry. It is internal infrastructure that
fingerprints tool calls to cache approval decisions within a session.

**How it works:**
- Each tool call is fingerprinted via `build_approval_key` (exact match for
  denials) or `build_approval_grouping_key` (lossy match for approvals).
- Fingerprint shapes vary by tool category:
  - **File writers**: `file:<tool_name>:<hash of args>`
  - **Shell tools**: `shell:<tool_name>:<hash of args>` (exact) / `shell:<command prefix>` (grouping)
  - **Network fetchers**: `net:<hostname>`
  - **Everything else**: `tool:<tool_name>:<hash of input>`
- Entries carry an `approved_for_session` flag: when `true`, the approval is
  reused for the remainder of the session; when `false`, it is a one-shot grant.
- The cache is keyed by `ApprovalKey` (a SHA-256 digest) and stored in memory
  only (no persistence).
