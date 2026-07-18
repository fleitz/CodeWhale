# Operations: the Operator as COO

Status: draft for discussion (not scheduled)
Scope: product + architecture direction for Operate mode, Fleet, and Workflow
Audience: maintainers, contributors, and anyone writing user-facing copy

---

## 1. Summary

CodeWhale already spawns real, independent CodeWhales: a Fleet worker is a full
headless `codewhale exec` run tracked in a durable ledger with leases,
heartbeats, and resume. What we have not done is give that power a **model**, a
**face**, or a **name users can repeat**.

This document proposes all three:

- **Model.** Extend the ReAct loop every agent already runs with a third
  primitive: **Expand**. At each step an agent may *Reason*, *Act inline*, or
  *Expand* — hand a subgraph of the work to a new, fully independent CodeWhale.
  Expansion is governed by one legible rule (§5) instead of vibes.
- **Shape.** The running system is an **org chart**: a dynamic graph of
  CodeWhales with the **Operator at the head as COO** — it plans the graph,
  staffs it, arbitrates resources, integrates results, and reports up to the
  only person above it: the user.
- **Contract.** Parallelism becomes safe and fast through three standing
  mechanisms — write leases, interface-first decomposition, and an integrator
  lane — plus an approval inbox so the human is never the serialization point.

The promised outcome, in the user's terms: **tasks complete faster, at higher
quality, and end up costing less** — and the receipts prove it after every
operation instead of asking anyone to take our word for it.

## 2. Product vocabulary

Consistency here matters more than cleverness. One noun per concept, used
identically in the TUI, docs, website, and README.

| Term | Meaning |
|---|---|
| **Operator** | The CodeWhale at the head of the chart. The COO: plans, staffs, arbitrates, integrates, reports. Does as little direct work as possible. |
| **Operation** | A goal handed to the Operator, plus an objective and live meters. The unit users start, watch, attach to, and get a receipt for. |
| **Org chart** | The live graph of CodeWhales in an operation: who reports to whom, who owns what, what state each is in. |
| **Expand** | The act of spawning a peer CodeWhale to own a subgraph end-to-end. The third primitive of the loop (§5). |
| **Lease** | An enforced, ledger-recorded grant of write access to a slice of the repo. |
| **Integrator** | The standing role that merges worker branches into the train. Workers never merge. |
| **Receipt** | The end-of-operation evidence: wall-clock, speedup, spend, quality checks, rework ratio. |

"Operate" the TUI mode stops being a posture ("scheduling emphasis, not
authority" — MODES.md today) and becomes the mode in which you run Operations.

## 3. Problem

1. **Parallelism is the top pain.** Users cannot fan work out efficiently.
   Worktrees vs. branches is unresolved ceremony, agents step on each other,
   and merge pain arrives late, after worker context is gone.
2. **Operate is illegible.** It currently changes emphasis, not meaning. Users
   can't say what it *is*, so they can't want it.
3. **Fleet is invisible.** Durable multi-worker runs exist but render as shell
   status lines. Nobody notices parallelism they cannot see.
4. **Subagent fan-out serializes through the parent.** Every result that flows
   back through a parent transcript burns parent tokens and forces a
   sequencing point. The parent context window is a serial bus.
5. **The human is the hidden bottleneck.** Under Ask posture, N workers means
   N scattered approval prompts. Wall-clock gains evaporate at the keyboard.
6. **Hard budgets feel like handcuffs.** Users want cost *visibility and
   control*, not a ceiling that kills an operation at 90% done.

## 4. What exists today (build on it, don't rebuild it)

| Capability | Where it lives | Role in this design |
|---|---|---|
| Durable workers = full `codewhale exec` runs | Fleet (`crates/lane`, FLEET.md ledger, leases, heartbeats, `fleet resume`) | The execution substrate. Already "separate CodeWhales entirely." |
| Role profiles with per-role model/provider/thinking | `/fleet setup`, `.codewhale/agents/<role>.toml` | The staffing system for the org chart. |
| Ordered phases, gates, deterministic fan-in | Workflow overlay (`crates/workflow`) | The Operator's plan format for structured operations. |
| Per-tool-call allow/deny/ask | Hooks + execpolicy | Enforcement layer for leases (§7). |
| Repo law that Full Access can't skip | `.codewhale/constitution.json` write holds | Where the concurrency contract and expansion rule live. |
| Cheap coordination model seam | Fin (`deepseek-v4-flash`), RLM `sub_query_batch` | Operator-side triage, summarization, routing at near-zero cost. |
| Real per-route pricing, `/goal` token targets | config routes, MODES.md | The metering substrate for §6. |
| Durable, addressable threads of work + event log | Workroom draft (rfcs/3209, WORKROOM_ARCHITECTURE.md) | The Operation's identity, event stream, and attach surface. |
| Chat bridges + Termux | `integrations/`, TERMUX.md | Remote attach: run at home, supervise from a phone. |
| Configurable sub-agent depth | agent surface (AGENTS.md guardrails) | The recursion bound for nested expansion. |

Nothing in this design requires a new execution engine. It requires a model, a
contract, and a cockpit on top of what is listed above.

## 5. The core model: ReAct → Reason · Act · **Expand**

Classic ReAct alternates *reasoning* and *acting*. CodeWhale's loop adds a
third choice at every step:

```
loop:
  Reason   — think about the state of the goal
  choose one:
    Act     — do the next piece of work inline (tools, edits, shell)
    Expand  — carve off a subgraph and spawn a peer CodeWhale to own it
  observe results (own tools, or ledger events from expanded peers)
```

**Expand is not "call a subagent."** An expanded CodeWhale is a peer process
with its own context, its own loop (it may itself Expand, within the
configured depth bound), its own worktree, and its own lease. It communicates
**through artifacts** — commits, ledger events, receipts — never through the
parent's transcript. The parent reads state transitions and summaries, not
conversations. This is what breaks the serial-bus problem (§3.4) and what
makes supervision nearly free.

### 5.1 The expansion rule ("offload when operationally optimal")

One legible rule, stated in the constitution so every CodeWhale in the chart
operates the same way:

> **Expand when the work is independent of your current context AND its
> expected volume exceeds the handoff cost. Otherwise act inline.**

Concretely:

- **Independent**: it can be specified in a self-contained brief (goal, lease,
  interfaces, done-criteria) without shipping your transcript along.
- **Expected volume exceeds handoff cost**: handoff has a real fixed price —
  worktree setup, context bootstrap, integration. A ~3-minute task runs
  inline, always. Nothing destroys trust in "it's faster" like an operation
  that is slower than doing the work directly.
- **Corollary — refuse to over-expand**: the Operator estimates before
  fanning out and declines when the graph is too small. Declining to expand
  is a success mode and appears on the receipt as such.

### 5.2 The org chart

The Operator sits at the head as **COO**. Its job description, in priority
order:

1. **Plan the graph.** Decompose the goal into a task DAG. First artifact is
   the *contract commit* (§7.2): interfaces, stubs, seams — pushed before
   anyone else starts.
2. **Staff it.** Assign roles from Fleet profiles; pick each role's
   model/provider/thinking per the operation's objective (§6). Frontier model
   where judgment concentrates (planning, review), open models where volume
   concentrates (implementation, tests, mechanical work).
3. **Arbitrate.** Own the lease map. Grant, revoke, and reassign write access
   as the graph evolves. Field escalations.
4. **Integrate.** Run (or delegate) the integrator lane (§7.3).
5. **Report.** Maintain the live org chart view, the approval inbox, and the
   final receipt. The Operator does as little direct work as possible — a COO
   who is busy typing is a planning failure.

Standing roles the Operator can staff: **Leads** (own a subgraph, may expand
further), **Workers** (own a leaf task end-to-end), **Integrator** (merge
train), **Verifier** (independent checks; tournaments in §8). Roles are Fleet
profiles — this is `/fleet setup` content, not new machinery.

## 6. The Operation object: meters, not ceilings

An Operation is:

```
goal        "migrate the config system to the new route model"
objective   minimize-time | minimize-cost | maximize-quality   (pick one to lead)
meters      live spend, token mix, wall-clock, burn rate       (always on)
notify-at   optional soft thresholds ("tell me at $5")         (never a wall)
chart       the live org graph + lease map + DAG
receipt     the end-of-operation evidence
```

**Deliberately absent: a hard budget.** Hard caps are handcuffs — they kill
operations at 90% complete and train users to over-provision. Instead:

- **Meters are always visible.** Live spend and burn rate in the cockpit
  header, per-worker and total. Real route pricing already exists; unknown
  prices display as unknown, never $0.
- **Thresholds notify, humans decide.** `--notify-at $5` pings the approval
  inbox (and bridges) with the chart state and a projection; the user chooses
  to continue, narrow, or wind down. The default posture is *transparency
  with an easy brake*, not a wall.
- **The objective shapes spending; it doesn't cap it.** `minimize-cost` makes
  the Operator staff cheaper loadouts, disable speculation, and prefer inline
  work. `minimize-time` widens fan-out and enables speculative execution.
  `maximize-quality` buys tournaments and verification lanes.
- Hard caps remain available for CI/unattended runs (`--hard-cap`), where a
  wall genuinely is the right tool. They are never the default and never
  required.

## 7. The concurrency contract

Solved once, at the system level, encoded in constitution + hooks — never
re-negotiated by individual agents.

### 7.1 Write leases

The Operator partitions the repo (crate, directory, or file set) and records
leases in the ledger. **Hooks enforce them**: a worker writing outside its
lease is denied at the tool layer and must request the lease from the
Operator. Conflicts stop being merge-time surprises and become cheap,
explicit, logged arbitration events. Fleet's ledger already reconciles task
leases with heartbeats and worker death; this extends the same concept to
file ownership.

### 7.2 Interface-first decomposition (the contract commit)

Most collisions are not overlapping edits; they are implicit disagreements
about seams. The Operator's first artifact is therefore a **contract commit**
on the operation's base branch: module boundaries, type signatures, stubs.
Workers code against agreed interfaces in isolated worktrees. Pure
convention, zero new machinery, and the single highest-leverage quality move
for parallel work.

### 7.3 The integrator lane

Nobody chooses between worktrees and branches ever again, because the
Operation always does the same thing:

- **worktree-per-worker, branch-per-task, integration-by-lane.**
- A standing Integrator merges landed worker branches into a train branch
  continuously, surfacing conflicts **while both workers still have context**
  (cheap now, expensive after they're gone), running the targeted test gate,
  and fast-forwarding the real branch only when green.
- Workers never merge. Humans never resolve agent-vs-agent conflicts.

This automates the maintainer practice this repo already trusts — scratch PR
trains that absorb candidate work to expose conflicts early, then harvest
clean slices (CLAUDE.md / AGENTS.md stewardship). The pattern is proven here;
the design just gives it a role and a loop.

### 7.4 Rework ratio: the honesty metric

Every receipt reports **rework ratio** — tokens spent on conflict resolution,
re-implementation, and integration fixes as a share of total. It is the
number that keeps the expansion rule honest: a chart that fans out too wide
or leases too loosely shows up as rework, publicly, on its own receipt.

## 8. Scheduling: where faster/better/cheaper actually comes from

- **Critical-path first.** The Operator starts the *longest chains* in the
  DAG first, not the easiest tasks. The longest chain sets wall-clock
  regardless of how many easy wins land early.
- **Speculative work on cheap tokens.** Under `minimize-time` with a
  local/open model in the loadout, start probable-next tasks before they are
  confirmed. Wasted speculation on a local model costs ~nothing; saved
  wall-clock is real.
- **Tournaments for quality.** Parallelism makes redundancy cheap: race two
  open-model workers on a risky task, let a Verifier pick. Quality rises
  while total cost stays below one frontier-model attempt. This is the
  concrete resolution of the faster/better/cheaper triangle rather than a
  trade-off between its corners.
- **Right-sized inline work.** Per §5.1, small tasks never expand.

### 8.1 The approval inbox

Under Ask posture, N workers must not mean N scattered prompts. All asks from
all workers queue into **one Operator surface**, deduplicated and
pattern-grouped: "4 workers want `cargo test` — approve the pattern for this
operation?" Grants become operation-scoped rules (typed persistent permission
rules, issue #1186, slots exactly here). The inbox also renders on the chat
bridges, so an operation at home can be supervised from a phone. This
unglamorous feature decides whether parallelism *feels* fast.

## 9. Surfaces: the cockpit

**Principle: orchestration lives in the ledger; every surface is a
projection.** Nothing scrapes terminals; nothing depends on any renderer.

- **tmux cockpit.** `codewhale operate "<goal>"` creates or attaches a tmux
  session named for the operation: an Operator window (org chart, meters,
  approval inbox) plus one pane per worker running an attach viewer against
  that worker's event stream. Panes are views: kill one and the worker keeps
  running; reattach from any terminal, including over SSH from Termux. tmux
  is chosen because it is the terminal-native way to *see* parallelism — and
  a grid of CodeWhales visibly working in parallel is also the screenshot
  that travels (§10).
- **TUI fallback.** Where tmux is absent (Windows, minimal environments), the
  same projection renders as the existing workroom/overlay surface inside one
  TUI. Feature parity, different geometry.
- **Workroom identity.** Each operation is a workroom (rfcs/3209): stable id,
  `codewhale://workroom/...` link, event log with agent attribution. That id
  is what the TUI, tmux, bridges, and the runtime API all attach to.
- **Bridges.** Telegram/WeChat/Feishu/WeCom get three verbs: watch the chart,
  answer the inbox, read the receipt. A standing operation with no deadline
  and ambient meters *is* the always-on daemon — same object, no new product.

## 10. Communicating the value

### 10.1 In the product (UI/UX)

The product must let users **see** the speed, **verify** the quality, and
**feel** the price. Every surface answers one of those.

- **The first-run moment is the org chart filling in.** Within seconds of
  `codewhale operate`, the user watches the Operator plan the DAG, staff
  roles, and light up panes. Seeing the plan *before* the work is what makes
  the speed legible and trustworthy.
- **Meters over warnings.** Spend, burn rate, and token mix live in the
  header, always-on and glanceable — the design language of a dashboard, not
  a nag. Costs render in real route prices; unknown renders as unknown.
- **The receipt is the hero artifact.** End of operation, one card:

  ```
  Operation: migrate config system          ✅ complete
  Wall-clock 11m 42s  ·  est. serial 44m  ·  3.8× speedup
  Spend $0.61  ·  78% of tokens on open models
  Quality: 214 tests green · 2 tournaments run · rework 6%
  Chart: 1 operator · 4 workers · 1 integrator · 1 verifier
  ```

  It is copyable, shareable, and honest — including the rework line and
  including "declined to expand (task too small)" when that was the call.
  Users repeating their own receipts to each other is the growth loop.
- **One inbox.** Approvals, threshold notifications, and escalations arrive
  in a single queue with chart context attached. The user is interrupted
  once, well — never N times, badly.
- **Legible mode copy.** The Operate mode picker line becomes: *"Operate —
  state a goal; CodeWhale runs a fleet of itself under contract."* One
  sentence, repeatable, true.

### 10.2 Website copy (codewhale.net)

Lead with the outcome triangle, prove it with the receipt, explain the moat.

> **Hero:** *One goal in. A fleet of CodeWhales out.*
> Codewhale Operations plans your task as an org chart, staffs it with the
> right model for each job — frontier where it thinks, open models where it
> types — and runs it in parallel under a contract that keeps agents out of
> each other's way. You watch the chart work. You get a receipt.
>
> **Three pillars:**
> - *Faster.* Critical-path scheduling and true parallel workers. Speedup is
>   printed on every receipt, not promised in a blog post.
> - *Better.* Contract-first decomposition, an integrator that merges while
>   context is warm, and tournament verification on risky work.
> - *Cheaper.* Any model, routed by role. Most tokens land on open models at
>   open-model prices. Meters, not walls — you always see the spend, and
>   nothing kills your run at 90% done.
>
> **The moat line:** *Vendor tools sell you one brain. Codewhale runs the
> whole org — any brain, your hardware, your rules, your prices.*

Show, don't claim: the hero image is the tmux cockpit mid-operation; the
section below it is a real receipt.

### 10.3 GitHub README

The README's job is to earn the second sentence. Proposed insertion into
"What it does" (keeping the existing voice: concrete, unhyped, receipts-not-
vibes):

> - Runs Operations: `codewhale operate "<goal>"` puts an Operator at the
>   head of an org chart of independent CodeWhales — planned as a DAG,
>   staffed per-role with any provider's models, executed in parallel under
>   write leases and an integrator lane so workers never step on each other.
>   Ends with a receipt: wall-clock, speedup, spend, token mix, rework.
>   ([docs/OPERATIONS.md])

And in `codewhale --help` / the Use section:

```bash
codewhale operate "fix the flaky auth tests and refactor the retry logic"
# → plans the chart, spawns the fleet, opens the cockpit (tmux if available)
```

The same three words everywhere — **faster, better, cheaper** — always in
that order, always backed by the receipt fields that prove each one
(wall-clock/speedup · quality/rework · spend/token mix).

## 11. Phasing

Each phase ships something a user can feel; each maps to existing tracker
threads where noted.

1. **The Operation object.** Workroom-identified operation over the Fleet
   ledger: goal, objective, meters, receipt. `codewhale operate "<goal>"`.
   (Builds on #4175 product-model tracker, rfcs/3209.)
2. **The cockpit.** tmux projection + attach viewers + TUI fallback. This is
   the visibility moment; ship it early because it changes how the rest is
   perceived.
3. **The contract.** Leases enforced by hooks, contract commits, the
   Integrator role and train. (Automates the scratch-train practice;
   composes with #4177/#4179 workflow-fleet role work.)
4. **The inbox.** Aggregated, pattern-grouped approvals; operation-scoped
   grants (#1186); threshold notifications; bridge rendering.
5. **The brain.** Expansion rule in the constitution; role/model loadouts per
   objective (#3205 fleet model classes and route roles); critical-path
   scheduling; speculation; tournaments; refuse-to-expand estimation.
6. **The standing operation.** No-deadline operations with ambient meters —
   the always-on layer, controlled from the bridges, priced by local/open
   models.

Explicit prerequisite across all phases: constitution adherence and intent
fidelity (#4032, #3275) must hold before "a fleet of yourself under contract"
is a claim we can make in public copy. Trust primitives first; the org chart
is only as credible as its law.

## 12. Open questions

- **Receipt honesty spec.** "Est. serial time" needs a defensible method
  (sum of leaf-task actuals? calibrated estimate?) — an inflated speedup
  number would poison the whole story.
- **Lease granularity.** Path-based is enforceable today via hooks; symbol- or
  hunk-level would reduce false conflicts but needs tooling. Start with
  paths; measure how often coarse leases force arbitration.
- **Nested expansion depth default.** Depth is already configurable; what
  default keeps charts legible (2?) and when does a Lead get to exceed it?
- **Cross-repo operations.** The workspace-parent trick (start from a shared
  parent dir) works today; does an Operation ever need first-class
  multi-workspace leases?
- **Naming.** "Org chart" vs "chart" vs "graph" in user-facing copy — pick
  one after seeing it rendered.
