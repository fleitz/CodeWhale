# CodeWhale Sub-Agent System

This page covers the complete sub-agent system: the `agent` tool, spawn lifecycle, agent types, structs, concurrency, cancellation, persistence, the whale name system, inter-agent mailboxes, and TUI integration.

---

## 1. The `agent` Tool

The entire sub-agent system is exposed to the model through a single tool: `agent`.

**Source:** `crates/tui/src/tools/subagent/mod.rs:3018-3110`

### Purpose

```
Start one focused child agent task. Use this only for independent work
that benefits from a clean context. The child runs in the background
and reports back automatically when finished; keep tiny reads/searches
local. Returns a session projection with the generated agent_id and
transcript_handle for UI/debug inspection.
```

### Tool Schema Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `prompt` | string | **Yes** | Focused task for the child. Prefer a compact Subagent Brief with `QUESTION`, `SCOPE`, `ALREADY_KNOWN`, `EFFORT`, `STOP_CONDITION`, `OUTPUT`. |
| `type` | string | No | Sub-agent type. See §2 Agent Types below. |
| `name` | string | No | Optional stable session name. Defaults to the generated `agent_id`. |
| `model_strength` | `"same"` \| `"faster"` | No | Child model strength. `same` = as capable as the parent; `faster` = smaller/faster same-family sibling. |
| `model` | string | No | Exact provider model id. Overrides `model_strength`. |
| `thinking` | `"inherit"` \| `"auto"` \| `"off"` \| `"low"` \| `"medium"` \| `"high"` \| `"max"` | No | Child thinking budget. Default: `inherit` (follows parent). |
| `cwd` | string | No | Optional working directory; must be inside the parent workspace. |
| `fork_context` | boolean | No | `false` (default): fresh child context. `true`: include parent context prefix. |
| `max_depth` | integer | No | Optional remaining nested-agent depth budget (0–3). Defaults to the configured runtime budget. |

**Source:** `crates/tui/src/tools/subagent/mod.rs:3032-3078`

---

## 2. Agent Types

Six agent types are defined in the `SubAgentType` enum, plus `Custom`.

**Source:** `crates/tui/src/tools/subagent/mod.rs:401-426`

| Type | Canonical Name | Accepted Aliases | Description |
|------|----------------|------------------|-------------|
| **General** | `general` | `general-purpose`, `general_purpose`, `worker`, `default` | Full tool access for multi-step tasks. The default type. |
| **Explore** | `explore` | `exploration`, `explorer` | Read-only tools for fast codebase search. Defaults to `model_strength: "faster"`. |
| **Plan** | `plan` | `planning`, `planner`, `awaiter` | Analysis tools for architectural planning. |
| **Review** | `review` | `code-review`, `code_review`, `reviewer` | Read + analysis tools for code review. |
| **Implementer** | `implementer` | `implement`, `implementation`, `builder` | Writing/patching code to satisfy a specific change. Push-hard on landing the change cleanly. |
| **Verifier** | `verifier` | `verify`, `verification`, `validator`, `tester` | Running test suites and validation gates, reporting pass/fail with evidence. |
| **Custom** | `custom` | — | Custom tool access defined at spawn time. |

**Parsing** (`SubAgentType::from_str`): `crates/tui/src/tools/subagent/mod.rs:428-444`

### Tool Allowlists (deprecated)

Each type had a default tool allowlist (deprecated since v0.6.6 in favor of full parent registry inheritance):

- **General**: `read_file`, `write_file`, `edit_file`, `apply_patch`, `exec_shell`, `grep_files`, `file_search`, `web_search`, checklist tools, `note`, `update_plan` (line 486–511)
- **Explore**: `read_file`, `grep_files`, `file_search`, `exec_shell`, `web_search` (line 512–524)
- **Plan**: `read_file`, `grep_files`, `file_search`, `note`, `update_plan`, checklist tools (line 525–541)
- **Review**: `read_file`, `grep_files`, `file_search`, `note` (line 542)
- **Implementer**: `read_file`, `write_file`, `edit_file`, `apply_patch`, `exec_shell`, checklist tools, `update_plan` (line 543–566)
- **Verifier**: `read_file`, `exec_shell`, `run_tests`, `run_verifiers`, `diagnostics` (line 567–582)

---

## 3. SpawnRequest Struct

The internal representation of a spawn request, parsed from the `agent` tool's JSON input.

**Source:** `crates/tui/src/tools/subagent/mod.rs:1229-1255`

```rust
struct SpawnRequest {
    session_name: Option<String>,           // stable session name
    prompt: String,                          // the child's task description
    agent_type: SubAgentType,                // one of the 7 types
    assignment: SubAgentAssignment,          // { objective, optional role }
    allowed_tools: Option<Vec<String>>,      // explicit tool allowlist (Custom roles)
    model: Option<String>,                   // exact provider model id
    model_strength: SubAgentModelStrength,   // Same | Faster
    thinking: SubAgentThinking,              // Inherit | Auto | Effort(ReasoningEffort)
    cwd: Option<PathBuf>,                    // working directory (must be inside workspace)
    resident_file: Option<String>,           // cache-aware resident mode file path
    fork_context: bool,                      // seed child with parent prefix
    max_depth: Option<u32>,                  // legacy recursion budget for descendants
}
```

### SubAgentAssignment

**Source:** `crates/tui/src/tools/subagent/mod.rs:388-399`

```rust
struct SubAgentAssignment {
    objective: String,
    role: Option<String>,
}
```

### Model Strength Enum

**Source:** `crates/tui/src/tools/subagent/mod.rs:1170-1196`

```rust
enum SubAgentModelStrength {
    Same,    // aliases: inherit, parent, current
    Faster,  // aliases: fast, smaller, small, lower, cheap, flash
}
```

Default: `Explore` type → `Faster`; all others → `Same`.

### Thinking Budget Enum

**Source:** `crates/tui/src/tools/subagent/mod.rs:1198-1221`

```rust
enum SubAgentThinking {
    Inherit,                                  // aliases: parent, same, current
    Auto,                                     // aliases: automatic
    Effort(ReasoningEffort),                  // Off, Low, Medium, High, Max
}
```

---

## 4. Recursion Depth Model

Sub-agents can spawn their own children, forming a tree. A depth cap prevents unbounded fanout.

### Constants

**Source:** `crates/config/src/lib.rs:1338-1343`

```rust
pub const DEFAULT_SPAWN_DEPTH: u32 = 3;
pub const MAX_SPAWN_DEPTH_CEILING: u32 = 3;
```

**Source:** `crates/tui/src/tools/subagent/mod.rs:1333`

```rust
pub const DEFAULT_MAX_SPAWN_DEPTH: u32 = codewhale_config::DEFAULT_SPAWN_DEPTH;
```

### Depth Fields on SubAgentRuntime

**Source:** `crates/tui/src/tools/subagent/mod.rs:1388-1399`

```
spawn_depth: u32       // 0 = top-level, 1 = direct child, etc.
max_spawn_depth: u32   // hard cap on recursion depth
```

### would_exceed_depth

**Source:** `crates/tui/src/tools/subagent/mod.rs:1640-1644`

```rust
pub fn would_exceed_depth(&self) -> bool {
    self.spawn_depth + 1 > self.max_spawn_depth
}
```

When the limit is exceeded, the spawn is rejected with:

```
Sub-agent depth limit reached (current depth N, max M).
Increase via [runtime] max_spawn_depth in config.toml.
```

### Depth Inheritance

When a child runtime is created (`child_runtime()`), `spawn_depth` is incremented by 1 and `max_spawn_depth` is preserved from the parent.

**Source:** `crates/tui/src/tools/subagent/mod.rs:1611-1638`

```
ASCII Diagram:

Depth 0:  Root Engine (spawn_depth=0, max_spawn_depth=3)
              │
              ├── Child A (spawn_depth=1, max_spawn_depth=3)
              │       │
              │       └── Grandchild A1 (spawn_depth=2, max_spawn_depth=3)
              │               │
              │               └── Great-grandchild (spawn_depth=3, max_spawn_depth=3)
              │                       └── [REJECTED: spawn_depth+1=4 > 3]
              │
              └── Child B (spawn_depth=1, max_spawn_depth=3)
```

The model-visible `max_depth` parameter on the `agent` tool clamps to `[0, MAX_SPAWN_DEPTH_CEILING]` (0–3). It overrides the default for that specific child.

**Source:** `crates/tui/src/tools/subagent/mod.rs:4586-4607`

---

## 5. Child Runtime Inheritance

### `child_runtime()` — Turn-bound children

**Source:** `crates/tui/src/tools/subagent/mod.rs:1611-1638`

Creates a child runtime that inherits from the parent:

| Field | Inheritance |
|-------|-------------|
| `client` | Cloned (same DeepSeekClient) |
| `model` | Cloned |
| `auto_model` | Cloned |
| `reasoning_effort` | Cloned |
| `role_models` | Cloned |
| `context` | Cloned (auto_approve preserved) |
| `allow_shell` | Cloned |
| `worker_profile` | Cloned |
| `event_tx` | Cloned |
| `manager` | Cloned (shared across all depths) |
| `spawn_depth` | **Incremented by 1** |
| `parent_agent_id` | Cloned |
| `max_spawn_depth` | Cloned |
| `cancel_token` | **Derived as child token** (`parent.child_token()`) |
| `mailbox` | Cloned |
| `parent_completion_tx` | Cloned |
| `fork_context` | Cloned |
| `mcp_pool` | Cloned |
| `step_api_timeout` | Cloned |
| `speech_output_dir` | Cloned |
| `todos` | Cloned (shared todo list) |

### `background_runtime()` — Detached agent sessions

**Source:** `crates/tui/src/tools/subagent/mod.rs:1594-1599`

Background agents use `background_runtime()`, which:
1. Calls `child_runtime()` for all inheritable fields
2. **Replaces the cancellation token with a fresh one** so the child outlives the parent turn
3. Explicit agent cancellation still aborts via the manager

### SubAgentRuntime full struct

**Source:** `crates/tui/src/tools/subagent/mod.rs:1370-1434`

```rust
pub struct SubAgentRuntime {
    pub client: DeepSeekClient,
    pub model: String,
    pub auto_model: bool,
    pub reasoning_effort: Option<String>,
    pub reasoning_effort_auto: bool,
    pub role_models: HashMap<String, String>,
    pub context: ToolContext,
    pub allow_shell: bool,
    pub worker_profile: WorkerRuntimeProfile,
    pub event_tx: Option<mpsc::Sender<Event>>,
    pub manager: SharedSubAgentManager,
    pub spawn_depth: u32,
    pub parent_agent_id: Option<String>,
    pub max_spawn_depth: u32,
    pub cancel_token: CancellationToken,
    pub mailbox: Option<Mailbox>,
    pub parent_completion_tx: Option<mpsc::UnboundedSender<SubAgentCompletion>>,
    pub fork_context: Option<SubAgentForkContext>,
    pub mcp_pool: Option<Arc<Mutex<McpPool>>>,
    pub step_api_timeout: Duration,
    pub speech_output_dir: Option<PathBuf>,
    pub todos: SharedTodoList,
}
```

---

## 6. `fork_context` and Prefix Caching

### What It Is

When `fork_context: true`, the child agent receives a byte-identical copy of the parent's system prompt and message prefix before the child's own task instructions are appended. This allows DeepSeek's server-side prefix cache to reuse the already-warmed prefix, dramatically reducing latency and cost for the child's first turn.

### SubAgentForkContext

**Source:** `crates/tui/src/tools/subagent/mod.rs:1357-1361`

```rust
pub struct SubAgentForkContext {
    pub system: Option<SystemPrompt>,           // parent's system prompt
    pub messages: Vec<Message>,                 // parent's prior messages
    pub structured_state_block: Option<String>, // optional state block
}
```

### How It Works

1. When `fork_context: true`, the child's initial messages are NOT just the child prompt.
2. Instead, the child receives: `[parent system prompt] + [parent messages] + [fork_state block] + [subagent_context block] + [child assignment]`
3. The parent system prompt and messages are kept **byte-identical** to maximize DeepSeek prefix-cache reuse.
4. The `fork_context` travels through `SubAgentRuntime` and is available to all descendants.

**Source:** `crates/tui/src/tools/subagent/mod.rs:3301-3338` (`build_initial_subagent_messages`)

### When to Use

From the constitution (`constitution.md:529`):

> Use `fork_context: true` when multiple perspectives should share the same parent context: the runtime preserves the parent prefill/prompt prefix byte-identically where available so DeepSeek prefix-cache reuse stays high.

Default is `false` (fresh child context).

---

## 7. Whale Name System

Sub-agents are assigned friendly nicknames drawn from the Cetacea infraorder — baleen whales, toothed whales, and select dolphins.

### The 60 Species

**Source:** `crates/tui/src/tools/subagent/mod.rs:170-273`

The full list is interleaved English / Simplified Chinese for roughly even distribution:

```
Blue, 蓝鲸, Humpback, 座头鲸, Sperm, 抹香鲸, Fin, 长须鲸, Sei, 塞鲸,
Bryde's, 布氏鲸, Minke, 小须鲸, Antarctic Minke, 南极小须鲸,
Pygmy Right, 小露脊鲸, Omura's, 大村鲸, Eden's, 艾氏鲸, Rice's, 赖斯鲸,
Gray, 灰鲸, Bowhead, 弓头鲸, North Atlantic Right, 北大西洋露脊鲸,
North Pacific Right, 北太平洋露脊鲸, Southern Right, 南露脊鲸,
Beluga, 白鲸, Narwhal, 独角鲸, Orca, 虎鲸, Pilot, 领航鲸,
False Killer, 伪虎鲸, Pygmy Killer, 小虎鲸, Melon-headed, 瓜头鲸,
Beaked, 喙鲸, Cuvier's Beaked, 柯氏喙鲸, Baird's Beaked, 贝氏喙鲸,
Blainville's Beaked, 柏氏喙鲸, Ginkgo-toothed Beaked, 银杏齿喙鲸,
Strap-toothed, 带齿喙鲸, Stejneger's Beaked, 斯氏喙鲸,
Dwarf Sperm, 小抹香鲸, Pygmy Sperm, 侏儒抹香鲸,
Rough-toothed, 糙齿海豚, Atlantic Spotted, 大西洋斑海豚,
Pantropical Spotted, 热带斑海豚, Spinner, 长吻飞旋海豚,
Clymene, 短吻飞旋海豚, Striped, 条纹海豚,
Common Bottlenose, 宽吻海豚, Indo-Pacific Bottlenose, 印太瓶鼻海豚,
Risso's, 灰海豚, Commerson's, 花斑海豚, Chilean, 智利海豚,
Heaviside's, 海氏矮海豚, Hector's, 赫氏矮海豚,
Amazon River, 亚马逊河豚, Ganges River, 恒河豚, Indus River, 印度河豚,
La Plata, 拉普拉塔河豚, Franciscana, 拉河豚
```

### Deterministic Hash

**Source:** `crates/tui/src/tools/subagent/mod.rs:279-285`

```rust
pub fn whale_name_for_id(id: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    let idx = (hasher.finish() as usize) % WHALE_NICKNAMES.len();
    WHALE_NICKNAMES[idx].to_string()
}
```

The same `agent_id` (UUID) always maps to the same whale name — stable across session restarts for persisted agents.

### Collision Avoidance

**Source:** `crates/tui/src/tools/subagent/mod.rs:291-318`

```rust
pub fn assign_unique_whale_name(id: &str, active_names: &HashSet<String>) -> String
```

If the deterministic name is already in use, a numeric suffix is appended (e.g., `"La Plata (2)"`). The suffix is also derived from the hash for stability.

---

## 8. Completion Events / Sentinel Protocol

When a sub-agent finishes, a `<codewhale:subagent.done>` sentinel is injected into the parent's transcript.

### SubAgentCompletion Struct

**Source:** `crates/tui/src/tools/subagent/mod.rs:1341-1351`

```rust
pub struct SubAgentCompletion {
    pub agent_id: String,    // the completing child's id
    pub payload: String,     // "Human summary\n<cw:subagent.done>"
}
```

### Sentinel Format

From the constitution (`constitution.md:537-557`):

```
Agent name line (e.g., "Beluga completed: ...")
<codewhale:subagent.done>
```

The sentinel carries:
- `agent_id` — child's identifier
- `name` — child's whale name
- `status` — `"completed"` or `"failed"`
- `summary_location` / `error_location`

### Integration Protocol

1. When the parent sees `<codewhale:subagent.done>`, read the summary line immediately before it.
2. Integrate findings — do not re-do what the child already did.
3. For audit detail, use `handle_read` on the transcript handle.
4. If the child failed, assess whether to open a replacement or proceed with a fallback.
5. Update checklists to reflect the contribution.
6. Multiple sentinels may arrive in one turn when children were opened in parallel.

### Routing Path

```
Child completes
    │
    ▼
parent_completion_tx (mpsc::UnboundedSender<SubAgentCompletion>)
    │
    ├── Root children → engine turn loop inbox
    └── Nested children → parent sub-agent's local inbox
```

**Source:** `crates/tui/src/tools/subagent/mod.rs:1408-1414`

---

## 9. SubAgentManager Architecture

The central registry that owns all agent lifecycle.

### Struct

**Source:** `crates/tui/src/tools/subagent/mod.rs:1753-1784`

```rust
pub struct SubAgentManager {
    agents: HashMap<String, SubAgent>,                    // active agent instances
    worker_records: HashMap<String, AgentWorkerRecord>,   // headless worker records
    worker_event_seq: u64,                                // monotonic event counter
    workspace: PathBuf,                                   // project root
    state_path: Option<PathBuf>,                          // persist file location
    max_steps: u32,                                       // default: u32::MAX (unbounded)
    max_agents: usize,                                    // cap (default 20, clamped to MAX_SUBAGENTS)
    running_heartbeat_timeout: Duration,                  // stale-agent detection
    current_session_boot_id: String,                      // "boot_XXXXXXXXXXXX"
    launch_gate: Arc<Semaphore>,                          // concurrency throttle
    last_persist_at: Option<Instant>,                     // debounce bookkeeping
    persist_pending: bool,                                // coalesced write flag
}
```

### Shared Handle

```rust
type SharedSubAgentManager = Arc<RwLock<SubAgentManager>>;
```

All agents at all depths share the **same** manager instance. This means:
- A root engine and its grandchildren all read/write through one `Arc<RwLock<SubAgentManager>>`.
- Cancellation, listing, and persist all go through the same lock.

### SubAgent Struct (per-instance)

**Source:** `crates/tui/src/tools/subagent/mod.rs:1648-1675`

```rust
pub struct SubAgent {
    pub id: String,
    pub session_name: String,
    pub fork_context: bool,
    pub agent_type: SubAgentType,
    pub prompt: String,
    pub assignment: SubAgentAssignment,
    pub model: String,
    pub nickname: Option<String>,
    pub status: SubAgentStatus,
    pub result: Option<String>,
    pub steps_taken: u32,
    pub checkpoint: Option<SubAgentCheckpoint>,
    pub needs_input: Option<SubAgentNeedsInput>,
    pub started_at: Instant,
    pub last_activity_at: Instant,
    pub allowed_tools: Option<Vec<String>>,
    pub session_boot_id: String,
    pub workspace: PathBuf,
    input_tx: Option<mpsc::UnboundedSender<SubAgentInput>>,
    task_handle: Option<JoinHandle<()>>,
}
```

### SubAgentStatus Enum

**Source:** `crates/tui/src/tools/subagent/mod.rs:588-595`

```rust
pub enum SubAgentStatus {
    Running,
    Completed,
    Interrupted(String),    // continuable checkpoint parked
    Failed(String),
    Cancelled,
}
```

### AgentWorkerStatus (Headless)

**Source:** `crates/tui/src/tools/subagent/mod.rs:653-664`

```rust
pub enum AgentWorkerStatus {
    Queued,
    Starting,
    Running,
    WaitingForUser,
    ModelWait,
    RunningTool,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}
```

---

## 10. Mailbox System

The inter-agent communication system based on monotonic sequence-numbered envelopes.

**Source:** `crates/tui/src/tools/subagent/mailbox.rs:1-491`

### MailboxMessage Enum

```rust
pub enum MailboxMessage {
    Started { agent_id, agent_type },
    Progress { agent_id, status },
    ToolCallStarted { agent_id, tool_name, step },
    ToolCallCompleted { agent_id, tool_name, step, ok },
    ChildSpawned { parent_id, child_id },
    Completed { agent_id, summary },
    Failed { agent_id, error },
    Interrupted { agent_id, reason },
    Cancelled { agent_id },
    TokenUsage { agent_id, model, usage },
}
```

### Architecture

```
┌──────────────────────────────────────┐
│             Mailbox                  │
│  ┌────────────────────────────────┐  │
│  │ MailboxInner                   │  │
│  │  tx: mpsc::UnboundedSender     │──┼──► MailboxReceiver
│  │  next_seq: AtomicU64           │  │    (single drainer)
│  │  seq_tx: watch::Sender<u64>    │──┼──► Subscriber A (watch)
│  │  closed: AtomicBool            │──┼──► Subscriber B (watch)
│  │  cancel_token: CancellationToken│  │
│  └────────────────────────────────┘  │
└──────────────────────────────────────┘
```

### Key Properties

1. **Monotonic sequences**: Every message gets a globally-increasing `seq` number. `MailboxEnvelope { seq: u64, message: MailboxMessage }`.
2. **Fanout**: Multiple subscribers can watch the sequence counter via `subscribe()`. Each `recv()` returns when the counter advances.
3. **Close-as-cancel**: Closing the mailbox (`close()`) cancels the bound cancellation token, propagating to all derived child tokens.
4. **Cloneable**: The `Mailbox` is cheaply cloneable (all inner fields are `Arc`/atomic). The entire spawn tree publishes into one ordered stream.

### Receiver Pattern

```rust
// Single drainer
impl MailboxReceiver {
    pub fn has_pending(&mut self) -> bool;
    pub fn drain(&mut self) -> Vec<MailboxEnvelope>;
    pub async fn recv(&mut self) -> Option<MailboxEnvelope>;
}
```

---

## 11. Concurrency Model

### Limits

| Constant | Value | Source |
|----------|-------|--------|
| `MAX_SUBAGENTS` | **20** | `crates/tui/src/config.rs:23` |
| `DEFAULT_MAX_SUBAGENTS` | **20** | `crates/tui/src/config.rs:22` |
| `MAX_AGENT_WORKER_RECORDS` | **256** | `mod.rs:107` |
| `MAX_AGENT_WORKER_EVENTS_PER_RECORD` | **128** | `mod.rs:108` |

### Launch Gate (Semaphore)

**Source:** `crates/tui/src/tools/subagent/mod.rs:1770-1776, 3386-3392`

```rust
launch_gate: Arc<Semaphore>,  // permits = min(launch_concurrency, max_agents)
```

Only **direct (depth-1) children** go through the gate:

```
Parent spawns 25 children at once
    │
    ├── 20 acquire permits immediately → start executing
    └── 5 queue with reason: "queued: waiting for a sub-agent launch slot"
         (acquire permits as running children finish)
```

Deeper descendants (depth ≥ 2) bypass the gate so a permit-holding parent waiting on its own children cannot deadlock the tree.

### Acquisition Flow

**Source:** `crates/tui/src/tools/subagent/mod.rs:3378-3392`

```rust
// Try immediate acquisition
match gate.try_acquire_owned() {
    Ok(permit) => _launch_permit = Some(permit),
    Err(NoPermits) => _launch_permit = acquire_queued_launch_permit(...).await,
    Err(Closed) => proceed without backpressure,
}
```

The launch concurrency is configurable via `[subagents] launch_concurrency`. The default is the full `max_agents` cap, meaning no gating by default.

### Fanout Guidance

From the constitution (`constitution.md:366-372`):

> Up to 20 sub-agents run at once by default. Open one `agent` call per genuinely independent target in the same turn — the dispatcher runs them in parallel — then coordinate as completion events report back.

---

## 12. Cancellation Propagation

### Token Hierarchy

```
Root CancellationToken
    │
    ├── child_token() → Child A
    │       │
    │       └── child_token() → Grandchild A1
    │
    └── child_token() → Child B
```

**Source:** `crates/tui/src/tools/subagent/mod.rs:1611-1638` (`child_runtime`)

### Two Cancellation Paths

1. **Turn-bound children** (`child_runtime`): Share a derived child token. When the parent turn is cancelled (e.g., user presses Esc), all turn-bound children are cancelled recursively.
2. **Background children** (`background_runtime`): Get a **fresh** `CancellationToken`. They survive parent turn cancellation. Explicit agent cancellation aborts them through the manager.

### Close-as-Cancel

When a `Mailbox` is closed:

```rust
pub fn close(&self) {
    if !self.inner.closed.swap(true, Ordering::AcqRel) {
        self.inner.cancel_token.cancel();  // fires the bound token
    }
}
```

**Source:** `mailbox.rs:224-228`

A test verifies propagation across the default depth of 3:

```rust
// root → child → grandchild
let root = CancellationToken::new();
let child = root.child_token();
let grandchild = child.child_token();
let (mb, _rx) = Mailbox::new(root.clone());

mb.close();
assert!(child.is_cancelled());
assert!(grandchild.is_cancelled());
```

**Source:** `mailbox.rs:364-380`

---

## 13. Persist / Checkpoint System

### State File

- **Location**: `.codewhale/state/subagents.v1.json` (preferred) or `.deepseek/state/subagents.v1.json` (fallback)
- **Schema version**: `1`
- **On-disk format**: `PersistedSubAgentState { schema_version, agents: Vec<PersistedSubAgent>, workers: Vec<AgentWorkerRecord> }`

**Source:** `crates/tui/src/tools/subagent/mod.rs:1308-1324, 109-110`

### Persist Flow

```
Every step of every agent
    │
    ▼
update_checkpoint()  ───► persist_state_debounced()
    │                          │
    │                    ┌─────┴─────┐
    │                    │ due?      │
    │                    │ (1500ms)  │
    │                    ├───────────┤
    │                    │ YES: write │
    │                    │ full fleet │
    │                    │ to disk    │
    │                    └───────────┘
    │                    │ NO: set    │
    │                    │ persist_   │
    │                    │ pending    │
    │                    └───────────┘
    ▼
Terminal state change → persist_state_best_effort() (always writes)
```

### Debounce

**Source:** `crates/tui/src/tools/subagent/mod.rs:1902-1932`

- Hot-path writes are coalesced: at most one disk write per `SUBAGENT_PERSIST_DEBOUNCE` (1500ms).
- Skipped writes set `persist_pending = true`.
- Terminal writes and `flush_pending_persist()` always write.

### Atomic Write

**Source:** `crates/tui/src/tools/subagent/mod.rs:2958-2967`

```rust
fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, payload)?;
    fs::rename(tmp_path, path)?;
    Ok(())
}
```

Writes to a `.tmp` file then renames — crash-safe.

### Recovery on Restart

**Source:** `crates/tui/src/tools/subagent/mod.rs:1945-2030`

On manager construction, `load_state()` reads the persisted file:
1. All agents with status `Running` are reclassified as `Interrupted("Interrupted by process restart")`.
2. Agents from the prior session get `from_prior_session: true` (filtered from default listings).
3. Agents whose `session_boot_id` doesn't match the current manager's boot id are classified as "prior session."

### SubAgentCheckpoint

**Source:** `crates/tui/src/tools/subagent/mod.rs:1258-1270`

```rust
pub struct SubAgentCheckpoint {
    pub checkpoint_id: String,
    pub agent_id: String,
    pub continuation_handle: String,
    pub reason: String,
    pub continuable: bool,
    pub steps_taken: u32,
    pub message_count: usize,
    pub created_at_ms: u64,
    pub messages: Vec<Message>,
}
```

Interrupted agents with `continuable: true` can be resumed from their checkpoint messages.

---

## 14. TUI Integration

### Agent Cards

**Source:** `crates/tui/src/tui/widgets/agent_card.rs:1-870`

Two card types render live in the chat transcript:

#### DelegateCard (single agent)

```rust
pub struct DelegateCard {
    pub agent_id: String,
    pub agent_type: String,
    pub status: AgentLifecycle,     // Pending | Running | Completed | Failed | Cancelled | Interrupted
    pub summary: Option<String>,
    actions: Vec<String>,           // last 3 actions (DELEGATE_MAX_ACTIONS = 3)
    truncated: bool,                // true if older actions were dropped
}
```

Renders as:
```
⚙ Delegate  running  implementer · abc12345
  │ tool call: read_file src/main.rs
  │ tool call: edit_file src/main.rs
  │ tool call: exec_shell cargo build
  ╰ Summary: done, 3 steps, 1.2s
```

#### FanoutCard (multi-child dispatch)

```rust
pub struct FanoutCard {
    workers: Vec<WorkerSlot>,
}
```

Renders as a dot-grid: `●` filled (running/completed), `○` pending.

### AgentLifecycle Colors

| Status | Color |
|--------|-------|
| Pending | TEXT_MUTED |
| Running | STATUS_WARNING (amber) |
| Completed | STATUS_SUCCESS (green) |
| Failed | STATUS_ERROR (red) |
| Cancelled | TEXT_MUTED |
| Interrupted | STATUS_WARNING |

### Subagent Routing

**Source:** `crates/tui/src/tui/subagent_routing.rs:1-596`

The routing module manages:
- **`reconcile_subagent_activity_state`**: Syncs the TUI's agent progress state with the manager's canonical snapshot.
- **Terminal card retention**: Completed/failed/cancelled cards are retained for `SUBAGENT_TERMINAL_CARD_TTL` (5 minutes), up to `SUBAGENT_TERMINAL_CARD_MAX_RETAINED` (24).
- **Card reconciliation**: If a card missed its terminal mailbox envelope (e.g., API timeout), `reconcile_cards_with_snapshots` corrects it from the manager snapshot.

### Session Projection

When `agent` is called, the return value is a `SubAgentSessionProjection`:

```json
{
    "name": "sub-agent-a",
    "agent_id": "agent_abc123",
    "run_id": "agent_abc123",
    "status": "starting",
    "terminal": false,
    "context_mode": "fresh",
    "fork_context": false,
    "prefix_cache": { "mode": "fresh", ... },
    "transcript_handle": { ... },
    "follow_up": { "tool": "handle_read", ... },
    "takeover": { "kind": "local_subagent_session", ... },
    "artifacts": [ ... ],
    "usage": { "status": "unknown", ... },
    "verification": { "status": "self_report_only", ... },
    "snapshot": { ... },
    "worker_record": { ... }
}
```

**Source:** `crates/tui/src/tools/subagent/mod.rs:2809-2931`

---

## 15. System Prompts

### Per-Type Intros

Each agent type gets a role-specific intro prefixed to the output format contract:

```
[ROLE_INTRO]
[SUBAGENT_OUTPUT_FORMAT]
```

**Source:** `crates/tui/src/tools/subagent/mod.rs:459-472`

### Sub-Agent Context Line

Every sub-agent system prompt ends with:

```
You are a background sub-agent: every instruction comes from the orchestrating
agent, not a human. Never address the end user or ask them questions — do the
assigned work and report results back to the orchestrator.
```

**Source:** `crates/tui/src/tools/subagent/mod.rs:3284-3288`

### Output Format Contract

Every sub-agent's final message MUST end with a structured report:

```
### SUMMARY
### EVIDENCE
### CHANGES
### RISKS
### BLOCKERS
```

**Source:** `crates/tui/src/prompts/subagent_output_format.md`

### Agent System Prompt (`agent.txt`)

**Source:** `crates/tui/src/prompts/agent.txt`

The parent-side prompt that teaches the model how to use sub-agents:
- Write child prompts as compact Subagent Briefs (`QUESTION`, `SCOPE`, `ALREADY_KNOWN`, `EFFORT`, `STOP_CONDITION`, `OUTPUT`).
- Prefer parallel exploration with 2–4 `type: "explore"` sub-agents.
- Use `model_strength: "same"` for capability-critical work; `"faster"` for read-only lookup.
- Explore briefs default to `quick` (3–5 tool calls).
- Implementer children are not capped at 3–5 calls.
- Sub-agent outputs are **self-reports**, not verified facts — re-check before relying.

---

## 16. Complete Lifecycle Diagram

```
User turn: "agent(type="explore", prompt="...")"
    │
    ▼
AgentTool::execute()
    │
    ├── parse_spawn_request() → SpawnRequest
    ├── would_exceed_depth() check
    ├── rate_limit check
    ├── cwd validation
    ├── model resolution
    ├── resident_file lease check
    │
    ▼
SubAgentManager::spawn_background_with_assignment_options()
    │
    ├── generate UUID agent_id
    ├── assign whale nickname (deterministic hash)
    ├── create SubAgent instance
    ├── register AgentWorkerRecord
    ├── persist_state_debounced()
    │
    ▼
tokio::spawn(run_subagent_task)
    │
    ├── acquire launch_gate permit (direct children only)
    ├── run_subagent() loop:
    │     ├── build system prompt + messages
    │     ├── LLM API call (per-step timeout)
    │     ├── execute tool calls
    │     ├── emit progress events (mailbox + event_tx)
    │     ├── update_checkpoint()
    │     └── persist_state_debounced()
    │
    ▼
Terminal state (Completed / Failed / Cancelled / Interrupted)
    │
    ├── persist_state_best_effort()     (always writes)
    ├── release_resident_leases_for()
    ├── send SubAgentCompletion to parent_completion_tx
    │     │
    │     ▼
    │   Parent receives <codewhale:subagent.done>
    │     │
    │     ├── Read summary line
    │     ├── Integrate findings
    │     └── Update checklist
    │
    ├── drop launch_gate permit
    └── worker record updated with terminal status
```

---

## References

| File | Purpose |
|------|---------|
| `crates/tui/src/tools/subagent/mod.rs:1-5523` | Core sub-agent system (types, manager, spawn, run loop, persist) |
| `crates/tui/src/tools/subagent/mailbox.rs:1-491` | Mailbox abstraction for inter-agent communication |
| `crates/tui/src/tui/widgets/agent_card.rs:1-870` | DelegateCard and FanoutCard TUI widgets |
| `crates/tui/src/tui/subagent_routing.rs:1-596` | TUI routing: activity reconciliation, card sync |
| `crates/tui/src/prompts/constitution.md:1-557` | Runtime constitution with sub-agent rules (§Agent Usage, §Internal Sub-agent Completion Events) |
| `crates/tui/src/prompts/agent.txt` | Parent-side system prompt for agent usage |
| `crates/tui/src/prompts/subagent_output_format.md` | Output format contract for sub-agents |
| `crates/config/src/lib.rs:1338-1343` | `DEFAULT_SPAWN_DEPTH` and `MAX_SPAWN_DEPTH_CEILING` constants |
| `crates/tui/src/config.rs:22-23` | `MAX_SUBAGENTS` and `DEFAULT_MAX_SUBAGENTS` constants |
