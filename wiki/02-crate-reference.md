# CodeWhale — Crate Reference

Complete reference for all 15 workspace crates in `crates/`. Each section covers
the crate's purpose, key types, workspace dependencies, and approximate line
count.

---

## Dependency Graph

```
                    ┌──────────────────────────────────────┐
                    │               tui (15)               │
                    │   ~9,000 lines · terminal UI          │
                    └──┬───┬───────┬────┬────────┬─────────┘
                       │   │       │    │        │
         ┌─────────────┘   │       │    │        └──────────────────┐
         ▼                 ▼       ▼    ▼                           ▼
   ┌──────────┐   ┌──────────┐ ┌──────────┐          ┌──────────────────┐
   │ release  │   │ secrets  │ │  tools   │          │ app-server (14)  │
   │   (10)   │   │   (2)    │ │   (6)    │          │ ~1,500 lines     │
   └──────────┘   └─────┬────┘ └────┬─────┘          └────────┬─────────┘
                        │           │                         │
         ┌──────────────┼───────────┼─────────────────────────┤
         │              │           │                         │
         ▼              ▼           ▼                         ▼
   ┌──────────────────────────────────────────────────────────────────┐
   │                         cli (13)                                  │
   │   ~4,300 lines · headless CLI + legacy shim                       │
   └──────┬────────────┬────────────┬───────────┬─────────────────────┘
          │            │            │           │
          ▼            ▼            ▼           ▼
   ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐
   │  agent   │ │  state   │ │   mcp    │ │  hooks   │
   │   (8)    │ │   (4)    │ │   (7)    │ │   (5)    │
   └────┬─────┘ └──────────┘ └──────────┘ └────┬─────┘
        │                                      │
        ▼                                      ▼
   ┌──────────────────────────────────────────────────────────────────┐
   │                         core (12)                                 │
   │   ~2,800 lines · central Runtime struct                          │
   └──┬──────────┬─────────────┬─────────────┬────────────┬───────────┘
      │          │             │             │            │
      ▼          ▼             ▼             ▼            ▼
┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐
│  agent   │ │  state   │ │   mcp    │ │  hooks   │ │  tools   │
│   (8)    │ │   (4)    │ │   (7)    │ │   (5)    │ │   (6)    │
└────┬─────┘ └──────────┘ └──────────┘ └──────────┘ └────┬─────┘
     │                                                   │
     ▼                                                   ▼
┌──────────────────────────────────────────────────────────────────┐
│                        config (9)                                 │
│   ~8,100 lines · config schema, TOML I/O, provider defaults       │
└──────┬──────────────────────────┬────────────────────────────────┘
       │                          │
       ▼                          ▼
┌──────────────┐          ┌──────────────┐
│  execpolicy  │          │   secrets    │
│     (3)      │          │     (2)      │
└──────┬───────┘          └──────────────┘
       │
       ▼
┌──────────────┐
│  protocol    │
│     (1)      │
└──────────────┘

┌──────────────┐          ┌──────────────┐
│  whaleflow   │          │   release    │
│    (11)      │          │    (10)      │
│  standalone  │          │  standalone  │
└──────────────┘          └──────────────┘
```

**Key:** Numbers in parentheses are crate indices from the table below.
Arrows (`──▶` implied by layout) mean "depends on".
- `whaleflow` and `release` are standalone (no workspace crate deps).
- `protocol` is the leaf — every other crate either depends on it directly or
  through transitive chains.
- `core` is the central hub — depended on by `cli`, `app-server`, and `tui`.

---

## 1. protocol (`codewhale-protocol`)

**Purpose:** Shared wire types for thread management, tool calls, app requests,
and event frames. This is the lowest-level crate in the dependency graph —
nearly every other crate depends on it.

**Key types:**
- `Envelope<T>` — request envelope with `request_id`, `thread_id`, and body
- `Thread`, `ThreadStatus`, `ThreadStartParams`, `ThreadResumeParams`, `ThreadForkParams`
- `ThreadRequest` / `ThreadResponse` — tagged thread-level RPC
- `ThreadGoal`, `ThreadGoalStatus` — goal tracking
- `AppRequest` / `AppResponse` — application-level RPC (capabilities, config, models)
- `PromptRequest` / `PromptResponse` — simple prompt/reply
- `ToolPayload` / `ToolOutput` — tool call payload and result
- `ToolKind` — `Function | Mcp`
- `EventFrame` — streaming event envelope
- `AskForApproval`, `ReviewDecision`, `NetworkPolicyAmendment` — approval model
- Sub-modules: `fleet`, `runtime`, `workroom`

**Workspace deps:** None (leaf crate)

**Approx. lines:** ~714 (lib.rs)

---

## 2. secrets (`codewhale-secrets`)

**Purpose:** API key storage with pluggable backends. Provides OS keyring
integration (macOS Keychain, Windows Credential Manager, Linux Secret Service)
with a file-based JSON fallback (`~/.codewhale/secrets/`). Also supports
in-memory storage for tests.

**Key types:**
- `KeyringStore` trait — `get`, `set`, `delete`, `backend_name`
- `DefaultKeyringStore` — OS-native keyring (macOS/Windows/Linux)
- `FileKeyringStore` — JSON file backend with permission checks (0600)
- `InMemoryKeyringStore` — for tests
- `SecretsError` — keyring, I/O, JSON, and permission errors
- `Secrets` — high-level resolver: checks store first, falls back to env vars
- `SecretSource` — discriminates where a key was found

**Workspace deps:** None (leaf crate)

**Approx. lines:** ~1,488 (lib.rs)

---

## 3. execpolicy (`codewhale-execpolicy`)

**Purpose:** Execution policy engine determining whether a tool invocation
requires user approval. Implements a multi-layer ruleset system (builtin
defaults, agent-layer, user-layer) with priority-based prefix matching and typed
ask rules.

**Key types:**
- `RulesetLayer` — `BuiltinDefault | Agent | User` (priority-ordered)
- `Ruleset` — trusted prefixes, denied prefixes, ask rules at a given layer
- `ToolAskRule` — typed rule matching tool name, command prefix, or file path
- `AskForApproval` — `UnlessTrusted | OnFailure | OnRequest | Reject{..} | Never`
- `ExecPolicyAmendment` — proposed trusted-prefix additions
- `ExecApprovalRequirement` — `Skip | NeedsApproval | Forbidden`
- `ExecPolicyEngine` — evaluates a command against all active rulesets
- `ExecPolicyContext` / `ExecPolicyDecision`
- `BashArityDict` — bash builtin arity dictionary for safe command-line parsing

**Workspace deps:** `protocol`

**Approx. lines:** ~853 (lib.rs)

---

## 4. state (`codewhale-state`)

**Purpose:** Persistent state management backed by SQLite (via `rusqlite`).
Stores threads, messages (tree-structured), checkpoints, thread goals, dynamic
tool registrations, and background jobs. Also maintains an append-only JSONL
session index.

**Key types:**
- `StateStore` — primary entry point: open/create, CRUD for all entities
- `ThreadMetadata` — thread record with git context, sandbox, approval mode
- `ThreadStatus` — `Running | Idle | Completed | Failed | Paused | Archived`
- `SessionSource` — `Interactive | Resume | Fork | Api | Unknown`
- `MessageRecord` — tree-structured message with `parent_entry_id`
- `CheckpointRecord` — named state snapshot
- `DynamicToolRecord` — per-thread tool registration
- `JobStateRecord`, `JobStateStatus` — background job persistence
- `ThreadGoalRecord`, `ThreadGoalStatusRecord` — goal tracking

**Workspace deps:** None (leaf crate; depends only on external crates like `rusqlite`, `serde`)

**Approx. lines:** ~2,069 (lib.rs)

---

## 5. hooks (`codewhale-hooks`)

**Purpose:** Lifecycle event dispatch system. Fans out structured events
(response start/delta/end, tool lifecycle, job lifecycle, approval lifecycle) to
registered sinks: stdout, JSONL files, and HTTP webhooks.

**Key types:**
- `HookEvent` enum — `ResponseStart | ResponseDelta | ResponseEnd | ToolLifecycle | JobLifecycle | ApprovalLifecycle | GenericEventFrame`
- `HookSink` trait — async `emit` method
- `StdoutHookSink` — prints JSON lines to stdout
- `JsonlHookSink` — appends timestamped JSON to a file
- `WebhookHookSink` — POSTs JSON to an HTTP endpoint with retry
- `HookDispatcher` — fans events to all registered sinks (best-effort, errors don't abort)

**Workspace deps:** `protocol`

**Approx. lines:** ~514 (lib.rs)

---

## 6. tools (`codewhale-tools`)

**Purpose:** Tool invocation lifecycle, schema validation, and concurrent
execution scheduler. Defines the abstraction over all agent-callable tools
(built-in functions, shell commands, MCP tools).

**Key types:**
- `ToolCapability` — `ReadOnly | WritesFiles | ExecutesCode | Network | Sandboxable | RequiresApproval`
- `ApprovalRequirement` — `Auto | Suggest | Required`
- `ToolError` — `InvalidInput | MissingField | PathEscape | ExecutionFailed | Timeout | NotAvailable | PermissionDenied`
- `ToolResult` — content + success + optional metadata
- `ToolHandler` trait — async `invoke`, `capabilities`, `approval_requirement`
- `ConfiguredToolSpec` — tool name, description, JSON Schema input
- `ToolCall`, `ToolCallRequest`, `ToolCallOutcome` — call lifecycle types
- `ToolRegistry` — maps tool names to handlers with concurrent execution via `ToolCallRuntime`
- `ToolCallRuntime` — manages locks per tool to prevent concurrent conflicting access
- `TOOL_EXECUTION_LOCK_HELD` — task-local marker

**Workspace deps:** `protocol`

**Approx. lines:** ~718 (lib.rs)

---

## 7. mcp (`codewhale-mcp`)

**Purpose:** Model Context Protocol integration. Manages MCP server process
lifecycle, tool discovery, tool invocation proxying, and resource access.
Includes both live server management and an in-memory stub for testing.

**Key types:**
- `McpServerConfig` — server name, command, args, env, enabled flag
- `McpServerDefinition` — config + `ToolFilter`
- `ToolFilter` — allow/deny lists for tool exposure
- `McpManagedClient` trait — `list_tools`, `call_tool`, `list_resources`, `read_resource`
- `InMemoryMcpClient` — stub for tests
- `McpManager` — manages multiple server connections, tool registration, startup lifecycle
- `McpToolDescriptor`, `McpResourceDescriptor` — tool/resource metadata
- `McpStartupStatus`, `McpStartupUpdateEvent`, `McpStartupCompleteEvent` — startup progress
- `McpStartupFailure` — error record for failed servers

**Workspace deps:** None (leaf crate; depends only on `anyhow`, `serde`, `serde_json`)

**Approx. lines:** ~1,406 (lib.rs)

---

## 8. agent (`codewhale-agent`)

**Purpose:** Model/provider registry with alias resolution and fallback chains.
Maps user-requested model names (including aliases) to concrete model entries
across all supported providers.

**Key types:**
- `ModelFamily` — `DeepSeek | Anthropic | OpenAI | Google | Meta | Mistral | Qwen | Grok | Cohere | GptOss | Inferencer`
- `ModelInfo` — canonical `id`, `provider`, `aliases`, `supports_tools`, `supports_reasoning`
- `ModelResolution` — resolved model + `used_fallback` + `fallback_chain`
- `ModelRegistry` — pre-populated registry of all built-in models with alias lookup

**Workspace deps:** `config` (for `ProviderKind`)

**Approx. lines:** ~1,670 (lib.rs)

---

## 9. config (`codewhale-config`)

**Purpose:** Configuration schema, TOML file I/O, provider defaults, and
runtime option resolution. This is the largest crate — it defines every
configuration key, every provider default (base URL, default model), and the
full precedence chain (CLI flags → env vars → config file → defaults).

**Key types:**
- `ProviderKind` enum — 25 variants: `Deepseek | NvidiaNim | Openai | Atlascloud | WanjieArk | Volcengine | Openrouter | XiaomiMimo | Novita | Fireworks | Siliconflow | Arcee | SiliconflowCN | Moonshot | Sglang | Vllm | Ollama | Huggingface | Together | OpenaiCodex | Anthropic | Zai | Stepfun | Minimax | Deepinfra`
- `ConfigToml` — the full on-disk config schema (all sections, all keys)
- `ConfigStore` — loads/saves `ConfigToml` from `~/.codewhale/config.toml`
- `CliRuntimeOverrides` — CLI-driven overrides merged into resolved config
- `ResolvedRuntimeOptions` — final resolved configuration after precedence
- `RuntimeApiKeySource` — where an API key was resolved from
- Sub-module: `provider` — per-provider routing tables and capability flags

**Workspace deps:** `execpolicy`, `secrets`

**Approx. lines:** ~8,080 (lib.rs)

---

## 10. release (`codewhale-release`)

**Purpose:** Release discovery, version comparison, and checksum verification.
Fetches release metadata from GitHub Releases API or CNB mirror, compares
against the running version, and downloads platform binaries with SHA-256
verification.

**Key types:**
- `ReleaseChannel` — `Stable | Beta`
- `ReleaseQuery` — `Mirror | GitHubLatest | GitHubReleaseList`
- Functions: `resolve_release_query`, `release_base_url_from_env`, `cnb_release_base_url`
- Constants: `CHECKSUM_MANIFEST_ASSET`, `LATEST_RELEASE_URL`, `RELEASES_URL`, `CNB_REPO_URL`
- Environment variables: `CODEWHALE_RELEASE_BASE_URL`, `CODEWHALE_USE_CNB_MIRROR`, `DEEPSEEK_TUI_VERSION`
- `fetch_release_json_blocking` / `fetch_release_json_async`
- `update_network_fallback_hint` — CNB mirror instructions for mainland China

**Workspace deps:** None (standalone; depends only on `reqwest`, `semver`, `serde`)

**Approx. lines:** ~766 (lib.rs)

---

## 11. whaleflow (`codewhale-whaleflow`)

**Purpose:** Typed WhaleFlow workflow IR (intermediate representation) with
validation. Defines the workflow specification language — a DAG of typed nodes
— and provides compilers from JavaScript, TypeScript, and Starlark source into
the IR. The crate stops at the Rust-owned IR boundary; runtime execution is
layered on top by consumers.

**Key types:**
- `WorkflowConfig` — top-level: goal, max_concurrent, description, phases
- `WorkflowSpec` — full spec: goal, budget, permissions, model_policy, nodes
- `WorkflowNode` enum — `BranchSet | Leaf | Sequence | Reduce | TeacherReview | LoopUntil | Cond | Expand`
- `BranchSpec`, `LeafSpec`, `SequenceSpec`, `ReduceSpec` — per-node configs
- `TeacherReviewSpec`, `LoopUntilSpec`, `CondSpec`, `ExpandSpec`
- `BudgetSpec` — max_steps, timeout_secs, max_parallel
- `PermissionSpec` — allow_write, allow_network, allowed_tools, file_scope
- `ModelPolicy`, `PromotionPolicy`, `AgentType`, `TaskMode`, `IsolationMode`
- `WorkflowPlan` — compiled, validated plan ready for execution
- `WorkflowValidationError`
- `JavascriptWorkflowResult`, `compile_javascript_workflow`, `compile_typescript_workflow`
- `compile_starlark_workflow`, `compile_starlark_workflow_with_repair` (non-OHOS only)
- Sub-modules: `js_authoring`, `starlark_authoring`, `model_policy`, `replay`

**Workspace deps:** None (standalone; optional `starlark` dependency on non-OHOS)

**Approx. lines:** ~3,121 (lib.rs)

---

## 12. core (`codewhale-core`)

**Purpose:** Central runtime combining all subsystems into one orchestrating
struct. The `Runtime` owns the config, model registry, thread manager, tool
registry, MCP manager, exec policy engine, hook dispatcher, and job manager.
All three entry points (CLI, TUI, app-server) construct a `Runtime` and drive it.

**Key types:**
- `Runtime` — the central struct:
  - `config: ConfigToml`
  - `model_registry: ModelRegistry`
  - `thread_manager: ThreadManager`
  - `tool_registry: Arc<ToolRegistry>`
  - `mcp_manager: Arc<McpManager>`
  - `exec_policy: ExecPolicyEngine`
  - `hooks: HookDispatcher`
  - `jobs: JobManager`
- `ThreadManager` — thread lifecycle (create, resume, fork, archive, list)
- `NewThread` — result of spawning/resuming a thread
- `InitialHistory` — `New | Forked | Resumed`
- `JobManager` — background job lifecycle with retry, persistence, history
- `JobRecord`, `JobStatus`, `JobRetryMetadata`, `JobHistoryEntry`

**Workspace deps:** `agent`, `config`, `execpolicy`, `hooks`, `mcp`, `protocol`, `state`, `tools`

**Approx. lines:** ~2,767 (lib.rs)

---

## 13. cli (`codewhale-cli`)

**Purpose:** Headless CLI entry point. Provides the `codewhale` binary with
subcommands for `exec` (headless agent runs), `auth` (provider key management),
`update` (self-update), `mcp` (run as MCP server), `doctor` (diagnostics),
`thread` (CRUD), `config` (get/set/list), `models` (list), shell completions,
and more.

**Key types:**
- `Cli` struct (clap-derived) — all CLI flags and subcommands
- `Commands` enum — `Exec | Auth | Update | Mcp | Doctor | Thread | Config | Models | Complete | AppServer`
- `ProviderArg` — clap-compatible provider enum mirroring `ProviderKind`
- `ExecArgs`, `AuthArgs`, `UpdateArgs`, `McpArgs`, `ThreadArgs`
- `run_cli()` — main entry point (called from `src/main.rs`)
- `run_exec()` — headless agent execution with `--allowed-tools`, `--max-turns`
- `run_auth_set()` — store provider API key
- `run_update()` — self-update with checksum verification
- `run_mcp_stdio()` — expose CodeWhale as an MCP server over stdio
- Sub-modules: `metrics`, `update`

**Workspace deps:** `agent`, `app-server`, `config`, `execpolicy`, `mcp`, `release`, `secrets`, `state`

**Approx. lines:** ~4,270 (lib.rs)

**Binaries:** `codewhale` (main), `codew` (legacy shim → forwards to `codewhale`)

---

## 14. app-server (`codewhale-app-server`)

**Purpose:** HTTP + stdio API server exposing the runtime over REST, SSE, and
JSON-RPC. Serves as the backend for web extensions (VS Code), the Tauri desktop
app, and programmatic clients. Supports CORS, bearer-token auth, and stdio
transport for MCP integration.

**Key types:**
- `AppServerOptions` — listen address, config path, auth token, CORS origins
- `AppState` — shared server state: config, runtime, registry, pending user input
- `AppTransport` — `Http | Stdio`
- `ToolCallRequest`, `JsonRpcRequest`, `JsonRpcError`
- `ConfigGetParams`, `ConfigSetParams`, `ThreadIdParams`, `ThreadMessageParams`
- `run()` — starts the HTTP server (axum router with CORS, auth middleware)
- `run_stdio()` — starts the stdio JSON-RPC server
- `run_stdio_server()` (in `crates/mcp`) — MCP server over stdio
- `chat_completions` sub-module — OpenAI-compatible `/v1/chat/completions` endpoint

**Workspace deps:** `agent`, `config`, `core`, `execpolicy`, `hooks`, `mcp`, `protocol`, `state`, `tools`

**Approx. lines:** ~1,524 (lib.rs)

---

## 15. tui (`codewhale-tui`)

**Purpose:** Full interactive terminal UI. The flagship interface: a ratatui-based
TUI with tabs, file browser, model switching, goal loop, session management,
sandbox controls, and the full tool palette. This is the largest crate by a wide
margin — it contains the entire interactive experience.

**Key types:**
- `Cli` struct (clap-derived) — TUI-specific flags (yolo, model, provider, workspace, etc.)
- `Commands` enum — `Exec | Eval | Auth | Update | Mcp | Doctor | Thread | Config | Models | Complete | AppServer | Purge | Skills | Fleet | Acp`
- `Config` — TUI config (from `~/.codewhale/config.toml`)
- `SessionManager` — interactive session lifecycle
- `LlmClient` — HTTP client for model providers with retry and streaming
- `McpPool` — MCP server connection pool
- `Message`, `ContentBlock`, `MessageRequest`, `SystemPrompt` — LLM message types
- `EvalHarness` — evaluation framework with scenario steps
- ~100 sub-modules covering: TUI rendering, model routing, sandbox,
  automation, skills, fleet, MCP, RLM, project context, tools, snapshots,
  goal loop, task manager, memory, localization, and more.

**Workspace deps:** `config`, `execpolicy`, `protocol`, `release`, `secrets`, `tools`

**Approx. lines:** ~8,973 (main.rs)

**Note:** The TUI crate does **not** depend on `core` or `app-server` — it
implements its own session management, LLM client, and MCP pool directly,
sharing only the lower supporting crates with the rest of the workspace.

---

## Summary Table

| # | Crate | Package Name | Lines | Workspace Deps |
|---|---|---|---|---|
| 1 | protocol | `codewhale-protocol` | ~714 | — |
| 2 | secrets | `codewhale-secrets` | ~1,488 | — |
| 3 | execpolicy | `codewhale-execpolicy` | ~853 | protocol |
| 4 | state | `codewhale-state` | ~2,069 | — |
| 5 | hooks | `codewhale-hooks` | ~514 | protocol |
| 6 | tools | `codewhale-tools` | ~718 | protocol |
| 7 | mcp | `codewhale-mcp` | ~1,406 | — |
| 8 | agent | `codewhale-agent` | ~1,670 | config |
| 9 | config | `codewhale-config` | ~8,080 | execpolicy, secrets |
| 10 | release | `codewhale-release` | ~766 | — |
| 11 | whaleflow | `codewhale-whaleflow` | ~3,121 | — |
| 12 | core | `codewhale-core` | ~2,767 | agent, config, execpolicy, hooks, mcp, protocol, state, tools |
| 13 | cli | `codewhale-cli` | ~4,270 | agent, app-server, config, execpolicy, mcp, release, secrets, state |
| 14 | app-server | `codewhale-app-server` | ~1,524 | agent, config, core, execpolicy, hooks, mcp, protocol, state, tools |
| 15 | tui | `codewhale-tui` | ~8,973 | config, execpolicy, protocol, release, secrets, tools |

**Total workspace:** ~39,000 lines of Rust across 15 crates.
