# CodeWhale — Project Overview

**Version:** 0.8.62 · **Rust edition:** 2024 · **Min rustc:** 1.88 · **License:** MIT

## What is CodeWhale?

CodeWhale is an **open-source AI coding agent and LLM harness**. It runs on your
machine (terminal TUI, CLI, or embedded app-server), connects to 25+
model providers, and executes multi-step software engineering tasks: reading
code, making edits, running shell commands, verifying results, planning, and
course-correcting when something fails.

It is model-agnostic: DeepSeek and open-weight models are first-class, but
Claude, GPT, Kimi, GLM, and local vLLM/Ollama are full peers. Switch providers
and models mid-session without restarting.

CodeWhale is written in Rust and ships as three binaries:
- `codewhale` (CLI)
- `codewhale-tui` (terminal UI)
- `codew` (legacy shim → `codewhale`)

---

## High-Level Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                         ENTRY POINTS                                │
│                                                                     │
│   ┌──────────┐    ┌──────────────┐    ┌──────────────────────┐      │
│   │   CLI    │    │     TUI      │    │    APP-SERVER         │      │
│   │ (clap)   │    │ (ratatui +   │    │ (axum HTTP + SSE +   │      │
│   │          │    │  schemaui)   │    │  ACP + stdio)        │      │
│   └────┬─────┘    └──────┬───────┘    └──────────┬───────────┘      │
│        │                 │                       │                  │
│        └─────────────────┼───────────────────────┘                  │
│                          ▼                                          │
│   ┌──────────────────────────────────────────────────────────────┐ │
│   │                        CORE RUNTIME                           │ │
│   │   ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐   │ │
│   │   │ ThreadManager│  │  ToolRegistry │  │   McpManager     │   │ │
│   │   │ (lifecycle,  │  │  (invoke,     │  │  (external tool  │   │ │
│   │   │  fork/resume)│  │   validate)   │  │   servers)       │   │ │
│   │   └──────────────┘  └──────────────┘  └──────────────────┘   │ │
│   │   ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐   │ │
│   │   │ ExecPolicy   │  │ HookDispatcher│  │  JobManager      │   │ │
│   │   │ (approval)   │  │ (event sinks) │  │  (background)    │   │ │
│   │   └──────────────┘  └──────────────┘  └──────────────────┘   │ │
│   └──────────────────────────────────────────────────────────────┘ │
│                          │                                          │
│          ┌───────────────┼───────────────────────┐                  │
│          ▼               ▼                       ▼                  │
│   ┌──────────┐   ┌──────────────┐   ┌──────────────────┐          │
│   │  agent   │   │  whaleflow   │   │       RLM        │          │
│   │ (model   │   │ (workflow    │   │ (persistent      │          │
│   │ registry)│   │  orchestr.)  │   │  Python REPL)    │          │
│   └──────────┘   └──────────────┘   └──────────────────┘          │
│                                                                     │
│   ┌──────────────────────────────────────────────────────────┐     │
│   │                   SUPPORTING LAYER                        │     │
│   │  protocol  │ state │ config │ secrets │ execpolicy │ hooks│     │
│   │  tools     │  mcp  │ release│                                     │
│   └──────────────────────────────────────────────────────────┘     │
└─────────────────────────────────────────────────────────────────────┘
```

### Entry Points

| Entry Point | Crate | Transport | Description |
|---|---|---|---|
| **CLI** | `crates/cli` | Terminal stdin/stdout | Headless exec, auth, config, update, MCP server |
| **TUI** | `crates/tui` | Terminal (ratatui) | Full interactive terminal UI with tabs, file browser, goal loop |
| **App-Server** | `crates/app-server` | HTTP/SSE (axum), stdio JSON-RPC | Embedded API for web extensions, Tauri, and remote tools |

### Web Layer

- **`web/`** — Next.js community site ([codewhale.net](https://codewhale.net/))
- **`codew/`** — Tauri desktop app shell that embeds the app-server

---

## The 15 Crates at a Glance

| # | Crate | One-Line Description |
|---|---|---|
| 1 | **protocol** | Shared wire types: threads, tool payloads, app requests, event frames |
| 2 | **secrets** | API key storage: OS keyring + file-backed fallback |
| 3 | **execpolicy** | Execution policy engine: trust rules, approval gating, sandbox decisions |
| 4 | **state** | SQLite-backed persistence: threads, messages, checkpoints, jobs |
| 5 | **hooks** | Lifecycle event dispatch: stdout, JSONL file, webhook sinks |
| 6 | **tools** | Tool invocation lifecycle, schema validation, concurrent scheduler |
| 7 | **mcp** | Model Context Protocol: server lifecycle, tool proxy, resource access |
| 8 | **agent** | Model/provider registry with alias resolution and fallback chains |
| 9 | **config** | Config schema, TOML I/O, provider defaults, runtime resolution |
| 10 | **release** | Release discovery, version checking, checksum verification, CNB mirrors |
| 11 | **whaleflow** ⚠️ | Workflow IR (defined, experimental): typed spec with validation, Starlark/JS authoring, deterministic replay — not yet wired into runtime |
| 12 | **core** | Central runtime: threads, tools, MCP, exec policy, hooks, jobs in one struct |
| 13 | **cli** | Headless CLI: `exec`, `auth`, `update`, `mcp`, `doctor`, shell completions |
| 14 | **app-server** | HTTP + stdio API server exposing the runtime over REST, SSE, and JSON-RPC |
| 15 | **tui** | Full terminal UI: interactive sessions, model switching, file browser, goal loop |

---

## Key Concepts

### Threads

A **thread** is a persisted conversation session. Threads track the model
provider, working directory, git context, sandbox policy, and message tree.
Threads can be created, resumed, forked, archived, and listed. Goals can be
attached to threads for persistent multi-turn objective tracking.

### Tools

Tools are the agent's interface to the world: `read_file`, `edit_file`,
`exec_shell`, `grep_files`, `file_search`, `git_diff`, and 40+ others. Every
tool invocation goes through the **execpolicy** engine which determines whether
approval is needed, and through the **hooks** system which fans out lifecycle
events.

### Sub-Agents

The agent can spawn **sub-agents** — independent child tasks that run in
parallel (up to 20 at once) with their own clean context. Sub-agents are
provider-aware: the parent can assign a cheaper/faster model tier. Each
sub-agent returns a structured output contract.

### RLM (Persistent Python REPL)

**RLM** (Rust Language Model) contexts are persistent Python kernels that live
across turns. The agent can load a file into an RLM, run Python REPL blocks
against it, and retrieve structured results via `handle_read`. This is used for
large-file inspection, data analysis, and programmatic transformations without
blowing up the agent context window.

### WhaleFlow (Workflow Orchestration)

**WhaleFlow** is a typed workflow IR (intermediate representation) for defining
multi-step agent pipelines. Workflows are authored in JavaScript, TypeScript,
or Starlark and compile to a `WorkflowSpec` containing nodes: `BranchSet`,
`Leaf`, `Sequence`, `Reduce`, `TeacherReview`, `LoopUntil`, `Cond`, and
`Expand`. Each node carries its own budget, permissions, and model policy.

---

## Constitution: Nested Authority

CodeWhale uses a **nested constitution** to resolve conflicts in the mountain
of context an agent accumulates. The system prompt is layered, most-static first:

1. **Global constitution** — compiled into every binary
2. **Project constitution** — `.codewhale/constitution.json` in your repo
3. **Current request** — the operative instruction this turn
4. **Live evidence** — what the tools actually returned

When two instructions conflict, each yields to the one above. The model doesn't
renegotiate the stack — it acts on overlapping context without paralysis.

---

## Capabilities at a Glance

- **25 providers** — DeepSeek, GLM, Claude, GPT, Kimi, MiniMax, OpenRouter, local vLLM/SGLang/Ollama, and more
- **Three modes** — Plan (read-only), Agent (executes with approval), YOLO (auto-approve)
- **Persistent goal loop** — `/goal` keeps the agent working until done, blocked, or stopped
- **Rollback** — side-git snapshots and `/restore`; never touches your repo's `.git`
- **Sandboxing** — bwrap, Landlock, Seatbelt, seccomp; configurable per thread
- **MCP bidirectionally** — consume tools from external servers, or expose CodeWhale as an MCP server
- **Skills** — reusable workflows in `~/.codewhale/skills/`
- **Durable sessions** — survive restarts and system sleep
- **Headless mode** — `codewhale exec` for scripts and CI
- **Embedded** — HTTP/SSE and ACP runtime APIs, VS Code extension, Telegram/Feishu bridges
