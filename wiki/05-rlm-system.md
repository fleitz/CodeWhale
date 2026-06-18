# 5 — RLM System (Recursive Language Model)

**Source files cited:**
- `crates/tui/src/tools/rlm.rs` (971 lines)
- `crates/tui/src/tools/handle.rs` (927 lines)
- `crates/tui/src/rlm/mod.rs` (46 lines)
- `crates/tui/src/rlm/bridge.rs` (556 lines)
- `crates/tui/src/rlm/session.rs` (541 lines)
- `crates/tui/src/rlm/prompt.rs` (201 lines)
- `crates/tui/src/rlm/turn.rs` (995 lines)
- `crates/tui/src/repl/runtime.rs` (1486 lines)

---

## 1. Concept

**RLM** (Recursive Language Model) is CodeWhale's system for persistent Python REPL
sessions that handle large-context work without copying the full payload into the
parent LLM transcript.

The core insight (from Zhang, Kraska & Khattab, arXiv:2512.24601, §2 Algorithm 1):

```text
state ← InitREPL(prompt=P)
state ← AddFunction(state, sub_RLM)
hist ← [Metadata(state)]
while True:
    code ← LLM(hist)
    (state, stdout) ← REPL(state, code)
    hist ← hist ∥ code ∥ Metadata(stdout)
    if state[Final] is set:
        return state[Final]
```

The long input `P` is held **only** as a REPL variable (`_context`). It never
appears in the root LLM's context window. The root LLM sees only compact
metadata — length, preview, prior-round summaries — and emits Python code
blocks that inspect or delegate sub-work. This keeps the parent transcript lean
and makes unbounded-context work practical.

> `crates/tui/src/rlm/mod.rs:1-26` — module-level doc comment documents the
> paper-spec algorithm and invariants.

---

## 2. The 5 RLM Tools

CodeWhale exposes RLM through five tool functions: `rlm_session_objects`,
`rlm_open`, `rlm_eval`, `rlm_configure`, and `rlm_close`. The tool structs are
defined in `crates/tui/src/tools/rlm.rs`.

### 2.1 `rlm_session_objects`

**Purpose:** List the active session's symbolic objects (system prompt,
transcript, individual messages) as compact cards. Each card includes an `id`
that can be passed to `rlm_open` via the `session_object` parameter.

**Key parameters:** None.

**Returns:** A JSON array of `objects`, each with `id`, `kind`, `title`,
`length`, `preview_500`, and `sha256`. Also includes an `open_with` example.

**Design note:** Large tool results and thinking blocks in the transcript are
redacted into compact metadata; use returned handles and `handle_read` for
bounded payload projections.

> `crates/tui/src/tools/rlm.rs:37-89` — `RlmSessionObjectsTool` definition.

**Available session objects:**

| Object ref | Kind | Description |
|---|---|---|
| `session://active/session` | `session_metadata` | Session id, model, workspace, message count |
| `session://active/system_prompt` | `system_prompt` | The active system prompt text |
| `session://active/transcript` | `transcript` | Full transcript as compact JSONL |
| `session://active/latest_user` | `message` | Latest user message |
| `session://active/messages/N` | `message` | Individual message at index N |

> `crates/tui/src/rlm/session.rs:134-256` — `SessionObjectSnapshot` and its
> resolution logic.

---

### 2.2 `rlm_open`

**Purpose:** Create a named RLM context by loading a source into a persistent
Python kernel. Returns only metadata (name, length, preview, sha256) — the
parent transcript holds a handle, not the body.

**Key parameters** (exactly one source must be provided):

| Parameter | Description |
|---|---|
| `name` | Caller-chosen context name, unique within the parent session. Defaults to a slug derived from the source. |
| `file_path` | Workspace-relative file to load. |
| `content` | Inline content (capped at 200,000 chars). |
| `url` | HTTP/HTTPS URL fetched through `fetch_url`. |
| `session_object` | Stable symbolic ref from `rlm_session_objects` (e.g. `session://active/system_prompt`). |

**What happens:**
1. Source validation — exactly one of `file_path`, `content`, `url`, or
   `session_object` must be non-empty (`rlm.rs:148-174`).
2. Source loading — the body is read from the chosen source
   (`rlm.rs:546-601`).
3. The body is written to a temp file under
   `$TMPDIR/deepseek_rlm_ctx/session_<pid>_<uuid>.txt`
   (`rlm.rs:200`, `session.rs:113-123`).
4. A Python subprocess is spawned with the file path in the `RLM_CONTEXT_FILE`
   environment variable. Python reads the file on bootstrap, loading it into
   the `_context` variable (`rlm.rs:203-204`, `runtime.rs:925-933`).
5. The session is stored in a shared `HashMap<String, Arc<Mutex<RlmSession>>>`
   keyed by name (`rlm.rs:210-211`).

**Returns:** JSON with `name`, `id` (format: `rlm:<uuid>`), `length`, `type`,
`preview_500`, `sha256`.

> `crates/tui/src/tools/rlm.rs:91-223` — `RlmOpenTool` definition.

---

### 2.3 `rlm_eval`

**Purpose:** Execute one Python code block against a named RLM context.
Returns a bounded projection of stdout/stderr plus metadata. If the code calls
`FINAL(value)` / `finalize(value)`, the final value is stored as a `var_handle`.

**Key parameters:**

| Parameter | Description |
|---|---|
| `name` | RLM context name from `rlm_open`. |
| `code` | Raw Python (no markdown fences). The loaded source is in scope as `_context`, `_ctx`, and `content`. |

**What happens:**
1. The session is looked up by name (`rlm.rs:281`).
2. If the session has a kernel, an `RlmBridge` is constructed with the
   configured `sub_rlm_max_depth` (capped at `HARD_SUB_RLM_DEPTH_CAP = 3`)
   (`rlm.rs:292-297`).
3. The code is executed in the Python REPL. During execution, Python may emit
   RPC requests (`llm_query`, `rlm_query`, batch variants) that the bridge
   services (`rlm.rs:299-311`).
4. If `finalize()` / `FINAL()` was called, the value is stored in the
   `HandleStore` as a `var_handle` (`rlm.rs:317-332`).
5. Large stdout/stderr (>1,000 chars) are routed to `var_handle`s instead of
   being inlined (`rlm.rs:340-378`).

**Returns:** JSON with `name`, `id`, `duration_ms`, `rpc_count`, `had_error`,
`new_vars`, optional `final` handle, optional `stdout_preview`,
`stdout_handle`, `stderr_preview`, `stderr_handle`, `confidence`.

> `crates/tui/src/tools/rlm.rs:225-419` — `RlmEvalTool` definition.

---

### 2.4 `rlm_configure`

**Purpose:** Adjust runtime behavior for a named RLM context: output feedback
mode, child query timeout, recursive sub-RLM depth, and session sharing.

**Key parameters:**

| Parameter | Type | Default | Description |
|---|---|---|---|
| `name` | string | required | RLM context name. |
| `output_feedback` | `"full"` or `"metadata"` | `"full"` | When `"metadata"`, stdout/stderr are omitted from eval responses. |
| `sub_query_timeout_secs` | integer (1–600) | 120 | Per-child completion timeout. |
| `sub_rlm_max_depth` | integer (0–3) | 1 | Recursion budget for nested `sub_rlm` calls (hard-capped at 3). |
| `share_session` | boolean | false | Whether the session is shareable across agents. |

**Returns:** JSON with `name` and `current_config`.

> `crates/tui/src/tools/rlm.rs:421-484` — `RlmConfigureTool` definition.
> `crates/tui/src/rlm/session.rs:94-111` — `RlmSessionConfig` struct.

---

### 2.5 `rlm_close`

**Purpose:** Tear down a named RLM context: remove it from the session store,
shut down its Python kernel, and return usage/lifecycle metadata.

**Key parameters:**

| Parameter | Description |
|---|---|
| `name` | RLM context name from `rlm_open`. |

**What happens:**
1. The session is removed from the shared store (`rlm.rs:515-518`).
2. The kernel is extracted and shut down (`rlm.rs:526,538-540`).

**Returns:** JSON with `name`, `id`, `rpc_count`, `total_duration_ms`,
`peak_var_count`, `created_ms_ago`, `context_path`.

> `crates/tui/src/tools/rlm.rs:486-544` — `RlmCloseTool` definition.

---

## 3. Session Lifecycle

### 3.1 Creation (`rlm_open`)

When `rlm_open` is called:
1. The source body is loaded and validated.
2. A temp context file is written.
3. `PythonRuntime::spawn_with_context()` spawns a new Python subprocess.
   The bootstrap script (`runtime.rs:553-981`) initializes the REPL loop,
   loads `_context` from the file, defines all helper functions, and enters
   the `_main_loop()`.
4. An `RlmSession` struct is created and stored in the shared session map.

> `crates/tui/src/rlm/session.rs:25-64` — `RlmSession` struct.

### 3.2 Evaluation (`rlm_eval`)

Each `rlm_eval` call:
1. Looks up the session.
2. Constructs an `RlmBridge` if an LLM client is available.
3. Sends the code block to the Python REPL via stdin, framed by
   `__RLM_RUN_<sid>__` / `__RLM_END_<sid>__` sentinels.
4. During execution, Python may emit RPC requests on stdout
   (`__RLM_REQ_<sid>__::{json}`). The bridge dispatches these and writes
   responses back on stdin (`__RLM_RESP_<sid>__::{json}`).
5. When the block finishes, Python emits `__RLM_DONE_<sid>__::<round_id>`.
   If `FINAL` was called, a `__RLM_FINAL_<sid>__::{json}` line is also emitted.
6. The Rust side parses stdout, stderr, final values, and error state into a
   `ReplRound` struct.

> `crates/tui/src/repl/runtime.rs:38-61` — `ReplRound` struct.
> `crates/tui/src/repl/runtime.rs:948-978` — `_main_loop()`.

### 3.3 Configuration (`rlm_configure`)

Configuration is applied directly to the `RlmSession.config` field
(`RlmSessionConfig`). Changes take effect on the next `rlm_eval` call. The
`sub_rlm_max_depth` is hard-capped at `HARD_SUB_RLM_DEPTH_CAP = 3`.

> `crates/tui/src/tools/rlm.rs:35` — `HARD_SUB_RLM_DEPTH_CAP`.

### 3.4 Closure (`rlm_close`)

The session is removed from the shared store and its kernel is shut down.
Attempting `rlm_eval` on a closed session returns an error:
`"rlm_eval: context \`{name}\` is closed"`.

> `crates/tui/src/tools/rlm.rs:285-289`.

### 3.5 The `RlmBridge` Struct

The `RlmBridge` is the RPC dispatcher that services `llm_query` / `rlm_query`
calls coming back from Python. It lives for the duration of one `rlm_eval` call.

```
RlmBridge {
    client:       Arc<dyn RlmLlmClient>,   // LLM client trait object
    child_model:  String,                   // e.g. "deepseek-v4-flash"
    depth_remaining: u32,                   // recursion budget
    usage:        Arc<Mutex<Usage>>,        // cumulative token tracking
}
```

> `crates/tui/src/rlm/bridge.rs:59-80` — `RlmBridge` struct and constructor.

The bridge implements `RpcDispatcher`, routing four request types:

| RPC Request Type | Dispatches to |
|---|---|
| `Llm { prompt, model, max_tokens, system }` | `dispatch_llm` — one-shot child LLM call |
| `LlmBatch { prompts, model, dependency_mode, safety_note }` | `dispatch_llm_batch` — parallel LLM calls |
| `Rlm { prompt, model }` | `dispatch_rlm` — recursive sub-RLM |
| `RlmBatch { prompts, model, dependency_mode, safety_note }` | `dispatch_rlm_batch` — parallel recursive sub-RLMs |

> `crates/tui/src/rlm/bridge.rs:282-321` — `RpcDispatcher` impl.

Key invariants:
- The `model` parameter from Python is **ignored**; child calls are pinned to
  the configured child model (`DEFAULT_CHILD_MODEL = "deepseek-v4-flash"`)
  (`bridge.rs:86-96`, `rlm.rs:26`).
- Per-child timeout: 120 seconds (`bridge.rs:28`).
- Default `max_tokens` for children: 4096 (`bridge.rs:30`).
- Max batch size: 16 (`bridge.rs:32`).
- Batch requests require `dependency_mode="independent"` (or
  `"parallel_safe"` / `"map_reduce"`) (`bridge.rs:258-279`).

---

## 4. In-REPL Helpers

These Python functions are available inside every RLM REPL session. They are
defined in the bootstrap template at `crates/tui/src/repl/runtime.rs:553-981`.

### 4.1 Input Inspection

| Helper | Signature | Description |
|---|---|---|
| `context_meta()` | `() → dict` | Returns `{chars, lines, preview, tail_preview}` — never the full text. |
| `peek(start, end, unit="chars")` | `(int, int, str) → str` | Bounded slice by char offsets or line numbers. |
| `search(pattern, max_hits=100)` | `(str, int) → list[dict]` | Regex search returning hit records with `{index, start, end, match, snippet}`. |
| `chunk(max_chars=20000, overlap=0)` | `(int, int) → list[dict]` | Full-coverage chunks with `{index, start, end, text}`. |
| `chunk_context(max_chars=20000, overlap=0)` | — | Compatibility alias for `chunk()`. |
| `chunk_coverage(chunks)` | `(list[dict]) → dict` | Coverage report: `{chunks, input_chars, covered_chars, gaps, complete}`. |

> `runtime.rs:795-900`.

### 4.2 Child LLM / Sub-RLM Calls

| Helper | Signature | Description |
|---|---|---|
| `llm_query(prompt, model=None, max_tokens=None, system=None)` | `(str, ...) → str` | One-shot child LLM. `model` is ignored by Rust. |
| `llm_query_batched(prompts, model=None, dependency_mode=None, safety_note=None)` | `(list[str], ...) → list[str]` | Parallel fan-out. Requires `dependency_mode='independent'`. |
| `rlm_query(prompt, model=None)` | `(str, ...) → str` | Recursive sub-RLM. `model` is ignored by Rust. |
| `rlm_query_batched(prompts, model=None, dependency_mode=None, safety_note=None)` | `(list[str], ...) → list[str]` | Parallel recursive sub-RLMs. Requires `dependency_mode='independent'`. |
| `sub_query(prompt, slice=None)` | `(str, dict?) → str` | One child call, optionally scoped to a bounded slice. |
| `sub_query_batch(prompt, slices, dependency_mode=None, safety_note=None)` | `(str, list[dict], ...) → list[str]` | Apply one prompt to many independent slices concurrently. |
| `sub_query_map(prompts, slices=None, dependency_mode=None, safety_note=None)` | `(list[str], list[dict]?, ...) → list[str]` | N distinct independent prompts, optionally paired with N slices. |
| `sub_query_sequence(prompt, slices, carry_prompt=None)` | `(str, list[dict], str?) → list[str]` | Sequential dependent calls — each result feeds the next step. |
| `sub_rlm(prompt, source=None)` | `(str, dict?) → str` | Recursive sub-RLM for sub-tasks needing their own decomposition. |

> `runtime.rs:583-751`.

### 4.3 Session Control

| Helper | Signature | Description |
|---|---|---|
| `finalize(value, confidence=None)` | `(any, float?) → any` | Signal the final answer; emits `__RLM_FINAL__::{json}`. Sets `final_answer`, `final_confidence`, `final_result` globals. |
| `FINAL(value)` | `(any) → None` | Legacy compatibility alias for `finalize(value)`. |
| `FINAL_VAR(name)` | `(str) → None` | Legacy alias for `finalize(repl_get(name))`. |
| `evaluate_progress()` | `() → dict` | Returns `{has_final_answer, final_confidence, user_variables}`. |
| `SHOW_VARS()` | `() → dict` | Returns `{name: type_name}` for all user variables (excludes bootstrap internals). |
| `repl_get(name, default=None)` | `(str, any?) → any` | Read a variable from the global namespace. |
| `repl_set(name, value)` | `(str, any) → None` | Write a variable into the global namespace. |

> `runtime.rs:767-921`.

### 4.4 Context Variables

The loaded input is available as:
- `_context` — the canonical variable (always present).
- `_ctx` and `content` — compatibility aliases set equal to `_context`.

> `runtime.rs:925-933`.

> **Note:** There is no `context` or `ctx` variable. Use `_context` or the
> bounded helpers (`peek`, `search`, `chunk`, `context_meta`). The system prompt
> explicitly tests for this (`prompt.rs:157-166`).

---

## 5. Batch Helpers and `dependency_mode`

Batch helpers (`llm_query_batched`, `rlm_query_batched`, `sub_query_batch`,
`sub_query_map`) execute multiple child calls concurrently using
`futures_util::join_all`. They enforce a **dependency safety gate**:

### 5.1 `dependency_mode = "independent"`

Accepted values: `"independent"`, `"parallel_safe"`, `"map_reduce"`.

When set, the batch proceeds as parallel fan-out. Each prompt is dispatched
concurrently with no ordering guarantees.

> `crates/tui/src/rlm/bridge.rs:258-279` — `batch_guard()`.

### 5.2 Rejected Modes

Values like `"sequential"`, `"dependent"`, `"ordered"`, `"chain"`, `"serial"`
are rejected with an error directing the user to `sub_query_sequence(...)`.

Missing or unrecognized `dependency_mode` is also rejected.

> `runtime.rs:598-610` — `_batch_dependency_error()`.

### 5.3 Sequential Execution

For dependent work (where step B consumes step A's output), use:
- `sub_query_sequence(prompt, slices, carry_prompt=None)` — iterates through
  slices one at a time, feeding each child result + carry prompt into the next
  step's prompt.
- An explicit Python `for` loop calling `sub_query(prompt, slice=s)`.

> `runtime.rs:728-746` — `sub_query_sequence()` implementation.

### 5.4 Batch Size Limit

Maximum 16 prompts per batch. Exceeding this returns one error per prompt slot.

> `crates/tui/src/rlm/bridge.rs:32` — `MAX_BATCH`.

---

## 6. `var_handle` / `handle_read`

### 6.1 What `var_handle` Is

A `var_handle` is a compact symbolic reference that points to a large payload
stored in the `HandleStore`. Instead of copying the full payload into the parent
transcript, tools (RLM sessions, sub-agents) return a `var_handle` record.

```json
{
  "kind": "var_handle",
  "session_id": "rlm:abc123",
  "name": "final_1",
  "type": "str",
  "length": 15234,
  "repr_preview": "The answer is...",
  "sha256": "abcdef..."
}
```

> `crates/tui/src/tools/handle.rs:33-43` — `VarHandle` struct.

The `HandleStore` is a `HashMap<HandleKey, HandleRecord>` where each record
holds either `HandleValue::Text(String)` or `HandleValue::Json(Value)`.

> `crates/tui/src/tools/handle.rs:112-171` — `HandleStore`.

### 6.2 The `handle_read` Tool

`handle_read` retrieves a **bounded projection** from a `var_handle`. It
accepts exactly one projection type:

| Projection | Input | Description |
|---|---|---|
| `slice` | `{start, end?, unit?}` | Zero-based half-open slice over chars or lines. |
| `range` | `{start, end}` | One-based inclusive line range. |
| `count` | `true` | Character, line, and byte counts. |
| `jsonpath` | `"$..."` | Small JSONPath subset: `$`, `.field`, `[index]`, `[*]`, `['field']`. |
| `introspect` | `true` | Returns supported projections, size hints, and copy-pasteable examples. |

Parameters:
- `max_chars`: defaults to 12,000, hard-capped at 50,000.

> `crates/tui/src/tools/handle.rs:173-308` — `HandleReadTool`.

### 6.3 Handle Input Formats

`handle_read` accepts two forms of handle reference:
1. **Full var_handle object** — the JSON object with `kind`, `session_id`, `name`, etc.
2. **Compact string** — `"session_id/name"` (e.g., `"rlm:abc123/final_1"`).

It rejects artifact refs (`art_...`), tool-call ids (`call_...`), SHA refs, or
file paths — those should use `retrieve_tool_result` or `read_file`.

> `crates/tui/src/tools/handle.rs:331-369` — `parse_handle()`.

### 6.4 How Handles Keep the Parent Transcript Lean

When `rlm_eval` produces stdout/stderr exceeding 1,000 characters, the full
body is stored as a `var_handle` in the `HandleStore`. The tool result only
includes a short inline note (`"N chars; retrieve via handle_read"`) and the
handle object. The model retrieves the full content via `handle_read` only when
needed.

> `crates/tui/src/tools/rlm.rs:34` — `STDOUT_HANDLE_THRESHOLD_CHARS`.
> `crates/tui/src/tools/rlm.rs:340-378` — `route_output()`.

Similarly, `finalize()` / `FINAL()` results are always returned as handles, not
inlined.

---

## 7. `sub_rlm` Recursion

### 7.1 Recursion Budget

The hard depth cap for sub-RLM recursion is **3**:

> `crates/tui/src/tools/rlm.rs:35`:
> ```rust
> const HARD_SUB_RLM_DEPTH_CAP: u32 = 3;
> ```

The default configured depth is 1 (`RlmSessionConfig::default()` sets
`sub_rlm_max_depth: 1`). Callers can raise it up to 3 via `rlm_configure`.

> `crates/tui/src/rlm/session.rs:102-111`.

### 7.2 How Recursion Works

When `sub_rlm(prompt, source)` is called from Python:
1. Python emits an `RpcRequest::Rlm { prompt, model }` on stdout.
2. The bridge's `dispatch_rlm()` checks `self.depth_remaining`:
   - **If > 0:** A recursive `run_rlm_turn_inner` call is made with
     `depth_remaining - 1`. The nested turn spawns its own Python REPL,
     its own bridge, and runs the full RLM algorithm.
   - **If == 0:** The request gracefully degrades to a one-shot
     `dispatch_llm` (plain child completion), matching the paper's behavior.
3. The result is returned to the calling Python code.

> `crates/tui/src/rlm/bridge.rs:179-223` — `dispatch_rlm()`.

### 7.3 Recursion Architecture

The bridge → turn → bridge cycle is broken by type erasure:
`run_rlm_turn_inner` returns a `Pin<Box<dyn Future<Output = RlmTurnResult>>>`,
avoiding the infinite type recursion that would otherwise occur.

> `crates/tui/src/rlm/turn.rs:137-155` — `run_rlm_turn_inner()` signature.

Each nested turn:
- Spawns its own Python REPL with its own context.
- Creates its own `RlmBridge` with the decremented depth budget.
- Runs up to `MAX_RLM_ITERATIONS = 25` rounds.
- Returns an `RlmTurnResult` with the answer and accumulated usage.

Parent bridge consumption of child usage:
```rust
// fold bridge usage (children + nested sub_rlm) into totals
let bridge_usage = usage_handle.lock().await;
let mut final_usage = result.usage.clone();
super::add_usage_with_prompt_cache(&mut final_usage, &bridge_usage);
```

> `crates/tui/src/rlm/turn.rs:508-511`.

---

## 8. RLM vs Sub-Agents Comparison

| Dimension | RLM | Sub-Agents |
|---|---|---|
| **Purpose** | Persistent Python REPL for large-context computation and map-reduce over a single document/input. | Independent child LLM processes for parallel task decomposition. |
| **Runtime** | One long-lived Python subprocess. Code is `exec()`'d into a shared global namespace. | Ephemeral child LLM sessions — each sub-agent is a separate LLM invocation. |
| **Communication** | Code execution over stdin/stdout pipes. Python → Rust RPC for child LLM calls. Results flow back as Python return values. | Tool calls and mailbox. Sub-agents use the same tool surface as the parent; results are reported as structured output. |
| **State** | Persistent Python globals across rounds (variables, imports, file handles). | Stateless — each invocation is independent. |
| **Depth limit** | 3 (`HARD_SUB_RLM_DEPTH_CAP`). Degrades to plain LLM calls at depth 0. | 3 (same cap, independently configurable). |
| **Child model** | Pinned to `"deepseek-v4-flash"` (`DEFAULT_CHILD_MODEL`). Model parameter from Python is ignored. | Configurable per invocation (`model` / `model_strength`). |
| **Batch support** | `sub_query_batch` with `dependency_mode='independent'` (max 16). Sequential variant via `sub_query_sequence`. | Child agents can be spawned in parallel; no built-in batch primitive. |
| **Use cases** | Document analysis, chunked map-reduce, regex search over large inputs, coverage-gated synthesis, structured data extraction. | Parallel code review, multi-file exploration, independent task fan-out. |

---

## 9. Session Persistence

### 9.1 In-Memory Store

RLM sessions live in a shared in-memory store:

```rust
pub type SharedRlmSessionStore = Arc<Mutex<HashMap<String, Arc<Mutex<RlmSession>>>>>;
```

> `crates/tui/src/rlm/session.rs:17`.

Each session is keyed by its caller-chosen `name`. The store is held on the
`ToolContext` runtime and survives across tool calls within a parent session.

### 9.2 `RlmSession` Fields

```
RlmSession {
    name:            String,           // caller-chosen name
    id:              String,           // "rlm:<uuid>"
    kernel:          Option<PythonRuntime>,  // None after close
    context_meta:    ContextMeta,      // length, type, preview, sha256
    config:          RlmSessionConfig, // output_feedback, timeouts, depth, sharing
    rpc_count:       u32,             // cumulative sub-LLM calls
    total_duration:  Duration,        // cumulative eval time
    peak_var_count:  usize,           // high-water mark of Python vars
    final_count:     usize,           // number of finalize() calls
    created_at:      Instant,
    last_used_at:    Instant,
    context_path:    PathBuf,         // temp file holding the original body
}
```

> `crates/tui/src/rlm/session.rs:25-38`.

### 9.3 Context File

The loaded body is written to `$TMPDIR/deepseek_rlm_ctx/session_<pid>_<uuid>.txt`.
The Python REPL reads it on bootstrap via the `RLM_CONTEXT_FILE` environment
variable. The file persists for the lifetime of the session; the path is tracked
in `RlmSession.context_path`.

> `crates/tui/src/rlm/session.rs:113-123` — `write_context_file()`.

### 9.4 Handle Store

The `HandleStore` is also in-memory and shared across the parent session:

```rust
pub type SharedHandleStore = Arc<Mutex<HandleStore>>;
```

Handles from closed RLM sessions remain readable as long as the parent session
is alive. There is no automatic cleanup of handles on `rlm_close`.

> `crates/tui/src/tools/handle.rs:26-31`.

### 9.5 No Disk Serialization (Current)

As of v0.8.33, RLM sessions are **not** serialized to disk. They live only for
the duration of the parent agent session. Session sharing (`share_session:
true`) is a configuration field but its cross-agent semantics are not yet
fully implemented in the serialization layer.

---

## 10. Key Constants Summary

| Constant | Value | Location | Description |
|---|---|---|---|
| `HARD_SUB_RLM_DEPTH_CAP` | 3 | `rlm.rs:35` | Max sub-RLM recursion depth |
| `DEFAULT_CHILD_MODEL` | `"deepseek-v4-flash"` | `rlm.rs:26` | Child LLM model for sub-queries |
| `MAX_INLINE_CONTENT_CHARS` | 200,000 | `rlm.rs:27` | Max inline content for `rlm_open` |
| `STDOUT_HANDLE_THRESHOLD_CHARS` | 1,000 | `rlm.rs:34` | Threshold for handle routing |
| `CHILD_TIMEOUT_SECS` | 120 | `bridge.rs:28` | Per-child LLM timeout |
| `DEFAULT_CHILD_MAX_TOKENS` | 4096 | `bridge.rs:30` | Default max_tokens for children |
| `MAX_BATCH` | 16 | `bridge.rs:32` | Max prompts per batch RPC |
| `MAX_RLM_ITERATIONS` | 25 | `turn.rs:24` | Max RLM loop iterations |
| `MAX_CONSECUTIVE_NO_CODE` | 3 | `turn.rs:28` | Max consecutive rounds without `repl` fence |
| `ROOT_MAX_TOKENS` | 4096 | `turn.rs:30` | Max output tokens for root LLM |
| `ROOT_TEMPERATURE` | 0.3 | `turn.rs:36` | Temperature for root LLM calls |
| `DEFAULT_MAX_CHARS` (handle_read) | 12,000 | `handle.rs:21` | Default max chars for handle projections |
| `HARD_MAX_CHARS` (handle_read) | 50,000 | `handle.rs:22` | Hard cap for handle projections |
| `ROUND_TIMEOUT` | 180s | `runtime.rs:144` | Per-round execution timeout (inline REPL only) |
| `SPAWN_READY_TIMEOUT` | 10s (30s Windows) | `runtime.rs:146-148` | Bootstrap ready signal timeout |
