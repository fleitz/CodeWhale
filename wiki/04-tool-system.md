# CodeWhale Tool System Reference

> **Version:** v0.8.62
> **Source:** `crates/tui/src/tools/` and `crates/tools/src/lib.rs`

---

## Part 1: Tool Infrastructure

### Architecture Overview

CodeWhale's tool system is split across two crates:

| Crate | Role |
|-------|------|
| `codewhale-tools` (`crates/tools/src/lib.rs`) | Core types: `ToolSpec` (data), `ToolHandler`, `ToolRegistry`, `ToolCallRuntime`, `ToolCallSource`, `FunctionCallError` |
| `tui` (`crates/tui/src/tools/`) | Live implementations: the `ToolSpec` trait (behavior), `ToolContext`, `ToolRegistry` (TUI-flavor), and all ~50 concrete tool structs |

### Core Types

#### `ToolSpec` trait (behavior — `crates/tui/src/tools/spec.rs:736`)

Every tool implements this trait:

```rust
#[async_trait]
pub trait ToolSpec: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;          // JSON Schema for parameters
    fn capabilities(&self) -> Vec<ToolCapability>;
    fn approval_requirement(&self) -> ApprovalRequirement;     // default: derived from capabilities
    fn approval_requirement_for(&self, input: &Value) -> ApprovalRequirement;
    fn is_read_only(&self) -> bool;
    fn is_read_only_for(&self, input: &Value) -> bool;
    fn supports_parallel(&self) -> bool;      // default: false
    fn supports_parallel_for(&self, input: &Value) -> bool;
    fn starts_detached_for(&self, input: &Value) -> bool;   // default: false
    fn defer_loading(&self) -> bool;          // default: false
    fn model_visible(&self) -> bool;          // default: true
    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError>;
}
```

#### `ToolSpec` struct (data — `crates/tools/src/lib.rs:209`)

A serializable specification used by the dispatch-layer registry:

| Field | Type | Description |
|-------|------|-------------|
| `name` | `String` | Unique tool identifier |
| `input_schema` | `Value` | JSON Schema for input parameters |
| `output_schema` | `Value` | JSON Schema for output |
| `supports_parallel_tool_calls` | `bool` | Whether concurrent invocations are allowed |
| `timeout_ms` | `Option<u64>` | Per-call timeout; `None` = no timeout |

#### `ToolCapability` enum (`crates/tools/src/lib.rs:19`)

Flags describing what a tool can do:

| Variant | Meaning |
|---------|---------|
| `ReadOnly` | Only reads data, never modifies state |
| `WritesFiles` | Writes to the filesystem |
| `ExecutesCode` | Executes arbitrary shell commands |
| `Network` | Makes network requests |
| `Sandboxable` | Can be run in a sandbox |
| `RequiresApproval` | Requires user approval before execution |

#### `ApprovalRequirement` enum (`crates/tools/src/lib.rs:36`)

| Level | Meaning |
|-------|---------|
| `Auto` | Never needs approval (safe read-only operations) |
| `Suggest` | Suggest approval but allow skip |
| `Required` | Always require explicit user approval |

Default derivation: `ExecutesCode` → `Required`, `WritesFiles` → `Suggest`, otherwise `Auto`.

#### `ToolError` enum (`crates/tools/src/lib.rs:48`)

| Variant | Description |
|---------|-------------|
| `InvalidInput { message }` | Input validation failure |
| `MissingField { field }` | Required field not provided |
| `PathEscape { path }` | Path escapes workspace boundary |
| `ExecutionFailed { message }` | Runtime execution failure |
| `Timeout { seconds }` | Operation timed out |
| `NotAvailable { message }` | Tool or dependency not available |
| `PermissionDenied { message }` | Authorization failure |

#### `ToolResult` struct (`crates/tools/src/lib.rs:109`)

| Field | Type | Description |
|-------|------|-------------|
| `content` | `String` | Output content (JSON or plain text) |
| `success` | `bool` | Whether execution was successful |
| `metadata` | `Option<Value>` | Optional structured metadata |

#### `ToolCallSource` enum (`crates/tools/src/lib.rs:237`)

| Variant | Description |
|---------|-------------|
| `Direct` | Direct invocation from model or user |
| `JsRepl` | Invocation through JavaScript REPL environment |

#### `FunctionCallError` enum (`crates/tools/src/lib.rs:306`)

Covers dispatch-layer problems (distinct from `ToolError`):

| Variant | Description |
|---------|-------------|
| `ToolNotFound { name }` | No tool registered under that name |
| `KindMismatch { expected, got }` | Payload kind doesn't match handler |
| `MutatingToolRejected { name }` | Mutating tool blocked when `allow_mutating=false` |
| `TimedOut { name, timeout_ms }` | Execution exceeded timeout |
| `Cancelled { name }` | Execution was cancelled |
| `ExecutionFailed { name, error }` | Handler returned an error |

### `ToolRegistry` — Two Implementations

There are **two** registry types:

1. **`codewhale_tools::ToolRegistry`** (`crates/tools/src/lib.rs:396`): The dispatch-layer registry. Maps tool names to `ToolHandler` trait objects. Owns a `ToolCallRuntime` for concurrency control. Used by the engine to validate and dispatch tool calls.

2. **`tui::tools::ToolRegistry`** (`crates/tui/src/tools/registry.rs:29`): The TUI-layer registry. Maps tool names to `Arc<dyn ToolSpec>`. Used to build the model-visible tool catalog and execute tools within the TUI context. Features:
   - `register(tool)`, `get(name)`, `execute(name, input)`, `execute_full(name, input)`
   - `to_api_tools()` — converts all tools to API `Tool` format for the model
   - Memoised API cache via `OnceLock<Vec<Tool>>`
   - Large-output routing (#548) through `LargeOutputRouter`

### `ToolCallRuntime` (`crates/tools/src/lib.rs:357`)

RW-lock concurrency model:
- **Parallel-safe tools** acquire a **read lock** — multiple concurrent executions allowed
- **Serial tools** acquire a **write lock** — exclusive access only
- **Reentrant calls** (tool invoking another tool) skip locking to avoid deadlock

### `ToolContext` (`crates/tui/src/tools/spec.rs:115`)

The execution context passed to every tool's `execute` method. Key fields:

| Field | Type | Description |
|-------|------|-------------|
| `workspace` | `PathBuf` | Workspace root directory |
| `shell_manager` | `SharedShellManager` | Background task and streaming IO |
| `trust_mode` | `bool` | Allow paths outside workspace |
| `auto_approve` | `bool` | YOLO mode — skip safety checks |
| `shell_policy` | `ShellPolicy` | Effective shell execution policy |
| `features` | `Features` | Feature flag set |
| `network_policy` | `Option<NetworkPolicyDecider>` | Per-domain network policy |
| `runtime` | `RuntimeToolServices` | Durable services (tasks, automations, handles, RLM sessions) |
| `cancel_token` | `Option<CancellationToken>` | Engine turn cancellation |
| `sandbox_backend` | `Option<Arc<dyn SandboxBackend>>` | External sandbox routing |
| `memory_path` | `Option<PathBuf>` | User memory file path |
| `lsp_manager` | `Option<Arc<LspManager>>` | Post-edit diagnostics injection |
| `large_output_router` | `Option<LargeOutputRouter>` | Large-result synthesis routing |
| `search_provider` | `SearchProvider` | Web search backend selection |

### `RuntimeToolServices` (`crates/tui/src/tools/spec.rs:49`)

Optional durable services attached to the tool context:

| Field | Type | Description |
|-------|------|-------------|
| `shell_manager` | `Option<SharedShellManager>` | Shell process management |
| `task_manager` | `Option<SharedTaskManager>` | Durable task CRUD |
| `automations` | `Option<SharedAutomationManager>` | Scheduled automation CRUD |
| `task_data_dir` | `Option<PathBuf>` | Task storage directory |
| `active_task_id` | `Option<String>` | Currently active durable task |
| `active_thread_id` | `Option<String>` | Currently active thread |
| `dynamic_tool_executor` | `Option<Arc<dyn DynamicToolExecutor>>` | Dynamic/MCP tool dispatch |
| `hook_executor` | `Option<Arc<HookExecutor>>` | Shell-env hook injection |
| `handle_store` | `SharedHandleStore` | `var_handle` backing store |
| `rlm_sessions` | `SharedRlmSessionStore` | Persistent RLM kernels |

### Input Helpers (`crates/tools/src/lib.rs:158-201`)

| Function | Signature | Description |
|----------|-----------|-------------|
| `required_str` | `(&Value, &str) -> Result<&str, ToolError>` | Extract required string; lists provided fields on failure |
| `optional_str` | `(&Value, &str) -> Option<&str>` | Extract optional string |
| `required_u64` | `(&Value, &str) -> Result<u64, ToolError>` | Extract required u64 |
| `optional_u64` | `(&Value, &str, default: u64) -> u64` | Extract optional u64 with default |
| `optional_bool` | `(&Value, &str, default: bool) -> bool` | Extract optional bool with default |

---

## Part 2: Complete Tool Catalog

### 1. `agent` — Spawn Sub-Agent

**Source:** `subagent/mod.rs`

**Purpose:** Spawn a background sub-agent with a filtered toolset that inherits workspace configuration from the main session.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `prompt` | string | **yes** | Focused task for the child agent |
| `type` | string | no | Sub-agent type: `general`, `explore`, `plan`, `review`, `implementer`, `verifier`, `custom` |
| `role` | string | no | Role alias (must match `type` if both given) |
| `cwd` | string | no | Working directory for the child; must be inside workspace |
| `model` | string | no | Exact provider model id for the child |
| `model_strength` | string | no | `same` or `faster` |
| `thinking` | string | no | Thinking budget: `inherit`, `auto`, `off`, `low`, `medium`, `high`, `max` |
| `max_depth` | integer | no | Nested-agent depth budget (0–3) |
| `fork_context` | boolean | no | Whether to include parent context prefix |
| `name` | string | no | Optional stable session name |

**Returns:** `ToolResult` with sub-agent session snapshot metadata (agent_id, name, status, transcript_handle, artifacts).

**Capabilities:** `[ReadOnly]` — Approval: `Auto`
**Notable:** Sub-agents run with a filtered toolset. Each sub-agent gets a whale-species nickname. Agent lifecycle is tracked in `~/.deepseek/state/subagents.v1.json`. Step count is unbounded by default (`u32::MAX`).

---

### 2. `rlm_open` — Open Persistent Python REPL

**Source:** `rlm.rs:91`

**Purpose:** Load content (file, inline, URL, or session object) into a named Python kernel and return a metadata handle.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | no | Caller-chosen context name (default: slug from source) |
| `file_path` | string | no* | Workspace-relative file to load |
| `content` | string | no* | Inline content (capped at 200k chars) |
| `url` | string | no* | HTTP/HTTPS URL to fetch and load |
| `session_object` | string | no* | Symbolic ref from `rlm_session_objects` (e.g. `session://active/system_prompt`) |

\* Exactly one of `file_path`, `content`, `url`, or `session_object` required.

**Returns:** `ToolResult` with metadata: `name`, `length`, `preview`, `sha256`.

**Capabilities:** `[ReadOnly, Network, ExecutesCode]` — Approval: `Auto`

---

### 3. `rlm_eval` — Evaluate Python in REPL

**Source:** `rlm.rs` (second half)

**Purpose:** Run one Python REPL block against a named RLM context.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | **yes** | RLM context name from `rlm_open` |
| `code` | string | **yes** | Raw Python code to execute |

**Returns:** `ToolResult` with bounded stdout/stderr projection plus metadata. Large stdout (>1K chars) is stored as a `var_handle` retrievable via `handle_read`.

**Capabilities:** `[ReadOnly, ExecutesCode]` — Approval: `Auto`

---

### 4. `rlm_configure` — Configure RLM Session

**Source:** `rlm.rs`

**Purpose:** Adjust runtime settings for a named RLM context.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | **yes** | RLM context name |
| `output_feedback` | string | no | `full` or `metadata` |
| `sub_query_timeout_secs` | integer | no | Child query timeout |
| `sub_rlm_max_depth` | integer | no | Recursive sub-RLM depth (0–3) |
| `share_session` | boolean | no | Explicit session sharing toggle |

**Returns:** `ToolResult` confirmation.

**Capabilities:** `[ReadOnly]` — Approval: `Auto`

---

### 5. `rlm_close` — Close RLM Session

**Source:** `rlm.rs`

**Purpose:** Close a named RLM context, tear down its Python kernel, and return usage/lifecycle metadata.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | **yes** | RLM context name from `rlm_open` |

**Returns:** `ToolResult` with session metadata and lifecycle summary.

**Capabilities:** `[ReadOnly, ExecutesCode]` — Approval: `Auto`

---

### 6. `rlm_session_objects` — List RLM Session Objects

**Source:** `rlm.rs:37`

**Purpose:** List active prompt/history/session symbolic objects as compact cards for use with `rlm_open`.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| *(none)* | | | No parameters |

**Returns:** `ToolResult` JSON with `objects` array and `open_with` instructions.

**Capabilities:** `[ReadOnly]` — Approval: `Auto` — Supports parallel: **yes**

---

### 7. `read_file` — Read Workspace File

**Source:** `file.rs:23`

**Purpose:** Read a UTF-8 file from the workspace, with auto-detection for PDFs and image OCR.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | **yes** | Path to file (relative to workspace or absolute) |
| `start_line` | integer | no | Starting line (1-based, default 1) |
| `max_lines` | integer | no | Max lines to return (default 200, max 500) |
| `pages` | string | no | PDF only: page range e.g. `"1-5"` or `"10"` |

**Returns:** `ToolResult` with numbered, line-tagged content. If `truncated="true"`, use `next_start_line` to continue. PDFs are auto-extracted. PNG/JPEG images are OCR-extracted.

**Capabilities:** `[ReadOnly, Sandboxable]` — Approval: `Auto` — Supports parallel: **yes**

---

### 8. `write_file` — Create/Overwrite File

**Source:** `file.rs:440`

**Purpose:** Create or overwrite a UTF-8 file in the workspace. Parent directories are auto-created.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | **yes** | Path to the file |
| `content` | string | **yes** | Content to write |

**Returns:** `ToolResult` with a unified diff of changes and a summary line. LSP diagnostics are auto-injected for the written file when LSP is enabled (#428).

**Capabilities:** `[WritesFiles, Sandboxable, RequiresApproval]` — Approval: `Suggest`

---

### 9. `edit_file` — Single Search/Replace Edit

**Source:** `file.rs:579`

**Purpose:** Replace text in a single file via exact search/replace, with automatic fuzzy fallback.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | **yes** | Path to the file |
| `search` | string | **yes** | Exact text to find (including whitespace, indentation, newlines) |
| `replace` | string | **yes** | Text to replace with |
| `fuzz` | boolean | no | **Deprecated.** Fuzzy fallback is now automatic. |

**Returns:** `ToolResult` with a compact unified diff. Three-stage matching: (1) exact match, (2) indentation-tolerant fuzzy match, (3) typographic-punctuation normalization (smart quotes, em-dashes, NBSP).

**Capabilities:** `[WritesFiles, Sandboxable, RequiresApproval]` — Approval: `Suggest`

---

### 10. `apply_patch` — Multi-Hunk Patch

**Source:** `apply_patch.rs:97`

**Purpose:** Apply a unified-diff patch (multi-hunk, multi-file) with fuzzy matching and transactional semantics.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | no | Path to the file to patch |
| `patch` | string | no | Unified diff patch content |
| `changes` | string | no | Alternative: inline changes |
| `fuzz` | integer | no | Max lines of context for fuzzy matching (default: 3, max: 50) |
| `dry_run` | boolean | no | When true, validate without writing |

**Returns:** `ToolResult` with `PatchResult`: files_applied, hunks_applied, fuzz_used, touched_files, file_summaries.

**Capabilities:** `[WritesFiles, Sandboxable, RequiresApproval]` — Approval: `Suggest`

---

### 11. `exec_shell` — Run Shell Command

**Source:** `shell.rs:2121`

**Purpose:** Execute a shell command in the workspace with foreground/background modes, TTY support, and sandbox integration.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `command` | string | **yes** | The shell command to execute |
| `timeout_ms` | integer | no | Timeout in ms (default: 120000, max: 600000) |
| `background` | boolean | no | Run in background and return task_id (default: false) |
| `interactive` | boolean | no | Run interactively with terminal IO |
| `stdin` | string | no | Optional stdin data (non-interactive only) |
| `cwd` | string | no | Optional working directory |
| `tty` | boolean | no | Allocate pseudo-terminal (implies background) |
| `combined_output` | boolean | no | Capture stdout+stderr as one PTY stream |

**Returns:** `ToolResult` with exit_code, stdout, stderr, duration_ms, sandbox metadata. Background jobs return immediately with a `task_id`.

**Capabilities:** `[ExecutesCode, Sandboxable, RequiresApproval]` — Approval: `Required` (downgraded to `Auto` for parallel-readonly commands via `approval_requirement_for`)

---

### 12. `grep_files` — Regex Search

**Source:** `search.rs:42`

**Purpose:** Search workspace files with a regex pattern; respects `.gitignore`.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `pattern` | string | **yes** | Regular expression pattern |
| `path` | string | no | Directory/file to search (default: `.`) |
| `include` | string[] | no | Glob patterns for files to include |
| `exclude` | string[] | no | Glob patterns for files to exclude |
| `context_lines` | integer | no | Context lines before/after each match (default: 2) |
| `case_insensitive` | boolean | no | Case-insensitive matching (default: false) |
| `max_results` | integer | no | Max results to return (default: 100) |

**Returns:** `ToolResult` JSON with `matches` array, `total_matches`, `files_searched`, `truncated`.

**Capabilities:** `[ReadOnly, Sandboxable]` — Approval: `Auto` — 30s timeout, 10MB max file size

---

### 13. `file_search` — Filename Search

**Source:** `file_search.rs:29`

**Purpose:** Find files by name using fuzzy matching with score-based ranking.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `query` | string | **yes** | Search query (file name or path fragment) |
| `path` | string | no | Base path to search (default: workspace) |
| `limit` | integer | no | Max results (default: 20, max: 200) |
| `extensions` | string[] | no | File extensions to filter by (e.g. `["rs", "md"]`) |
| `exclude` | string[] | no | Glob patterns to exclude |

**Returns:** `ToolResult` JSON array of `{path, name, score}`.

**Capabilities:** `[ReadOnly, Sandboxable]` — Approval: `Auto` — 30s timeout

---

### 14. `list_dir` — Directory Listing

**Source:** `file.rs:859`

**Purpose:** List entries in a directory relative to the workspace.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | no | Relative path (default: `.`) |

**Returns:** `ToolResult` JSON with directory entries (name, is_dir).

**Capabilities:** `[ReadOnly, Sandboxable]` — Approval: `Auto` — Supports parallel: **yes** — 30s timeout

---

### 15. `git_status` — Git Porcelain Status

**Source:** `git.rs:26`

**Purpose:** Run `git status --porcelain=v1 -b` in the workspace.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | no | Optional subdirectory or file to scope to |

**Returns:** `ToolResult` with stdout truncated at 40,000 chars, plus metadata (command, working_dir, pathspec, truncated).

**Capabilities:** `[ReadOnly, Sandboxable]` — Approval: `Auto` — Supports parallel: **yes**

---

### 16. `git_diff` — Git Diff

**Source:** `git.rs:107`

**Purpose:** Run `git diff` with sensible defaults and safe truncation.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | no | Subdirectory/file to scope to |
| `cached` | boolean | no | Diff staged changes (`--cached`) |
| `unified` | integer | no | Context lines (default: 3, max: 50) |

**Returns:** `ToolResult` with diff stdout (truncated at 40,000 chars) plus metadata.

**Capabilities:** `[ReadOnly, Sandboxable]` — Approval: `Auto` — Supports parallel: **yes**

---

### 17. `git_show` — Show Revision

**Source:** `git_history.rs:146`

**Purpose:** Run `git show` for a specific revision with optional patch and stats.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `rev` | string | **yes** | Revision (commit SHA, tag, branch, or ref expression) |
| `path` | string | no | Optional subdirectory/file scope |
| `patch` | boolean | no | Include patch hunks (default: true) |
| `stat` | boolean | no | Include `--stat` summary (default: true) |
| `unified` | integer | no | Context lines for patch (default: 3, max: 50) |

**Returns:** `ToolResult` with truncated stdout and metadata.

**Capabilities:** `[ReadOnly, Sandboxable]` — Approval: `Auto` — Supports parallel: **yes**

---

### 18. `git_log` — Commit History

**Source:** `git_history.rs:29`

**Purpose:** Run `git log` with optional path and author/date filters.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | no | Subdirectory or file to scope to |
| `max_count` | integer | no | Max commits (default: 20, max: 200) |
| `author` | string | no | Git author filter |
| `since` | string | no | Lower date bound (e.g. `"2 weeks ago"`) |
| `until` | string | no | Upper date bound |

**Returns:** `ToolResult` with truncated log output and metadata.

**Capabilities:** `[ReadOnly, Sandboxable]` — Approval: `Auto` — Supports parallel: **yes**

---

### 19. `git_blame` — Line Blame

**Source:** `git_history.rs:263`

**Purpose:** Run `git blame` on a file with optional revision and line-range controls.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | **yes** | Path to a tracked file within workspace |
| `rev` | string | no | Revision to blame against (default: HEAD) |
| `start_line` | integer | no | First line to include (default: 1) |
| `max_lines` | integer | no | Max lines to include (default: 200, max: 2000) |
| `porcelain` | boolean | no | Emit `--line-porcelain` output |

**Returns:** `ToolResult` with truncated blame output and metadata.

**Capabilities:** `[ReadOnly, Sandboxable]` — Approval: `Auto` — Supports parallel: **yes**

---

### 20. `web_search` — Web Search

**Source:** `web_search.rs:136`

**Purpose:** Search the web via multiple configurable backends (DuckDuckGo, Bing, Tavily, Bocha, Metaso, Baidu, Volcengine, Sofya).

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `query` | string | **yes*** | Search query |
| `q` | string | no | Alias for `query` |
| `search_query` | object[] | no | Array form: `[{"q":"...", "max_results":5}]` |
| `max_results` | integer | no | Max results (default: 5, max: 10) |
| `timeout_ms` | integer | no | Timeout in ms (default: 15000, max: 60000) |

\* One of `query`, `q`, or `search_query[0].q` required.

**Returns:** `ToolResult` JSON with `query`, `source`, `count`, `results` (title, url, snippet).

**Capabilities:** `[ReadOnly, Network]` — Approval: `Auto` — Supports parallel: **yes**

---

### 21. `fetch_url` — HTTP Fetch

**Source:** `fetch_url.rs:86`

**Purpose:** Fetch a known URL directly (HTTP GET), with HTML-to-text conversion and DNS rebinding protection.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `url` | string | **yes** | Absolute HTTP/HTTPS URL |
| `format` | string | no | `markdown` (default), `text`, or `raw` |
| `max_bytes` | integer | no | Truncate after N bytes (default: 1,000,000; hard max: 10,485,760) |
| `timeout_ms` | integer | no | Request timeout (default: 15000, max: 60000) |
| `fields` | string[] | no | JSONPath projections for JSON responses |

**Returns:** `ToolResult` JSON with `url`, `status`, `headers`, `content_type`, `content`, `truncated`.

**Capabilities:** `[ReadOnly, Network]` — Approval: `Auto` — Max 5 redirects followed

---

### 22. `checklist_write` — Replace Checklist

**Source:** `todo.rs:194`

**Purpose:** Replace the active thread/task checklist. Also exposed as `checklist_add`, `checklist_update`, `checklist_list`.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `todos` | object[] | **yes** | Complete list of todo items |
| `todos[].content` | string | **yes** | Task description |
| `todos[].status` | string | **yes** | `pending`, `in_progress`, or `completed` |

**Returns:** `ToolResult` with snapshot (items, completion_pct, in_progress_id).

**Capabilities:** `[WritesFiles]` — Approval: `Auto`

---

### 23. `update_plan` — Update Plan Metadata

**Source:** `plan.rs:250` (approx)

**Purpose:** Update high-level strategy metadata for complex initiatives.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `title` | string | no | Short plan title |
| `objective` | string | no | What the plan aims to accomplish |
| `context_summary` | string | no | Current state summary |
| `explanation` | string | no | High-level explanation |
| `sources_used` | string[] | no | Evidence sources |
| `critical_files` | string[] | no | Repo paths likely to be edited |
| `constraints` | string[] | no | Hard requirements |
| `recommended_approach` | string | no | Implementation strategy |
| `verification_plan` | string | no | Tests/checks expected |
| `risks_and_unknowns` | string | no | Known risks or blockers |
| `handoff_packet` | string | no | Continuation notes |
| `plan` | object[] | no | Plan steps: `[{step, status}]` |

**Returns:** `ToolResult` with a PlanSnapshot.

**Capabilities:** `[WritesFiles]` — Approval: `Auto`

---

### 24. `run_tests` — Run Cargo Tests

**Source:** `test_runner.rs:23`

**Purpose:** Run `cargo test` in the workspace root.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `args` | string | no | Extra arguments (shell-style) |
| `all_features` | boolean | no | Include `--all-features` |

**Returns:** `ToolResult` JSON with `success`, `exit_code`, `stdout`, `stderr`, `command`. Output truncated at 40,000 chars. Cargo failure summary included in metadata when applicable.

**Capabilities:** `[ExecutesCode, Sandboxable]` — Approval: `Required`

---

### 25. `run_verifiers` — Run Verification Gates

**Source:** `verifier.rs:30`

**Purpose:** Run independent verifier gates in parallel across detected Rust, Node, Python, and Go projects.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `profile` | string | no | Ecosystem: `auto`, `rust`, `node`, `python`, `go` (default: `auto`) |
| `level` | string | no | `quick` or `full` (default: `quick`) |
| `max_python_files` | integer | no | Max Python files to syntax-check (default: 200) |
| `commands` | object[] | no | Custom verifier gates: `[{name, program, args[], cwd?}]` |
| `background` | boolean | no | Start as background shell jobs |

**Returns:** `ToolResult` JSON with `success`, `profile`, `level`, `gate_count`, `passed`, `failed`, `skipped`, `summary`, per-gate `gates[]` results.

**Capabilities:** `[ExecutesCode]` — Approval: `Required`

---

### 26. `task_create` — Create Durable Task

**Source:** `tasks.rs:45`

**Purpose:** Create/enqueue a durable background task through TaskManager.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `prompt` | string | **yes** | Work prompt for the durable task |
| `model` | string | no | Model to use |
| `workspace` | string | no | Workspace path (default: current) |
| `mode` | string | no | `agent`, `plan`, or `yolo` |
| `allow_shell` | boolean | no | Allow shell execution |
| `trust_mode` | boolean | no | Trust mode |
| `auto_approve` | boolean | no | Auto-approve mode |

**Returns:** `ToolResult` with task record (id, status, prompt preview).

**Capabilities:** `[RequiresApproval]` — Approval: `Required`

---

### 27. `task_list` — List Durable Tasks

**Source:** `tasks.rs:46`

**Purpose:** List recent durable tasks with status, linked thread/turn ids, and summaries.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `limit` | integer | no | Max tasks to return (default: 20, min: 1, max: 100) |

**Returns:** `ToolResult` JSON with `summary` and `tasks[]`.

**Capabilities:** `[ReadOnly]` — Approval: `Auto`

---

### 28. `task_read` — Read Task Detail

**Source:** `tasks.rs:47`

**Purpose:** Read durable task detail including timeline, checklist, gate evidence, artifacts, and PR attempts.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `task_id` | string | **yes** | Full task id or unambiguous prefix |

**Returns:** `ToolResult` with full task record.

**Capabilities:** `[ReadOnly]` — Approval: `Auto`

---

### 29. `task_shell_start` — Start Background Shell

**Source:** `tasks.rs:50`

**Purpose:** Start a long-running shell command in the background and return a shell task_id immediately.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `command` | string | **yes** | Shell command to execute |
| `cwd` | string | no | Working directory |
| `stdin` | string | no | Optional stdin data |
| `timeout_ms` | integer | no | Timeout in ms (max: 600000) |
| `tty` | boolean | no | Allocate pseudo-terminal |

**Returns:** `ToolResult` with `task_id`, `status`, command echo.

**Capabilities:** `[ExecutesCode]` — Approval: `Required`

---

### 30. `task_shell_wait` — Poll Background Shell

**Source:** `tasks.rs:51`

**Purpose:** Poll a background shell task without blocking indefinitely. Optionally records gate evidence on the active durable task.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `task_id` | string | **yes** | Background shell task id |
| `timeout_ms` | integer | no | Wait timeout (default: 30000, max: 600000) |
| `wait` | boolean | no | Block until completion (default: false) |
| `command` | string | no | Original command (for gate evidence recording) |
| `gate` | string | no | Gate category for evidence: `fmt`, `check`, `clippy`, `test`, `custom` |

**Returns:** `ToolResult` with incremental output and exit status.

**Capabilities:** `[ExecutesCode]` — Approval: `Auto`

---

### 31. `handle_read` — Read var_handle Projection

**Source:** `handle.rs:173`

**Purpose:** Read a bounded projection from a `var_handle` returned by tools like RLM sessions or sub-agents.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `handle` | object \| string | **yes** | A `var_handle` object or compact `session_id/name` string |
| `slice` | object | no* | Char/line slice: `{start, end, unit?}` ("chars" or "lines") |
| `range` | object | no* | One-based line range: `{start, end}` |
| `count` | boolean | no* | Return metadata counts |
| `jsonpath` | string | no* | JSONPath projection: `$`, `.field`, `[index]`, `[*]`, `['field']` |
| `introspect` | boolean | no* | Return supported projections and size hints |
| `max_chars` | integer | no | Max chars to return (default: 12000, hard cap: 50000) |

\* Exactly one projection type required.

**Returns:** `ToolResult` with the bounded projection content.

**Capabilities:** `[ReadOnly]` — Approval: `Auto`

---

### 32. `note` — Append Agent Note

**Source:** `remember.rs:21` (registered as `note`)

**Purpose:** Append a durable note to the agent notes file.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `content` | string | **yes** | The note content to append |

**Returns:** `ToolResult` success confirmation.

**Capabilities:** `[WritesFiles]` — Approval: `Auto` — Writes to `notes_path` from `ToolContext` (typically `notes.md` in project state dir).

---

### 33. `remember` — Append User Memory

**Source:** `remember.rs:21`

**Purpose:** Append a durable note to the user memory file (`~/.deepseek/memory.md`). Only registered when `[memory] enabled = true`.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `note` | string | **yes** | Single-sentence durable note |

**Returns:** `ToolResult` success with `"remembered: ..."` content.

**Capabilities:** `[WritesFiles]` — Approval: `Auto`

---

### 34. `validate_data` — Validate JSON/TOML

**Source:** `validate_data.rs:16`

**Purpose:** Validate JSON or TOML content from inline input or a workspace file.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `path` | string | no* | Path to a file within workspace |
| `content` | string | no* | Inline content to validate |
| `format` | string | no | `auto` (default), `json`, or `toml` |

\* Exactly one of `path` or `content` required.

**Returns:** `ToolResult` with validation status and metadata. In `auto` mode, infers format from extension, falls back to trying both parsers.

**Capabilities:** `[ReadOnly, Sandboxable]` — Approval: `Auto` — Supports parallel: **yes**

---

### 35. `request_user_input` — Ask User Questions

**Source:** `user_input.rs:107`

**Purpose:** Ask the user 1–3 short questions and return their selections.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `questions` | object[] | **yes** | Array of 1–3 questions |
| `questions[].header` | string | **yes** | Question header |
| `questions[].id` | string | **yes** | Question identifier |
| `questions[].question` | string | **yes** | Question text |
| `questions[].options` | object[] | **yes** | 2–4 options: `[{label, description}]` |
| `questions[].allow_free_text` | boolean | no | Offer free-text "Other" response (default: false) |
| `questions[].multi_select` | boolean | no | Allow multiple selections (default: false) |

**Returns:** `ToolResult` with `answers[]` — each answer has `id`, `label`, `value`.

**Capabilities:** `[ReadOnly]` — Approval: `Auto`
**Notable:** The actual user interaction is handled by the engine; the tool's `execute` returns an error directing to engine handling.

---

### 36. `code_execution` / `js_execution` — Execute Code

**Source:** `js_execution.rs`

**Purpose:** Execute model-provided JavaScript via local Node.js runtime. (Python code execution follows the same pattern via the `code_execution` tool registered in the engine's deferred-tool dispatcher.)

**`js_execution` parameters:**

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `code` | string | **yes** | JavaScript source code to execute |

**Returns:** `ToolResult` JSON: `{type: "code_execution_result", stdout, stderr, return_code, content}`.

**Notable:** Tool is only advertised when Node.js is present on the host. 120-second timeout. Tempfile with `.js` extension is used.

**Capabilities:** `[ExecutesCode]` — Approval: `Required`

---

## Additional Tools

The following tools are also registered in the CodeWhale tool system but are not covered in the primary catalog above. Brief summaries:

| Tool | Source | Purpose |
|------|--------|---------|
| `project_map` | `project.rs` | Get a high-level map of project structure with tree view and key files |
| `review` | `review.rs` | Run a structured code review for a file, git diff, or GitHub PR |
| `pandoc_convert` | `pandoc.rs` | Convert documents between formats via pandoc |
| `image_ocr` | `image_ocr.rs` | Extract text from images (PNG, JPEG, TIFF) via local OCR |
| `speech` / `tts` | `speech.rs` | Generate speech/audio via configured TTS API |
| `revert_turn` | `revert_turn.rs` | Roll back workspace files to a snapshot before a recent turn |
| `diagnostics` | `diagnostics.rs` | Report workspace info, git detection, sandbox availability, and Rust toolchain |
| `finance` | `finance.rs` | Fetch live market quotes for stocks, ETFs, or crypto tickers |
| `load_skill` | `skill.rs` | Load a skill (SKILL.md body + companion file list) into the next turn |
| `web_run` | `web_run.rs` | Open/control a browser for web automation workflows |
| `notify` | `notify.rs` | Send desktop notifications |
| `github_*` | `github.rs` | GitHub issue/PR management (comment, close, read context) |
| `pr_attempt_*` | `tasks.rs` | PR attempt recording, listing, reading, preflight |
| `task_cancel` | `tasks.rs` | Cancel a queued or running durable task |
| `task_gate_run` | `tasks.rs` | Run an approved verification gate command |
| `automation_*` | `automation.rs` | Create, read, update, delete, list, pause, resume, run durable automations |
| `checklist_add` | `todo.rs` | Add one checklist item |
| `checklist_update` | `todo.rs` | Update one checklist item's status |
| `checklist_list` | `todo.rs` | List current checklist progress |
| `exec_shell_interact` | `shell.rs` | Send input to a background shell task |
| `exec_shell_cancel` | `shell.rs` | Cancel running background shell tasks |
| `retrieve_tool_result` | `tool_result_retrieval.rs` | Retrieve previously spilled large tool results |
