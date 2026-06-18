# Orchestration Terminology

CodeWhale should expose two orchestration concepts in user-facing copy:

1. **Agents**
2. **Workflows**

Everything else is an implementation layer, compatibility alias, or architecture
detail.

## Public Names

### Agents

An **Agent** is delegated work with its own role, lifecycle, model route, tool
permissions, transcript, and status.

Use **Agents** for:

- child or delegated work launched from a parent session
- background workers
- role-based scouts, reviewers, implementers, and verifiers
- local or remote workers launched by the durable control plane
- status/sidebar rows that show running delegated work

Public examples:

- "Open an Agent to review this diff."
- "Agents can run locally or remotely."
- "Agents report receipts, artifacts, and status back to the parent."

### Workflows

A **Workflow** is a repeatable multi-step plan that orchestrates agents and
control-flow nodes.

Use **Workflows** for:

- DAGs, phases, branches, reductions, loops, and tournaments
- replayable multi-agent plans
- teacher review and promotion gates
- durable orchestration that spans many agents or runs
- user-authored `.workflow.*`, Starlark, JSON, or TOML specs

Public examples:

- "Run a Workflow to audit the release."
- "Workflows orchestrate Agents through repeatable plans."
- "Workflow replay verifies the same plan without live model calls."

## Internal Names

| Internal name | Public framing | Notes |
|---|---|---|
| `sub-agent` / `subagent` | Agent, child Agent | Keep in code identifiers, config keys, compatibility docs, and protocol fields. Avoid as the headline product term. |
| `Fleet` | Agent control plane | Fleet is the scheduler, ledger, host transport, receipt store, and durable worker substrate for Agents. |
| `WhaleFlow` | Workflow engine | WhaleFlow is the Rust IR/compiler/replay engine behind Workflows. |
| `Workroom` | collaboration context | Workrooms organize threads, links, events, and shared visibility. They are not a third orchestration concept. |
| `/swarm` | high-fanout Workflow behavior | Keep gated or compatibility-only until it compiles into Workflow-backed Agent runs. |

## Naming Rules

- Prefer **Agents** and **Workflows** in website, README, wiki, release notes,
  screenshots, and first-run UI.
- Use internal names only when explaining source modules, config compatibility,
  protocol types, or migration details.
- When an internal name appears, define it through the two public names:
  "Fleet is the Agent control plane" or "WhaleFlow is the Workflow engine."
- Do not present Fleet, WhaleFlow, Workrooms, sub-agents, and swarm as five
  separate product concepts.
- Keep stable commands and config keys until a separate compatibility issue
  intentionally renames them.

## Recommended Surface Map

| Surface | Preferred label | Compatibility details |
|---|---|---|
| Sidebar panel | Agents | Existing `/subagents` may remain as an alias. |
| Config UI section | Agents | Existing `[subagents]` keys remain stable. |
| Workflow authoring docs | Workflows | Mention WhaleFlow once as the engine name. |
| Fleet docs | Agent control plane | Keep `codewhale fleet` as the CLI implementation surface. |
| Workroom docs | Collaboration context | Keep workroom links/protocol language for architecture docs. |
| Slash command docs | `/agents`, `/workflows` direction | Existing `/agent`, `/subagents`, `/fleet`, `/swarm` require compatibility planning before renaming. |

## One-Sentence Product Description

CodeWhale has two orchestration concepts: **Agents** for delegated work, and
**Workflows** for durable multi-agent plans.
