# Whaleflow — Deep Dive

> **Source crate:** `crates/whaleflow/src/`
> **Module files:** `lib.rs` (3121 lines), `model_policy.rs` (496 lines), `replay.rs` (791 lines), `starlark_authoring.rs` (761 lines), `js_authoring.rs` (547 lines)

> ⚠️ **Status: Defined, not yet wired.** Whaleflow is fully implemented (IR, compiler, replay, authoring) but not yet integrated into the runtime. The TUI lists it under "Experimental" with the note: *"preview overlay for workflow/fleet runs (not stable; see #3154/#3178)"*. No other crate depends on `codewhale-whaleflow`. The definitions on this page describe the intended architecture; the actual execution path through `core` or `tui` does not call into it yet.

---

## 1. Overview

Whaleflow is the **typed workflow orchestration engine** for CodeWhale. It defines a Rust-owned intermediate representation (IR) for agent workflows — directed acyclic graphs of task nodes with branching, sequencing, reduction, conditional execution, expansion, and teacher-review semantics. Workflows are **authored** in Starlark or JavaScript/TypeScript, **compiled** into a `WorkflowSpec` IR, **validated** for structural integrity, and then **executed** (or **replayed** deterministically from a prior trace).

The crate deliberately stops at the Rust IR boundary. Runtime tool exposure, worktree application, live model execution, and replay are layered on top only after cancellation and evidence semantics are proven.

```
┌─────────────────────────────────────────────────────────────────┐
│                    AUTHORING LAYER                               │
│  ┌──────────────────────┐  ┌──────────────────────────────┐     │
│  │  Starlark (.star)    │  │  JavaScript / TypeScript     │     │
│  │  13 builtins + VM    │  │  JSON-object-literal subset  │     │
│  └────────┬─────────────┘  └──────────────┬───────────────┘     │
│           │                               │                      │
│           ▼                               ▼                      │
│  ┌─────────────────────────────────────────────────────────┐     │
│  │              WorkflowSpec IR  (lib.rs)                  │     │
│  │  8 node variants, BudgetSpec, PermissionSpec,           │     │
│  │  PromotionPolicy, ModelPolicy                           │     │
│  └──────────────────────┬──────────────────────────────────┘     │
│                         │                                        │
│           ┌─────────────┴─────────────┐                         │
│           ▼                           ▼                          │
│  ┌──────────────────┐   ┌──────────────────────────────┐        │
│  │  MockWorkflow    │   │  WorkflowReplayExecutor      │        │
│  │  Executor        │   │  (deterministic replay from  │        │
│  │  (test harness)  │   │   SHA-256-hashed traces)     │        │
│  └──────────────────┘   └──────────────────────────────┘        │
└─────────────────────────────────────────────────────────────────┘
```

---

## 2. Compilation Paths: `WorkflowConfig` vs `WorkflowSpec`

Whaleflow supports **two compilation paths** for different authoring styles.

### 2.1 `WorkflowConfig` → `WorkflowPlan` (Phase-based)

`lib.rs:30-49`

```rust
pub struct WorkflowConfig {
    pub goal: String,
    pub max_concurrent: u8,       // default: 4, range: 1–20
    pub description: Option<String>,
    pub phases: Vec<Phase>,
}
```

A `WorkflowConfig` is a high-level description organized into **phases** (sequential groups of tasks). Each `Phase` (`lib.rs:370-383`) has:
- `name`, `description`, `depends_on` (phase-level DAG edges)
- `parallel` flag — when true, all tasks in the phase run concurrently
- `on_failure`: `FailurePolicy` (`SkipContinue` | `Abort`)
- `tasks: Vec<Task>`

Each `Task` (`lib.rs:395-413`) has `id`, `prompt`, `agent_type` (`AgentType`), `mode` (`TaskMode`), `isolation` (`IsolationMode`), `file_scope`, `depends_on_results`, `max_steps`, and `timeout_secs`.

**Compilation** (`lib.rs:239-341`) via `WorkflowPlan::from_config(config)`:

1. Validates non-empty goal, phase names, and task IDs
2. Checks `max_concurrent` is in `1..=20`
3. Ensures no duplicate phases or tasks
4. Validates phase dependencies exist and have no cycles (DFS-based topological sort, `lib.rs:1632-1687`)
5. Validates task result dependencies reference tasks in earlier phases
6. For parallel phases with `ReadWrite` tasks, validates non-overlapping `file_scope`

The output `WorkflowPlan` (`lib.rs:232-358`) exposes `goal()`, `max_concurrent()`, `phases()`, and `phase_names()`.

```
WorkflowConfig ──▶ validate ──▶ from_config ──▶ WorkflowPlan (IR)
                      │                              │
                      ▼                              ▼
              WorkflowValidationError        phase_names(), phases()
```

### 2.2 `WorkflowSpec` (Node-based)

`lib.rs:51-68`

```rust
pub struct WorkflowSpec {
    pub id: Option<String>,
    pub goal: String,
    pub description: Option<String>,
    pub budget: BudgetSpec,
    pub permissions: PermissionSpec,
    pub model_policy: ModelPolicy,
    pub promotion_policy: PromotionPolicy,
    pub nodes: Vec<WorkflowNode>,
}
```

`WorkflowSpec` is the **canonical IR** — a flat list of `WorkflowNode` variants forming a DAG. This is the target of all authoring paths (Starlark, JS/TS) and the input to all executors. It carries top-level budget, permissions, model policy, and promotion policy that cascade to child nodes.

```
Starlark source ──▶ compile_starlark_workflow ──▶ WorkflowSpec
JS/TS source    ──▶ compile_javascript_workflow ──▶ WorkflowSpec
                         compile_typescript_workflow
```

Both paths ultimately produce a `WorkflowSpec` or `WorkflowPlan`. The type alias `WorkflowIr = WorkflowPlan` (`lib.rs:360`) ties them together.

---

## 3. WorkflowNode Variants (8 total)

`lib.rs:70-81`

```rust
#[serde(tag = "kind", content = "spec", rename_all = "snake_case")]
pub enum WorkflowNode {
    BranchSet(BranchSpec),
    Leaf(LeafSpec),
    Sequence(SequenceSpec),
    Reduce(ReduceSpec),
    TeacherReview(TeacherReviewSpec),
    LoopUntil(LoopUntilSpec),
    Cond(CondSpec),
    Expand(ExpandSpec),
}
```

All 8 variants use **externally-tagged** serde serialization (`"kind"`/`"spec"` fields) with `snake_case` naming.

### 3.1 `BranchSet(BranchSpec)`

`lib.rs:83-98`

A container that executes its children — either in parallel or sequentially — to explore alternative approaches.

| Field | Type | Semantics |
|---|---|---|
| `id` | `String` | Unique node identifier |
| `description` | `Option<String>` | Human-readable description |
| `parallel` | `bool` (default: `false`) | Run children concurrently |
| `budget` | `BudgetSpec` | Budget applied to the entire branch set |
| `permissions` | `PermissionSpec` | Permissions for all children |
| `model_policy` | `ModelPolicy` | Model selection policy for children |
| `children` | `Vec<WorkflowNode>` | Child nodes |

### 3.2 `Leaf(LeafSpec)`

`lib.rs:100-120`

The **terminal execution unit** — represents a single agent invocation (a "task").

| Field | Type | Semantics |
|---|---|---|
| `id` | `String` | Unique leaf identifier |
| `prompt` | `String` | The agent prompt |
| `agent_type` | `AgentType` (default: `General`) | Which agent role executes |
| `mode` | `TaskMode` (default: `ReadOnly`) | Read-only or read-write |
| `isolation` | `IsolationMode` (default: `Shared`) | Shared or worktree isolation |
| `file_scope` | `Vec<String>` | Files this leaf may access |
| `depends_on_results` | `Vec<String>` | IDs of upstream leaves whose outputs are needed |
| `budget` | `BudgetSpec` | Leaf-level budget |
| `permissions` | `PermissionSpec` | Leaf-level permissions |
| `model_policy` | `ModelPolicy` | Leaf-level model selection |

**Supporting enums** (`lib.rs:418-444`):

- `AgentType`: `General`, `Explore`, `Plan`, `Review`, `Implementer`, `Verifier`
- `TaskMode`: `ReadOnly`, `ReadWrite`
- `IsolationMode`: `Shared`, `Worktree`

### 3.3 `Sequence(SequenceSpec)`

`lib.rs:122-127`

Executes children in **declaration order**, one after another.

| Field | Type | Semantics |
|---|---|---|
| `id` | `String` | Unique identifier |
| `children` | `Vec<WorkflowNode>` | Nodes to execute sequentially |

### 3.4 `Reduce(ReduceSpec)`

`lib.rs:129-137`

**Aggregates** outputs from upstream nodes using a model-driven reduction prompt.

| Field | Type | Semantics |
|---|---|---|
| `id` | `String` | Unique identifier |
| `inputs` | `Vec<String>` | References to upstream node IDs |
| `prompt` | `String` | Reduction/merge prompt |
| `model_policy` | `ModelPolicy` | Model selection for the reducer |

### 3.5 `TeacherReview(TeacherReviewSpec)`

`lib.rs:139-146`

A **promotion gate** — the "teacher" reviews candidate outputs from multiple branches and selects the best.

| Field | Type | Semantics |
|---|---|---|
| `id` | `String` | Unique identifier |
| `candidates` | `Vec<String>` | References to candidate-producing nodes |
| `promotion_policy` | `PromotionPolicy` | How candidates are evaluated/selected |

### 3.6 `LoopUntil(LoopUntilSpec)`

`lib.rs:148-156`

Repeats child execution until a condition is met (or max iterations).

| Field | Type | Semantics |
|---|---|---|
| `id` | `String` | Unique identifier |
| `condition` | `String` | Predicate describing the loop exit condition |
| `max_iterations` | `Option<u32>` | Safety cap on iterations |
| `children` | `Vec<WorkflowNode>` | Body nodes |

### 3.7 `Cond(CondSpec)`

`lib.rs:158-166`

Conditional branching: `then` vs `else`.

| Field | Type | Semantics |
|---|---|---|
| `id` | `String` | Unique identifier |
| `condition` | `String` | Predicate to evaluate |
| `then_nodes` | `Vec<WorkflowNode>` | Executed when condition is true |
| `else_nodes` | `Vec<WorkflowNode>` | Executed when condition is false |

### 3.8 `Expand(ExpandSpec)`

`lib.rs:168-176`

**Dynamic node generation** — expands a source into a set of child nodes at execution time.

| Field | Type | Semantics |
|---|---|---|
| `id` | `String` | Unique identifier |
| `source` | `String` | Reference to the expansion source |
| `max_children` | `Option<usize>` | Cap on generated children |
| `template` | `Option<Box<WorkflowNode>>` | Optional template for generated nodes |

---

## 4. BudgetSpec and PermissionSpec

### 4.1 `BudgetSpec`

`lib.rs:178-186`

```rust
pub struct BudgetSpec {
    pub max_steps: Option<u32>,      // max agent steps
    pub timeout_secs: Option<u64>,   // wall-clock timeout
    pub max_parallel: Option<u8>,    // max parallel tasks
}
```

All fields default to `None` (unlimited). Budgets **cascade** — a `BranchSet`'s budget constrains all its children. If a budget is exhausted, the node status becomes `WorkflowRunStatus::BudgetExceeded`.

### 4.2 `PermissionSpec`

`lib.rs:188-198`

```rust
pub struct PermissionSpec {
    pub allow_write: bool,           // default: false
    pub allow_network: bool,         // default: false
    pub allowed_tools: Vec<String>,  // whitelist of tool names
    pub file_scope: Vec<String>,     // path scope restrictions
}
```

Permissions are **deny-by-default**: write and network access are off unless explicitly enabled. The `allowed_tools` whitelist restricts which tools an agent can invoke. `file_scope` restricts paths the agent may access.

---

## 5. PromotionPolicy and PromotionStrategy

`lib.rs:210-230`

### 5.1 `PromotionPolicy`

```rust
pub struct PromotionPolicy {
    pub strategy: PromotionStrategy,                // default: All
    pub require_teacher_review: bool,               // default: false
    pub min_successful_branches: Option<u32>,        // minimum viable branches
    pub promotion_gate: PromotionGate,               // quality bar
}
```

### 5.2 `PromotionStrategy` (enum)

```rust
pub enum PromotionStrategy {
    All,              // promote all candidates
    FirstSuccess,     // promote the first successful one
    BestScore,        // promote the highest-scoring candidate
    TeacherSelected,  // let the Teacher model choose
}
```

### 5.3 `PromotionGate`

`lib.rs:1079-1168`

```rust
pub struct PromotionGate {
    pub min_score_delta: i32,               // default: 1
    pub max_cost_delta_microusd: Option<i64>, // cost budget limit
    pub require_all_tests_pass: bool,        // default: true
    pub reject_policy_violations: bool,      // default: true
    pub reject_stale_replay: bool,           // default: true
}
```

The `PromotionGate::evaluate_candidate(candidate)` method (`lib.rs:1106-1168`) checks:
1. Score delta meets `min_score_delta`
2. Cost delta does not exceed `max_cost_delta_microusd`
3. All required tests pass (if `require_all_tests_pass`)
4. No policy violations (if `reject_policy_violations`)
5. Replay is not stale (if `reject_stale_replay`)

A candidate is `Promoted` only when all checks pass; otherwise `Rejected`. The result is a `PromotionGateDecision` (`lib.rs:1170-1184`).

**Policy cascading:**
```
PromotionPolicy.strategy = TeacherSelected
       │
       ▼
  TeacherReview node evaluates candidates
       │
       ▼
  PromotionGate.evaluate_candidate() per candidate
       │
       ▼
  PromotionGateDecision { Promoted | Rejected }
```

---

## 6. ModelPolicy System

`model_policy.rs:1-496`

### 6.1 `ModelRole` (8 variants)

`model_policy.rs:9-20`

| Variant | Maps from AgentType | Purpose |
|---|---|---|
| `Planner` | `AgentType::Plan` | High-level planning |
| `LeafReasoner` | `AgentType::General`, `AgentType::Explore` | General reasoning |
| `Implementer` | `AgentType::Implementer` | Code generation |
| `Reviewer` | `AgentType::Review`, `AgentType::Verifier` | Review/verification |
| `Teacher` | *(explicit config only)* | Teacher model for promotion |
| `Student` | *(explicit config only)* | Student model in promotion flows |
| `JsonExtractor` | *(explicit config only)* | Structured JSON extraction |
| `StarlarkRepair` | *(explicit config only)* | Starlark repair/recovery |

The mapping from `AgentType` to `ModelRole` is defined at `model_policy.rs:22-31`: General/Explore → LeafReasoner, Plan → Planner, Review/Verifier → Reviewer, Implementer → Implementer. The remaining 4 roles are configured explicitly.

### 6.2 `ModelCapabilities`

`model_policy.rs:33-45`

```rust
pub struct ModelCapabilities {
    pub tool_calls: bool,     // function/tool calling
    pub json_mode: bool,      // structured JSON output
    pub prompt_cache: bool,   // prompt caching for long contexts
    pub large_context: bool,  // large context windows
    pub streaming: bool,      // streaming responses
}
```

**Capability matching** (`model_policy.rs:47-56`): `satisfies(required)` returns `true` only when every `required` capability that is `true` is also `true` on `self`. Required capabilities that are `false` are ignored — this is a positive-only superset check.

```
self.capabilities.satisfies(required) ⇔
    (!required.tool_calls || self.tool_calls) &&
    (!required.json_mode || self.json_mode) &&
    (!required.prompt_cache || self.prompt_cache) &&
    (!required.large_context || self.large_context) &&
    (!required.streaming || self.streaming)
```

### 6.3 `ProviderRegistry`

`model_policy.rs:83-168`

```rust
pub struct ProviderRegistry {
    models: BTreeMap<String, ProviderModel>,           // key: "provider/model"
    role_policies: BTreeMap<ModelRole, ModelPolicy>,   // per-role defaults
}
```

**Registration:**
- `with_model(model)` / `insert_model(model)` — adds a `ProviderModel` (provider, model name, capabilities)
- `with_role_policy(role, policy)` — sets the default `ModelPolicy` for a role

**Resolution** (`resolve_role(role, policy, required_capabilities)`, `model_policy.rs:109-125`):

```
1. Determine policy:
   - If caller provides an explicit ModelPolicy → use it (source: Primary)
   - Else look up role_policies[role] → use role default (source: RoleDefault)
   - If neither exists → MissingPolicy error

2. Build candidate list from policy:
   - Primary model (from policy.model) — must exist
   - Fallback models (from policy.fallback_models) — in declaration order
   - Each model string: "provider/model" or "model" (uses policy.provider)

3. For each candidate in order:
   - Look up in registry.models by "provider/model" key
   - If not found → record "unknown" rejection, continue
   - If capabilities.satisfies(required) → return ResolvedModel with source
   - Else → record "missing capabilities" rejection, continue

4. If no candidate matches → NoCapableModel error with rejection list
```

**Fallback chain behavior:** Fallbacks are tried in declaration order. The first model that satisfies all required capabilities wins. This enables patterns like:

```
ModelPolicy {
    provider: "mock",
    model: "plain",           // tried first — no json_mode
    fallback_models: ["json"] // tried second — has json_mode ✓
}
```

### 6.4 Supporting Types

**`ProviderModel`** (`model_policy.rs:58-64`): A registered model with `provider`, `model`, and `capabilities`.

**`ResolvedModel`** (`model_policy.rs:66-73`): Resolution result with `role`, `provider`, `model`, `capabilities`, and `source` (Primary / Fallback / RoleDefault).

**`ModelSelectionSource`** (`model_policy.rs:75-81`): `Primary`, `Fallback`, `RoleDefault` — tracks how a model was selected.

**`CompletionRequest`** (`model_policy.rs:170-178`): `role`, `prompt`, `require_json`, `model_policy`.

**`CompletionResponse`** (`model_policy.rs:180-185`): `text`, `usage` (WorkflowUsage).

**`ModelProvider` trait** (`model_policy.rs:187-195`): `provider()`, `model()`, `capabilities()`, `complete(request)`.

**`MockModelProvider`** (`model_policy.rs:197-243`): Test-only implementation returning pre-configured responses.

**Error types:**
- `ModelPolicyError` (`model_policy.rs:245-258`): `MissingPolicy`, `MissingModel`, `MissingFallbackProvider`, `NoCapableModel`
- `ModelProviderError` (`model_policy.rs:260-268`): `Failed { provider, model, reason }`
- `JsonRepairError` (`model_policy.rs:270-274`): `Parse { reason }`

**JSON repair** (`model_policy.rs:276-299`): `parse_json_with_repair(raw)` tries direct deserialization, then on failure strips markdown fences and extracts the first `{...}` or `[...]` payload via `repair_json_text_once()`.

### 6.5 `ModelPolicy` struct

Defined in `lib.rs:200-208`, used throughout:

```rust
pub struct ModelPolicy {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub fallback_models: Vec<String>,
}
```

---

## 7. Deterministic Replay

`replay.rs:1-791`

### 7.1 Architecture

The replay system enables **deterministic re-execution** of a workflow from a previously recorded trace — without making live model calls. Every leaf invocation is replaced by a recorded result keyed by a SHA-256 hash of its inputs.

```
┌──────────────────┐     ┌──────────────────────────┐
│  First execution  │────▶│  WorkflowReplayTrace      │
│  (live models)    │     │  - leaf_records[]         │
│                   │     │  - control_records[]      │
└──────────────────┘     └───────────┬──────────────┘
                                     │
                                     ▼
                            ┌──────────────────────────┐
                            │  WorkflowReplayExecutor   │
                            │  - replays leaf results   │
                            │  - replays control nodes  │
                            │  - detects divergence     │
                            └──────────────────────────┘
```

### 7.2 SHA-256 Input Hashing

`replay.rs:423-439`

```rust
pub fn compute_leaf_input_hash(
    spec: &WorkflowSpec,
    leaf: &LeafSpec,
    resolved_inputs: &BTreeMap<String, Option<String>>,
) -> Result<String, WorkflowReplayError>
```

The hash input (`ReplayLeafInput`, `replay.rs:441-447`) serializes to JSON:
- `workflow_id` (optional)
- `workflow_goal`
- The entire `leaf` spec (id, prompt, agent_type, mode, isolation, file_scope, depends_on_results, budget, permissions, model_policy)
- `resolved_inputs` — a `BTreeMap<String, Option<String>>` of upstream outputs

This is hashed with **SHA-256** (`sha2::Sha256`) and formatted as a hex string. The hash captures *everything* that could affect the leaf's behavior — if any parameter changes, the hash changes and the replay will diverge.

**Stability guarantee** (`replay.rs:706-721`): Because `resolved_inputs` uses `BTreeMap`, hash output is stable regardless of insertion order.

### 7.3 `ReplayLeafRecord`

`replay.rs:29-35`

```rust
pub struct ReplayLeafRecord {
    pub trace_id: String,       // which trace this belongs to
    pub leaf_id: String,        // which leaf node
    pub input_hash: String,     // SHA-256 of (workflow, leaf, resolved_inputs)
    pub result: LeafResult,     // the recorded output
}
```

### 7.4 `ReplayControlRecord`

`replay.rs:37-45`

```rust
pub struct ReplayControlRecord {
    pub trace_id: String,
    pub node_id: String,
    pub kind: ControlNodeKind,              // BranchSet, Cond, Expand, etc.
    pub result: ControlNodeResult,          // recorded control outcome
    pub generated_nodes: Vec<WorkflowNode>, // for Expand nodes
}
```

### 7.5 `WorkflowReplayTrace`

`replay.rs:20-27`

```rust
pub struct WorkflowReplayTrace {
    pub trace_id: String,
    pub leaf_records: Vec<ReplayLeafRecord>,
    pub control_records: Vec<ReplayControlRecord>,
}
```

### 7.6 `WorkflowReplayExecutor`

`replay.rs:47-397`

```rust
pub struct WorkflowReplayExecutor {
    trace_id: String,
    options: ReplayOptions,
    leaf_records: BTreeMap<ReplayLeafKey, LeafResult>,
    control_records: BTreeMap<ReplayControlKey, ReplayControlRecord>,
    resolved_outputs: BTreeMap<String, Option<String>>,
}
```

**Construction:**
- `new(trace)` — builds internal lookup maps from the trace
- `with_options(trace, options)` — with `ReplayOptions { allow_live_replay }`

**Execution flow** (`run(spec)`, `replay.rs:101-106`):
1. Validates workflow nodes
2. Iterates through nodes, dispatching to type-specific handlers
3. For each leaf: computes `input_hash`, looks up matching record, replays or diverges
4. For each control node: looks up recorded control result, replays or diverges

**Divergence detection:** When a leaf has no matching record, the executor either:
- Returns `LiveReplayUnavailable` error (if `allow_live_replay` is set)
- Marks divergence (`ReplayDiverged` status) for the leaf and continues

**Control node replay** (`replay.rs:369-396`): `push_control_or_diverge` uses recorded control results for `Cond` branch selection, `Expand` generated nodes, and `LoopUntil` iteration count.

### 7.7 `ReplayOptions`

`replay.rs:14-18`

```rust
pub struct ReplayOptions {
    pub allow_live_replay: bool,  // default: false (always safe)
}
```

### 7.8 Error Types

`replay.rs:413-421`

```rust
pub enum WorkflowReplayError {
    Validation(WorkflowExecutionError),     // structural validation
    LiveReplayUnavailable { leaf: String }, // live replay not configured
    InputHash { reason: String },          // hash computation failed
}
```

---

## 8. Authoring

### 8.1 JavaScript / TypeScript Authoring

`js_authoring.rs:1-547`

JS/TS workflows are authored as **JSON object literals** passed to a `workflow({...})` call. The system does **not execute** JavaScript — it extracts and parses the object literal.

#### Compilation flow

```
source ──▶ reject_unsupported_constructs ──▶ extract_workflow_object ──▶
              (banned token scan)            (brace matching)
                                                      │
                                                      ▼
                                          serde_json::from_str<JsWorkflowSpec>
                                                      │
                                                      ▼
                                          JsWorkflowSpec::into_workflow()
                                                      │
                                                      ▼
                                          validate_workflow_nodes ──▶ WorkflowSpec
```

#### Banned constructs (`js_authoring.rs:59-86`)

The function `reject_unsupported_constructs` scans source for these **19 banned tokens** before any parsing:

| Token | Rationale |
|---|---|
| `import ` | Static import — would execute arbitrary code |
| `import(` | Dynamic import — runtime module loading |
| `require(` | CommonJS require — file system access |
| `fetch(` | Network access |
| `XMLHttpRequest` | Network access |
| `WebSocket` | Persistent network connection |
| `process.` | Node.js process access |
| `Deno.` | Deno runtime access |
| `Bun.` | Bun runtime access |
| `child_process` | Process spawning |
| `exec(` | Command execution |
| `spawn(` | Process spawning |
| `open(` | File/network open |
| `readFile` | File system read |
| `writeFile` | File system write |
| `async ` | Asynchronous execution (nondeterministic) |
| `await ` | Await (nondeterministic) |
| `eval(` | Dynamic code evaluation |
| `new Function` | Dynamic function constructor |

All are rejected with `JavascriptWorkflowError::UnsupportedConstruct { construct }`.

#### Brace matching (`js_authoring.rs:88-148`)

`extract_workflow_object(source)` finds the `workflow(` call, then uses a **character-level brace matcher** that respects strings and escape sequences to extract the outermost `{...}` object:

```
1. Find "workflow" in source
2. Find the '(' after "workflow"
3. Skip whitespace after '(' to find '{'
4. Walk characters tracking:
   - String quoting (", ', `) with escape handling
   - Brace depth counter
5. When depth returns to 0, return the span
```

This means TypeScript **type annotations** after the object literal (e.g., `satisfies WorkflowSpec`) are safely ignored — they appear after the closing `}`.

#### JS Workflow Node mapping (`js_authoring.rs:189-400`)

`JsWorkflowNode` is an `#[serde(untagged)]` enum that deserializes either:
- `Raw(WorkflowNode)` — a fully-formed node with `kind`/`spec` fields
- Or one of 8 typed variants: `Agent`, `Branch`, `Sequence`, `Reduce`, `TeacherReview`, `LoopUntil`, `Cond`, `Expand`

Each variant wraps a JS-specific spec struct that maps to the core `WorkflowNode` via `into_node()`:

| JS key | Wraps | Target WorkflowNode |
|---|---|---|
| `"agent": {...}` | `JsAgentNode` → `LeafSpec` | `Leaf` |
| `"branch": {...}` | `JsBranchNode` → `JsBranchSpec` → `BranchSpec` | `BranchSet` |
| `"sequence": {...}` | `JsSequenceNode` → `JsSequenceSpec` → `SequenceSpec` | `Sequence` |
| `"reduce": {...}` | `JsReduceNode` → `ReduceSpec` | `Reduce` |
| `"teacher_review": {...}` | `JsTeacherReviewNode` → `TeacherReviewSpec` | `TeacherReview` |
| `"loop_until": {...}` | `JsLoopUntilNode` → `JsLoopUntilSpec` → `LoopUntilSpec` | `LoopUntil` |
| `"cond": {...}` | `JsCondNode` → `JsCondSpec` → `CondSpec` | `Cond` |
| `"expand": {...}` | `JsExpandNode` → `JsExpandSpec` → `ExpandSpec` | `Expand` |

#### JS Example

```javascript
export default workflow({
  "id": "js-audit",
  "goal": "Audit a change with parallel agents",
  "nodes": [
    {
      "branch": {
        "id": "parallel-audit",
        "parallel": true,
        "children": [
          { "agent": { "id": "docs-audit", "prompt": "Inspect docs", "agent_type": "review" } },
          { "agent": { "id": "tests-audit", "prompt": "Inspect tests", "agent_type": "verifier" } }
        ]
      }
    },
    {
      "reduce": {
        "id": "synthesize",
        "inputs": ["docs-audit", "tests-audit"],
        "prompt": "Merge the branch findings"
      }
    }
  ]
});
```

TypeScript works identically — the type suffix is ignored by the brace matcher:
```typescript
export default workflow({ ... } satisfies WorkflowSpec);
```

---

### 8.2 Starlark Authoring

`starlark_authoring.rs:1-761`

Starlark (a deterministic, hermetic Python dialect by Google) is used as the **primary authoring language** for Whaleflow workflows. Workflows are authored as Starlark scripts using 13 built-in functions.

#### 8.2.1 The 13 Builtins

Defined via `#[starlark_module]` at `starlark_authoring.rs:221-413`:

| # | Builtin | Signature | Purpose |
|---|---|---|---|
| 1 | `workflow` | `(goal, nodes, id?, description?)` | **Entry point** — defines the top-level `WorkflowSpec`. Must be called exactly once. |
| 2 | `agent` | `(id, prompt, agent_type?, mode?, isolation?, file_scope?, depends_on_results?)` | Creates a `Leaf` node. Agent types: `"general"`, `"explore"`/`"explorer"`, `"plan"`, `"review"`, `"implementer"`/`"implement"`, `"verifier"`/`"verify"`. Mode: `"read_only"` (default) or `"read_write"`. Isolation: `"shared"` (default) or `"worktree"`. |
| 3 | `test` | `(id, command, file_scope?)` | Shorthand for `agent(agent_type="verifier", mode="read_only", prompt="Run test command: {command}")` |
| 4 | `search` | `(id, query, file_scope?)` | Shorthand for `agent(agent_type="explore", prompt="Search codebase: {query}")` |
| 5 | `shell` | `(id, command, file_scope?)` | Shorthand for `agent(agent_type="verifier", prompt="Run shell command: {command}")` |
| 6 | `branch` | `(id, children, parallel?)` | Creates a `BranchSet` node. `parallel` defaults to `true`. |
| 7 | `sequence` | `(id, children)` | Creates a `Sequence` node. |
| 8 | `reduce` | `(id, prompt, inputs?)` | Creates a `Reduce` node. |
| 9 | `teacher_review` | `(id, candidates?)` | Creates a `TeacherReview` node. |
| 10 | `tournament` | `(id, candidates?)` | **Alias** for `teacher_review` — semantically identical, creates a `TeacherReview` node. |
| 11 | `loop_until` | `(id, condition, children, max_iterations?)` | Creates a `LoopUntil` node. |
| 12 | `when` | `(id, condition, then_nodes, else_nodes?)` | Creates a `Cond` node. Uses the Starlark raw identifier `r#when` because `when` is not a keyword in Starlark but `r#when` avoids collision. |
| 13 | `expand` | `(id, source, max_children?)` | Creates an `Expand` node. |

**Key design pattern:** Nodes are serialized to JSON strings (`encode_node` → `serde_json::to_string`) and passed between builtins as opaque string tokens. They are deserialized back (`decode_node`) when consumed by parent builtins. This enables Starlark's type system (which lacks Rust-level enum variants) to represent the full `WorkflowNode` enum via JSON round-tripping.

#### 8.2.2 VM Execution Model

`starlark_authoring.rs:35-60`

```
1. reject_unsupported_constructs(source)   — scan for banned tokens
2. AstModule::parse(identifier, source, dialect)  — parse Starlark AST
       dialect.enable_f_strings = true
3. Create WorkflowBuilder (RefCell<Option<WorkflowSpec>>)
4. Evaluator::new(&module) with eval.extra = &builder
5. eval.eval_module(ast, &globals)          — execute Starlark
       globals = standard + workflow_builtins
6. Extract builder.workflow (error if missing)
7. validate_workflow_nodes(&workflow.nodes)
8. Return WorkflowSpec
```

**Sandboxing:** The Starlark VM runs with standard globals only — no `load()`, `import`, `open()`, `while`, `async`, `await`, or `class` constructs are permitted (`reject_unsupported_constructs`, `starlark_authoring.rs:90-105`). The 7 banned constructs mirror the JS safety model.

#### 8.2.3 Repair Mechanism

`starlark_authoring.rs:62-88`

Two repair functions handle LLM-generated Starlark that uses convenience aliases:

**`compile_starlark_workflow_with_repair(identifier, source)`** (`starlark_authoring.rs:62-77`):
1. Try `compile_starlark_workflow` directly
2. On failure, call `repair_starlark_workflow_once(source)`
3. If the repaired source differs from original, retry compilation
4. Return the result

**`repair_starlark_workflow_once(source)`** (`starlark_authoring.rs:79-88`): Performs 7 string replacements:

| LLM-generated pattern | Replaced with |
|---|---|
| `ctx.parallel(...)` | `branch(...)` |
| `ctx.sequence(...)` | `sequence(...)` |
| `ctx.loop_until(...)` | `loop_until(...)` |
| `ctx.when(...)` | `when(...)` |
| `ctx.expand(...)` | `expand(...)` |
| `ctx.tournament(...)` | `tournament(...)` |
| `ctx.teacher.review(...)` | `teacher_review(...)` |

These repairs handle the common LLM mistake of prefixing builtins with `ctx.` or using method-call style (`ctx.teacher.review(...)`).

#### Starlark Example

```python
workflow(
    id = "rlm-cache-change",
    goal = "Implement a cache policy change",
    nodes = [
        branch(
            id = "candidate-branches",
            parallel = True,
            children = [
                agent(id = "analyze", prompt = "Analyze the cache change impact", agent_type = "explore"),
                agent(id = "implement", prompt = "Implement the cache change", agent_type = "implementer"),
            ],
        ),
        loop_until(
            id = "implement-until-tests-pass",
            condition = "all tests pass",
            max_iterations = 3,
            children = [
                test(id = "regression-tests", command = "cargo test -p codewhale-whaleflow --locked"),
            ],
        ),
        teacher_review(id = "teacher-review", candidates = ["candidate-branches"]),
        reduce(
            id = "summarize-cache-change",
            prompt = "Summarize the cache change and its impact",
            inputs = ["analyze", "implement"],
        ),
    ],
)
```

---

## 9. BranchTournament and ParetoFrontier

`lib.rs:1375-1427`

### 9.1 `BranchTournament`

`lib.rs:1375-1392`

```rust
pub struct BranchTournament {
    pub min_score: u32,   // minimum score threshold
}
```

**Tournament selection** (`select(candidates)`, `lib.rs:1382-1392`):

```
1. Filter: only Succeeded candidates with score >= min_score
2. Sort by: (cost ascending, then score descending) via min_by_key
3. Return: the single best candidate (lowest cost among highest-scoring)
```

The tournament is a **lexicographic minimizer**: cost is the primary objective, score breaks ties. Only one winner emerges.

```
Tournament selection:
  candidates ──▶ filter(succeeded ∧ score ≥ min_score)
                     │
                     ▼
              min_by_key(cost, Reverse(score))
                     │
                     ▼
              Option<BranchCandidate>   (the winner)
```

### 9.2 `ParetoFrontier`

`lib.rs:1394-1427`

```rust
pub struct ParetoFrontier {
    pub max_items: usize,   // default: 8
}
```

**Pareto frontier selection** (`select(candidates)`, `lib.rs:1408-1427`):

```
1. Filter: only Succeeded candidates
2. Keep non-dominated candidates:
   A candidate is dominated if there exists another candidate where:
     other.score >= candidate.score  AND
     other.cost  <= candidate.cost   AND
     (other.score > candidate.score OR other.cost < candidate.cost)
3. Sort by: (score descending, cost ascending)
4. Truncate to max_items (minimum 1)
5. Return Vec<BranchCandidate>
```

Unlike the tournament (which picks one winner), the Pareto frontier returns a **set** of non-dominated candidates — those for which no other candidate is strictly better on both dimensions.

```
Pareto frontier:
  candidates ──▶ filter(succeeded)
                     │
                     ▼
              remove dominated (Pareto filter)
                     │
                     ▼
              sort by (score↓, cost↑)
                     │
                     ▼
              truncate(max_items=8)
                     │
                     ▼
              Vec<BranchCandidate>
```

### 9.3 `BranchCandidate`

`lib.rs:1001-1009`

```rust
pub struct BranchCandidate {
    pub branch_id: String,
    pub status: WorkflowRunStatus,
    pub score: u32,
    pub cost: u64,
    pub diversity_key: Option<String>,  // for diversity-preserving selection
}
```

`BranchCandidate` is the common currency for both selection algorithms. The `diversity_key` field enables future diversity-aware selection (e.g., ensuring selected branches come from different strategies).

---

## 10. TeacherReview and TeacherCandidate

`lib.rs:1001-1344`

### 10.1 `TeacherCandidateKind` (7 types)

`lib.rs:1011-1021`

```rust
pub enum TeacherCandidateKind {
    Note,                          // informational note from a leaf
    WorkflowRecipe,                // successful branch → reusable recipe
    SkillPatch,                    // (reserved for skill system)
    RegressionTest,                // failed leaf → regression test
    CachePolicyPatch,              // cache-hit result → policy patch
    BranchHeuristic,               // heuristic from branch result
    StarlarkAuthoringPromptPatch,  // from a control node
}
```

**Candidate kind derivation** (`lib.rs:1246-1344`):

| Source | Condition | Kind |
|---|---|---|
| **BranchResult** | Cache hits present | `CachePolicyPatch` |
| **BranchResult** | Succeeded, no cache hits | `WorkflowRecipe` |
| **BranchResult** | Otherwise | `BranchHeuristic` |
| **LeafResult** | Failed | `RegressionTest` |
| **LeafResult** | Cache hits present | `CachePolicyPatch` |
| **LeafResult** | Otherwise | `Note` |
| **ControlNodeResult** | Any | `StarlarkAuthoringPromptPatch` |

### 10.2 `TeacherCandidate`

`lib.rs:1033-1047`

```rust
pub struct TeacherCandidate {
    pub candidate_id: String,
    pub kind: TeacherCandidateKind,
    pub status: TeacherCandidateStatus,         // Proposed | Accepted | Rejected | Promoted
    pub source_node_id: String,
    pub source_branch_id: Option<String>,
    pub summary: String,
    pub evidence: Vec<String>,
    pub replay_results: Vec<StudentReplayResult>,
}
```

### 10.3 `TeacherCandidateStatus`

`lib.rs:1023-1031`

```rust
pub enum TeacherCandidateStatus {
    Proposed,    // newly created
    Accepted,    // passed review
    Rejected,    // failed review
    Promoted,    // passed promotion gate
}
```

### 10.4 `TeacherReviewReport`

`lib.rs:1196-1211`

```rust
pub struct TeacherReviewReport {
    pub review_node_id: String,
    pub candidates: Vec<TeacherCandidate>,
}
```

Constructed via `TeacherReviewReport::from_execution(review, execution)` (`lib.rs:1203-1211`), which calls `teacher_candidates_from_execution()` to convert all referenced branch, leaf, and control results into `TeacherCandidate` entries.

### 10.5 Student Replay

`lib.rs:1049-1077`

```rust
pub struct StudentReplayResult {
    pub trace_id: String,
    pub candidate_id: String,
    pub baseline: StudentReplayMetrics,           // before the candidate
    pub candidate: StudentReplayMetrics,          // after the candidate
    pub required_tests: Vec<StudentReplayTestResult>,
    pub policy_violations: Vec<String>,
    pub stale: bool,
    pub notes: Option<String>,
}

pub struct StudentReplayMetrics {
    pub score: i32,
    pub cost_microusd: u64,
}

pub struct StudentReplayTestResult {
    pub name: String,
    pub passed: bool,
}
```

`StudentReplayResult::score_delta()` (`lib.rs:1186-1189`) computes `candidate.score - baseline.score`.
`StudentReplayResult::cost_delta_microusd()` (`lib.rs:1191-1193`) computes the signed difference.

---

## Appendix: Key Supporting Types

### `WorkflowRunStatus` (`lib.rs:540-551`)

```rust
pub enum WorkflowRunStatus {
    Pending, Running, Succeeded, Failed, Cancelled, BudgetExceeded, ReplayDiverged,
}
```

### `ControlNodeKind` (`lib.rs:553-564`)

```rust
pub enum ControlNodeKind {
    BranchSet, Leaf, Sequence, Reduce, TeacherReview, LoopUntil, Cond, Expand,
}
```
Mirrors `WorkflowNode` variants for result tracking.

### `WorkflowExecution` (`lib.rs:566-617`)

```rust
pub struct WorkflowExecution {
    pub status: WorkflowRunStatus,
    pub usage: WorkflowUsage,
    pub memo_usage: WorkflowMemoUsage,
    pub leaf_results: Vec<LeafResult>,
    pub branch_results: Vec<BranchResult>,
    pub control_node_results: Vec<ControlNodeResult>,
}
```

### `WorkflowUsage` & `WorkflowMemoUsage` (`lib.rs:476-527`)

Track token usage, cost, and memoization (ARMH/prompt-cache) statistics.

### `MockWorkflowExecutor` (`lib.rs:664-968`)

A deterministic test executor that uses pre-configured leaf outcomes and predicate results — no live models needed. Supports all 8 node types, budget enforcement, and cancellation.

### Error Types

- `WorkflowValidationError` (`lib.rs:1586-1619`): 9 variants covering empty fields, duplicates, cycles, invalid dependencies, and scope overlaps
- `WorkflowExecutionError` (`lib.rs:1429-1443`): 4 variants for empty IDs, empty prompts, duplicate IDs, and unknown references
- `JavascriptWorkflowError` (`js_authoring.rs:12-24`): Unsupported constructs, missing workflow call, invalid objects, JSON errors, node errors
- `StarlarkWorkflowError` (`starlark_authoring.rs:21-33`): Unsupported constructs, missing workflow, invalid nodes, invalid enums, Starlark errors
