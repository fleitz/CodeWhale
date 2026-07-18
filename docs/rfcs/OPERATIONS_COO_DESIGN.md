# Operations: the Operator as COO

Status: draft for discussion (not scheduled)
Scope: product and architecture direction for Operate mode, Fleet, and Workflow
Audience: maintainers, contributors, and anyone who writes user-facing copy

## Summary

CodeWhale already spawns real, independent CodeWhales. A Fleet worker is a
full headless `codewhale exec` run, tracked in a durable ledger with leases,
heartbeats, and resume. We built the engine and forgot to build the meaning.
Users cannot see the workers, cannot say what Operate mode is, and cannot
trust parallel agents not to trample one another's work.

This document proposes the missing meaning. Every CodeWhale runs one loop:
reason, then either act or expand. To expand is to hand a piece of the work
to a new, fully independent CodeWhale. The running system forms an org
chart, and at its head sits the Operator — a chief operating officer that
plans the work as a graph, staffs each node, arbitrates access to the
repository, integrates the results, and reports to the one person above it:
the user. A single contract, enforced by machinery we already have, keeps
the workers out of one another's way.

The promise to the user is plain: tasks finish faster, at higher quality,
for less money. We do not ask anyone to believe this. Every operation ends
with a receipt that proves it or admits it didn't.

## Vocabulary

One noun per concept, used identically in the TUI, the docs, the website,
and the README. An **Operator** is the CodeWhale at the head of the chart.
An **Operation** is a goal handed to the Operator, together with an
objective and live meters; it is the unit a user starts, watches, attaches
to, and receives a receipt for. The **org chart** is the live graph of
CodeWhales in an operation. To **expand** is to spawn a peer CodeWhale that
owns a piece of the work end to end. A **lease** is an enforced,
ledger-recorded grant of write access to a slice of the repository. The
**integrator** is the standing role that merges worker branches; workers
never merge. The **receipt** is the evidence an operation leaves behind:
wall-clock time, speedup, spend, quality checks, and rework.

Operate, the TUI mode, stops being a posture — today MODES.md says it
"changes scheduling emphasis, not authority" — and becomes the mode in
which you run Operations.

## The problem

Parallelism is our users' loudest pain. They cannot fan work out
efficiently. The choice between worktrees and branches is unresolved
ceremony, agents collide in the same files, and merge pain arrives late,
after the workers that caused it have lost their context. Operate mode is
illegible: it changes emphasis rather than meaning, so users cannot say
what it is and therefore cannot want it. Fleet is invisible: durable
multi-worker runs render as shell status lines, and nobody notices
parallelism they cannot see.

Two subtler problems compound these. First, subagent fan-out serializes
through the parent: every result that returns through a parent transcript
burns parent tokens and forces a sequencing point, so the parent's context
window becomes a serial bus. Second, the human is the hidden bottleneck.
Under the Ask posture, six workers means six scattered approval prompts,
and the wall-clock gains evaporate at the keyboard.

A final constraint shapes the design: hard budgets feel like handcuffs.
Users want to see and steer their spending, not watch a ceiling kill an
operation at ninety percent done.

## What exists today

The design builds on machinery that already ships. Fleet provides durable
workers — each a full `codewhale exec` run — with a ledger, leases,
heartbeats, and `fleet resume`. Fleet profiles define reusable roles with
per-role model, provider, and thinking settings. Workflow supplies ordered
phases, gates, and deterministic fan-in. Hooks and execpolicy can allow,
deny, or question any tool call, which makes them an enforcement layer.
The constitution imposes repository law that even Full Access cannot skip.
Fin, the fast `deepseek-v4-flash` seam, gives the Operator near-free
triage and summarization. Route configuration knows real prices. The
Workroom draft (rfcs/3209) specifies durable, addressable threads of work
with an event log. The chat bridges and the Termux build allow remote
attachment. Sub-agent depth is already configurable, which bounds
recursion.

Nothing here requires a new execution engine. It requires a model, a
contract, and a cockpit.

## The loop: reason, act, expand

Classic ReAct alternates reasoning with acting. CodeWhale's loop offers a
third choice at every step. The agent reasons about the state of the goal,
then either acts inline — tools, edits, shell — or expands: it carves off
a self-contained piece of the work and spawns a peer CodeWhale to own it.
It then observes results, from its own tools or from ledger events its
peers emit, and the loop continues.

Expansion is not a subagent call. An expanded CodeWhale is a peer process
with its own context, its own loop, its own worktree, and its own lease.
Within the configured depth bound it may expand in turn. It communicates
through artifacts — commits, ledger events, receipts — and never through
the parent's transcript. The parent reads state transitions and summaries,
not conversations. This severs the serial bus: supervision becomes nearly
free, and the parent's context window stops taxing every result.

One rule governs expansion, and it belongs in the constitution so that
every CodeWhale in the chart obeys the same law: **expand when the work is
independent of your current context and its expected volume exceeds the
cost of handing it off; otherwise act inline.** Work is independent when a
self-contained brief — goal, lease, interfaces, done-criteria — specifies
it without shipping the transcript along. The handoff cost is real:
worktree setup, context bootstrap, integration. A three-minute task
therefore always runs inline. The Operator estimates before it fans out
and declines when the graph is too small, because nothing destroys trust
in "it's faster" like an operation that is slower than doing the work
directly. Declining to expand is a success, and the receipt records it as
one.

## The org chart

The Operator is a chief operating officer, and its job description reads
in priority order. It plans the graph: it decomposes the goal into a task
DAG, and its first artifact is the contract commit described below. It
staffs the graph: it assigns roles from Fleet profiles and picks each
role's model by the operation's objective — a frontier model where
judgment concentrates, in planning and review; open models where volume
concentrates, in implementation and tests. It arbitrates: it owns the
lease map, grants and revokes access as the graph evolves, and fields
escalations. It integrates, by running or delegating the integrator lane.
And it reports: the live chart, the approval inbox, the final receipt. The
Operator does as little direct work as possible. A COO who is busy typing
is a planning failure.

The Operator staffs from a small set of standing roles. Leads own a
subgraph and may expand further. Workers own a leaf task end to end. The
Integrator runs the merge train. The Verifier performs independent checks
and judges tournaments. Each role is a Fleet profile; staffing needs no
new machinery.

## The Operation: meters, not ceilings

An Operation consists of a goal, one leading objective — minimize time,
minimize cost, or maximize quality — always-on meters, optional
notification thresholds, the live chart, and the final receipt.

It deliberately lacks a hard budget. A cap kills an operation at ninety
percent done and teaches users to over-provision. Instead the meters stay
visible: live spend, burn rate, and token mix in the cockpit header, per
worker and in total, priced from real routes — an unknown price displays
as unknown, never as zero. Thresholds notify rather than terminate:
`--notify-at $5` sends the chart state and a projection to the approval
inbox and the bridges, and the user chooses to continue, narrow, or wind
down. The objective shapes spending without capping it: minimizing cost
staffs cheaper loadouts, disables speculation, and prefers inline work;
minimizing time widens fan-out and enables speculation; maximizing quality
buys tournaments and verification. For CI and unattended runs, where a
wall is genuinely the right tool, `--hard-cap` remains available. It is
never the default.

## The concurrency contract

The contract is settled once, at the system level, in the constitution and
the hooks. Individual agents never renegotiate it.

**Write leases.** The Operator partitions the repository — by crate,
directory, or file set — and records leases in the ledger. Hooks enforce
them: a worker that writes outside its lease is denied at the tool layer
and must request the lease from the Operator. A conflict thus stops being
a merge-time surprise and becomes a cheap, explicit, logged arbitration.
Fleet's ledger already reconciles task leases against heartbeats and
worker death; this extends the same idea to file ownership.

**The contract commit.** Most collisions are not overlapping edits but
implicit disagreements about seams. The Operator's first artifact is
therefore a commit on the operation's base branch that fixes the seams:
module boundaries, type signatures, stubs. Workers then code against
agreed interfaces in isolated worktrees. This costs nothing to build — it
is pure convention — and it is the single highest-leverage quality move in
parallel work.

**The integrator lane.** No one chooses between worktrees and branches
again, because every operation does the same thing: a worktree per worker,
a branch per task, integration by lane. A standing Integrator merges
landed branches into a train continuously, surfaces conflicts while both
workers still hold context — cheap to fix now, expensive after they are
gone — runs the targeted test gate, and fast-forwards the real branch only
when green. Workers never merge, and humans never resolve agent-against-
agent conflicts. This automates a practice the repository already trusts:
the scratch PR trains of our own stewardship docs, which absorb candidate
work to expose conflicts early and then harvest the clean slices. The
pattern is proven here; the design gives it a role and a loop.

**The honesty metric.** Every receipt reports the rework ratio: tokens
spent on conflict resolution, re-implementation, and integration fixes, as
a share of the total. A chart that fans out too wide or leases too loosely
shows up as rework, publicly, on its own receipt. This number keeps the
expansion rule honest.

## Scheduling

Speed, quality, and price improve through four policies. The Operator
schedules the critical path first: it starts the longest chains in the
DAG, not the easiest tasks, because the longest chain sets the wall-clock
no matter how many easy wins land early. Under a time objective with a
local or open model in the loadout, it speculates: it starts probable-next
tasks before they are confirmed, since wasted speculation on a local model
costs almost nothing and saved wall-clock is real. For risky work under a
quality objective, it runs tournaments: two open-model workers race on the
same task and a Verifier picks the winner — quality rises while the total
cost stays below one frontier-model attempt. And it right-sizes: small
work never expands.

One further mechanism decides whether parallelism feels fast in practice.
Under the Ask posture, all asks from all workers flow into a single
**approval inbox** on the Operator's surface, deduplicated and grouped by
pattern: "four workers want to run `cargo test` — approve the pattern for
this operation?" A grant becomes an operation-scoped rule; the typed
persistent permission rules of issue #1186 slot exactly here. The inbox
also renders on the chat bridges, so an operation running at home can be
supervised from a phone. The user is interrupted once, well — never six
times, badly.

## The cockpit

Orchestration lives in the ledger; every surface is a projection of it.
Nothing scrapes a terminal, and nothing depends on any particular
renderer.

`codewhale operate "<goal>"` creates or attaches a tmux session named for
the operation: an Operator window showing the chart, the meters, and the
inbox, and one pane per worker running an attach viewer against that
worker's event stream. The panes are views. Kill one and the worker keeps
running; reattach from any terminal, including over SSH from a phone
running Termux. We choose tmux because it is the terminal-native way to
see parallelism — and because a grid of CodeWhales visibly working in
parallel is also the screenshot that travels. Where tmux is absent, as on
Windows, the same projection renders inside the TUI as the existing
workroom overlay: the same information in different geometry.

Each operation is a workroom in the sense of rfcs/3209: a stable id, a
`codewhale://workroom/...` link, an event log with agent attribution. That
id is what the TUI, tmux, the bridges, and the runtime API all attach to.
The bridges need only three verbs: watch the chart, answer the inbox, read
the receipt. And a standing operation — no deadline, ambient meters,
priced by local models — is the always-on daemon we have discussed
elsewhere. It is the same object with no end date, not a new product.

## Communicating the value

### In the product

The interface must let users see the speed, verify the quality, and feel
the price; every surface answers one of the three.

The first-run moment is the org chart filling in. Within seconds of
`codewhale operate`, the user watches the Operator draw the DAG, staff the
roles, and light the panes. Showing the plan before the work is what makes
the speed legible and the delegation trustworthy. While the operation
runs, meters — not warnings — occupy the header: spend, burn rate, and
token mix, glanceable, in the design language of a dashboard rather than a
nag.

The receipt is the hero artifact. When the operation ends, one card
summarizes it:

```
Operation: migrate config system          ✅ complete
Wall-clock 11m 42s  ·  est. serial 44m  ·  3.8× speedup
Spend $0.61  ·  78% of tokens on open models
Quality: 214 tests green · 2 tournaments run · rework 6%
Chart: 1 operator · 4 workers · 1 integrator · 1 verifier
```

The receipt is copyable, shareable, and honest. It includes the rework
line, and when the Operator declined to expand because the task was too
small, it says so. Users quoting their own receipts to one another is the
growth loop.

The mode picker earns one sentence of copy: "Operate — state a goal;
CodeWhale runs a fleet of itself under contract." Repeatable, and true.

### On the website

The site leads with the outcome, proves it with a receipt, and then names
the moat. A hero along these lines: "One goal in. A fleet of CodeWhales
out. Codewhale plans your task as an org chart, staffs each role with the
right model for the job — frontier where it thinks, open models where it
types — and runs it in parallel under a contract that keeps agents out of
each other's way. You watch the chart work. You get a receipt." Beneath
it, three short proofs: faster, because critical-path scheduling and true
parallel workers put the speedup on every receipt rather than in a blog
post; better, because contract-first decomposition, warm-context
integration, and tournament verification raise quality while you watch;
cheaper, because any model can fill any role, most tokens land on open
models at open-model prices, and meters — not walls — mean nothing kills
your run at ninety percent done. The closing line states the moat: vendor
tools sell you one brain; Codewhale runs the whole org — any brain, your
hardware, your rules, your prices. The hero image is the cockpit
mid-operation. The section below it is a real receipt, not a mockup.

### In the README

The README's voice is concrete and unhyped; the new entry in "What it
does" should match it:

> Runs Operations: `codewhale operate "<goal>"` puts an Operator at the
> head of an org chart of independent CodeWhales — planned as a DAG,
> staffed per role from any provider, executed in parallel under write
> leases and an integrator lane so workers never step on each other. Ends
> with a receipt: wall-clock, speedup, spend, token mix, rework.

Everywhere we make the claim, the same three words appear in the same
order — faster, better, cheaper — and each points at the receipt field
that proves it: wall-clock and speedup; quality and rework; spend and
token mix.

## Phasing

Each phase ships something a user can feel. First, the Operation object: a
workroom-identified operation over the existing Fleet ledger, carrying
goal, objective, meters, and receipt, started by `codewhale operate`
(builds on the #4175 product-model tracker and rfcs/3209). Second, the
cockpit: the tmux projection, the attach viewers, and the TUI fallback —
early, because visibility changes how everything after it is perceived.
Third, the contract: leases enforced by hooks, contract commits, and the
Integrator with its train, composing with the workflow-role work in #4177
and #4179. Fourth, the inbox: aggregated, pattern-grouped approvals with
operation-scoped grants (#1186), threshold notifications, and bridge
rendering. Fifth, the brain: the expansion rule in the constitution,
objective-driven loadouts (#3205), critical-path scheduling, speculation,
tournaments, and the refusal to expand small work. Sixth, the standing
operation: no deadline, ambient meters, local-model pricing, controlled
from the bridges.

One prerequisite spans all six. Constitution adherence and intent
fidelity (#4032, #3275) must hold before "a fleet of yourself under
contract" appears in public copy. The org chart is only as credible as its
law.

## Open questions

The receipt's "estimated serial time" needs a defensible method — a sum of
leaf-task actuals, or a calibrated estimate — because an inflated speedup
number would poison the whole story. Lease granularity starts at the path
level, which hooks can enforce today; symbol- or hunk-level leases would
reduce false conflicts but need tooling, so we should first measure how
often coarse leases force arbitration. The default depth for nested
expansion is open — depth is already configurable, and a default of two
keeps charts legible, but Leads may need to exceed it. Cross-repository
operations work today through the shared-parent-workspace trick; whether
an Operation ever needs first-class multi-workspace leases is unproven.
And the copy must settle on one word — org chart, chart, or graph — after
we have seen it rendered.
