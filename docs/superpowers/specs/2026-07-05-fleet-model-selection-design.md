# Fleet model selection: operator + concrete models

**Date:** 2026-07-05
**Status:** Approved (design), implementation on `work/fleet-model-selection`
**Scope:** 0.8.68 follow-on. Does NOT touch the frozen 0.8.67 release candidate
(`work/v0.9.0-cutover` @ `8eb6715a9`) or the fallback lane.

## Problem

Every Fleet member runs on the user's currently-loaded model (e.g. `glm-5.2`)
regardless of the "model class" (loadout) chosen. The abstract classes
(`Fast / Balanced / Strong / DeepReasoning / Code / Review / ToolHeavy`) do not
select a concrete model — they collapse to the active/session model.

### Root cause (two seams, both degenerate to the active model)

1. `crates/tui/src/fleet/worker_runtime.rs:399` `effective_fleet_model_with_source`
   returns a slot's concrete `model` when set (`task.model` → `agent_profile.model`),
   else falls back to `run.model` (the active model). Slots today carry only a
   *class*, no concrete `model`, so all slots fall through to `run.model`.
2. `crates/tui/src/fleet/worker_runtime.rs:532` `fleet_model_route_for_loadout`
   maps `Fast → Faster` and every richer class → `ModelRoute::Auto`; because the
   effective model is already the active model string, it returns
   `Fixed(active-model)` before the loadout arm is even reached.
3. `crates/tui/src/tools/subagent/mod.rs:5975` `spawn_model_route` is a *second*
   resolver with the same defect: non-`Fast` → `Inherit`; the `Faster` path does
   `candidates.cheap … unwrap_or(active model)`. With one provider that is the
   active model again.

`ModelRoute` (`crates/tui/src/worker_profile.rs:114`) is abstract
(`Inherit / Faster / Auto / Fixed(id)`) — a class physically cannot point at a
"stronger" model. The only concrete selection primitive is `Fixed(id)`, and it
already works when a slot has a `model` set.

### History (why classes are inert)

The 0.9.0 plan intended classes to be provider-agnostic *hints* resolved to a
real model by a `RouteResolver` (issues #3205 / #2300). That auto-mapping was
**deferred and never built**; only the concurrency/limits half of
"provider-agnostic" shipped. The per-slot concrete `model` override was built and
works. So classes are a promise the resolver never kept.

## Decision

Retire the abstract **model classes** entirely (not roles — roles stay). Make
model selection concrete and operator-centric:

- **Operator slot** = a role whose model is `Inherit` — the live configured
  session model. Load `glm-5.2` → the operator *is* `glm-5.2`, dynamically.
- **Every other slot** carries a **concrete model id** (`ModelRoute::Fixed`) the
  user picks from available providers/models.
- **A non-operator slot with no model** visibly shows "↳ inherits operator
  (`glm-5.2`)" — same behavior as today but explicit, never silent.
- **`FleetLoadout` is retired** from the user surface and routing. Legacy configs
  still deserialize; for routing a legacy `loadout` maps to `Inherit` plus a
  one-time migration hint.

This leans on the concrete-model path that already works, deletes the leaky
abstraction, and needs less code than finishing the deferred `RouteResolver`
auto-router. It respects the plan's "don't hardcode a model roster" guardrail
because *the user* picks the models, not us.

## Config scope: global default + project override

The roster loader already merges built-in members + global `[fleet.profiles]` +
workspace `.codewhale/agents/*.toml`. Formalize precedence and flow `model`
through every layer:

- **Built-in** (base): default roster (operator + starter slots); operator =
  `Inherit`; other slots default to "inherits operator" until assigned a model.
- **Global** (`~/.codewhale/…`): the user's standing fleet, applies to all
  sessions. Overrides/extends built-ins **per slot id**.
- **Project** (`.codewhale/…`, wins when present): per-repo overrides, merged
  **per slot id** — a project overrides only the slots it names; others fall
  through to global/built-in.
- Precedence: **project > global > built-in**.
- `/fleet roster` shows each slot's resolved model + **provenance**
  (built-in / global / project).
- The wizard asks **"save to this project or globally?"** when persisting.

## Architecture / components

- **One resolver.** Collapse the two seams into a single slot→`ModelRoute`
  function with one rule: explicit slot `model` → `Fixed(id)`; else → `Inherit`.
  Delete all `FleetLoadout` match-arms. `spawn_model_route` and
  `fleet_model_route_for_loadout` both call it (or the latter is removed and the
  former is the single path). `Faster`/`Auto` remain in the enum only for
  back-compat deserialization; slot resolution never emits them.
- **Config schema.** Keep `FleetProfile.model` / `worker.model` as the concrete
  pin. Deserialize `loadout` / `model_class` / `model_class_hint` for
  back-compat but treat them as non-routing (migration note only).
- **Layered load.** Merge built-in/global/project rosters by slot id with the
  precedence above; record provenance per slot.
- **UI.** `/fleet setup`: replace the "Model Class" step with a concrete model
  picker (reuse the provider/offerings list used by the provider picker);
  operator shows "= active model"; save-scope prompt. `/fleet roster`: show
  resolved model + provenance; drop "loadout <class>" labels.

## Data flow

`session active model` + `slot config (model?, provenance)`
→ merged roster (project > global > built-in, per slot)
→ resolver: `model` set → `Fixed(model)`, else `Inherit`
→ `effective_fleet_model` (already: task.model → profile.model → run.model)
→ worker runs the resolved concrete model; roster/`/fleet roster` display it.

## Error handling / edge cases

- Concrete model validated against the active provider at spawn (existing check).
  Unknown/unavailable model → surface a clear error, do not silently fall back.
- Legacy `loadout` present → route as `Inherit`, emit one-time migration hint.
- Non-operator slot with no model → `Inherit`, labeled as inheriting operator.
- Empty/`"auto"` model string → treated as unset (Inherit), matching current
  `non_empty_trimmed` / `"auto"` handling.

## Testing (test-first; the headline test is the acceptance bar)

1. **Distinctness (the bar):** operator + two slots pinned to two *different*
   models resolve to **three distinct models**, not all-active.
2. Unset non-operator slot resolves to the operator/active model AND is labeled
   as inherited.
3. Legacy `loadout = "strong"` config loads, routes to `Inherit`, emits the
   migration hint (asserted once).
4. Layering: a project override changes only its named slot; other slots fall
   through to global/built-in (per-slot merge).
5. Both former seams (`spawn_model_route`, fleet worker runtime) go through the
   single resolver and agree.

**Real-harness verification:** build a fleet with the operator model + a second
concrete model on another slot, spawn headless, confirm each worker runs its
assigned model (not the active model across the board).

## Out of scope / YAGNI

- No class→concrete auto-router (#3205/#2300) — deleted, not finished.
- No new `ModelRoute` variants (no "Stronger"); concrete ids only.
- Roles (Explore/Review/Implementer/Verifier/…) unchanged.
- No changes to the 0.8.67 release candidate or fallback lane.
