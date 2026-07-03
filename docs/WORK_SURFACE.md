# Work surface

CodeWhale projects a single **Work surface** from two cooperating subsystems:
a counted **checklist** (the execution ledger) and optional **strategy
metadata** (phase-level context from `update_plan`). Both are model-facing, not
just a TUI decoration: the current Work state is synthesized and injected into
the model's context on every parent turn, every sub-agent fork, and every
`/relay` instruction.

## Conceptual model

| Surface | Tool | Role | Model-facing? |
|---|---|---|---|
| **Checklist** | `checklist_write`, `checklist_add`, `checklist_update`, `checklist_list` | Counted execution ledger. Every item carries a status (pending, in-progress, completed) and a completion percentage is tracked. This is the **primary progress surface** — it is subordinate to the durable task and should not be duplicated by other tools. | Yes — rendered as a `[ ]/[~]/[x]` list in every continuity path. |
| **Strategy metadata** | `update_plan` | Optional high-level phase context: objective, evidence, constraints, critical files, verification plan, risks, and handoff packet. Plan-mode agents use the richer PlanArtifact shape; Agent/YOLO modes can optionally set strategy fields. | Yes — rendered as labelled bullet fields (`- Title: …`, `- Objective: …`) and phase steps with the same `[ ]/[~]/[x]` markers. |

Strategy metadata and checklist work are **one surface**, not two peer progress
systems. When both exist, the Work block groups strategy around the checklist
so the model sees a single coherent picture of what is being done and why.

## TUI sidebar

In the TUI, the Work surface is the **To-do** / **Strategy context** panel
in the sidebar (`crates/tui/src/tui/sidebar.rs`). In Auto focus mode the
sidebar collapses to nothing when there is no active content (no To-do, no
live/queued fleet, no background jobs, no pinned context) so an idle session
gets a full-width transcript. Any active content brings it back.

The sidebar rendering is driven by `SidebarWorkSummary` which snapshots the
current goal, checklist items with completion percentage, strategy explanation,
and strategy steps from the same shared state that the model-facing blocks
consume.

## Model-facing `<work_state>` blocks

CodeWhale injects the current Work state into the model's context as a
`<work_state>` XML block. This block is **not** part of the static
system-prompt head — it changes every turn as the checklist advances, so it
occupies the same trailing, post-cache-boundary position as `<turn_meta>`.
Placing it in the stable system-prompt head would bust the KV prefix cache on
every turn.

### Block shape

```xml
<work_state>
### Work

Checklist (67% complete)

- [x] Read the relevant source files
- [~] Write the implementation
- [ ] Run verification
- [ ] Update docs

Strategy metadata

- Title: Add WebSocket keep-alive heartbeat
- Objective: Prevent proxy timeouts on idle connections
- Context: Nginx terminates idle WebSocket connections after 60s
- Source: crates/app-server/src/ws.rs:45-67
- Critical file: crates/app-server/src/ws.rs
- Recommended approach: Send a ping frame every 30s from the server side
- Verification plan: Run the existing ws integration test suite; add a 90s idle test
- Risks and unknowns: Mobile clients may not handle ping frames gracefully
- Handoff packet: The ping interval is configurable; default 30s, clamped to 10..300

- [x] Investigate the idle-disconnect logs
- [~] Implement the ping loop
- [ ] Add the 90s idle integration test

### Open Sub-Agents

- `agent-abc123` (role: explorer) - Find all call sites of handle_ws_message
</work_state>
```

### When the block is injected

The Work state block appears in three continuity paths, all rendered by the
same shared `StructuredState::capture()` + render pipeline
(`crates/tui/src/core/engine.rs`):

1. **Parent turn.** The block is appended after `<turn_meta>` in the trailing
   region of every user message. This re-grounds the parent model after long
   sessions, compaction, resume, or drift — without it, the model only sees
   stale tool results that may have been compacted away.

2. **Sub-agent fork.** Every forked child receives the parent's current Work
   state as `## Fork State` in its system context. This is the existing and
   tested path (`engine.rs:2428-2444`).

3. **`/relay`.** The relay instruction includes both checklist items and
   strategy metadata so the handoff artifact preserves the same continuity
   signal as a fork or a fresh parent turn (`relay.rs:44-169`).

All three paths consume the same `TodoListSnapshot` and `PlanSnapshot` from
the shared `checklist_*` and `update_plan` state. They cannot drift.

### Prefix-cache discipline

`<turn_meta>` is intentionally placed last in the user message for cache
stability: the leading bytes (the user's text) stay stable across date /
model-route / working-set changes, and only the trailing metadata block varies.
`<work_state>` follows the same law — it is placed next to `<turn_meta>` in
the trailing region so the stable user-input prefix remains cacheable. This
matters for DeepSeek's KV prefix cache, which matches byte sequences from the
start of each message.

### Empty state

When the checklist is empty and no strategy metadata is set, the block is
omitted entirely — it is not injected as an empty `<work_state/>` tag. This
avoids adding noise to turns that don't use planning.

## Relationship to durable tasks

The checklist is subordinate to the durable task object (`task_create` /
`task_list` / `task_read`). `checklist_write` replaces the active thread/task
checklist; `task_read` includes the current checklist in its detail output.
The task is the lifecycle owner — checklist is the model-visible projection of
progress.

## Relationship to sub-agents

Sub-agents receive the parent's Work state in their fork context as `## Fork
State` (see `docs/SUBAGENTS.md`). The child can update its own checklist
independently; parent and child checklists are separate surfaces. When a
sub-agent completes, the parent sees the child's result through `agent` but
the parent's own Work state is unchanged until the parent explicitly updates
it.

## Legacy compatibility

The legacy `todo_*` names (`todo_write`, `todo_add`, `todo_update`,
`todo_list`) are deprecated hidden compatibility aliases. They remain callable
for saved transcript replay but are not advertised to the model catalog.
Compatibility results include `_deprecation.use_instead = checklist_*`.

## See also

- `docs/TOOL_SURFACE.md` — tool catalog and the `checklist_*` / `update_plan` entries
- `docs/TOOL_LIFECYCLE.md` — lifecycle policy, deprecation manifest, and the canonical work-tracking surface
- `docs/SUBAGENTS.md` — fork-state handoff and sub-agent Work state
- `crates/tui/src/core/engine.rs` — `StructuredState::capture()` (line 86), `to_system_block()` (line 139), and parent-turn injection
- `crates/tui/src/commands/groups/session/relay.rs` — relay instruction builder
- `crates/tui/src/tui/sidebar.rs` — TUI sidebar Work panel rendering
