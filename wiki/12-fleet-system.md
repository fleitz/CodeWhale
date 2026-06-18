# Agent Fleet System (Control Plane)

> ⚠️ **EXPERIMENTAL — not yet wired.** The fleet types are defined and
> serializable, but the full fleet manager, worker runtime, and CLI surface
> remain behind the `whaleflow` experimental feature flag. This document
> describes the protocol-level contract and intended architecture; runtime
> behaviour may change before stabilisation. Tracked in issues
> [#3154](https://github.com/Hmbown/CodeWhale/issues/3154) (Agent Fleet
> control plane) and [#3096](https://github.com/Hmbown/CodeWhale/issues/3096)
> (Runtime API sub-agent direction). Protocol version **0.1.0**.

---

## Overview

Agent Fleet is the **local-first control plane for durable, multi-worker
runs**. A fleet worker is a headless `codewhale exec` process that the fleet
manager launches and tracks durably — it is **not** a separate execution
engine. The protocol types in `crates/protocol/src/fleet.rs` define the
serializable contract between the fleet manager, workers, CLI/TUI surfaces,
and the Runtime API.

Use Fleet rather than short-lived `agent` fanout whenever the work needs
retry, sleep/restart survival, remote execution, receipts, or a ledgered
audit trail.

Fleet state is stored under `.codewhale/fleet.jsonl`. Worker logs and
adapter logs live under `.codewhale/fleet/` and `.codewhale/fleet-host/`.

---

## Architecture Diagram

```
                        ┌──────────────────────────────────────┐
                        │            Fleet Manager             │
                        │  (CLI: `codewhale fleet ...`)        │
                        │  Runtime API: /v1/fleet/*            │
                        └────┬──────────┬──────────┬───────────┘
                             │          │          │
                   ┌─────────┐  ┌───────┐  ┌───────┐
                   │ Inbox   │  │Ledger │  │Config │
                   │(leases) │  │(.jsonl)│  │(toml) │
                   └─────────┘  └───────┘  └───────┘
                             │          │          │
              ┌──────────────┼──────────┼──────────┼──────────────┐
              │              │          │          │              │
         ┌────▼────┐   ┌────▼────┐ ┌───▼────┐ ┌───▼────┐   ┌────▼────┐
         │ Worker  │   │ Worker  │ │Worker  │ │Worker  │   │ Worker  │
         │ local-1 │   │ local-2 │ │ssh: b1 │ │ssh: b2 │   │docker:1 │
         │ (child) │   │ (child) │ │        │ │        │   │         │
         └────┬────┘   └────┬────┘ └───┬────┘ └───┬────┘   └────┬────┘
              │              │          │          │              │
              ▼              ▼          ▼          ▼              ▼
         ┌──────────────────────────────────────────────────────────┐
         │                   FleetRun                               │
         │  ┌──────────┐  ┌──────────┐  ┌──────────┐               │
         │  │TaskSpec  │  │TaskSpec  │  │TaskSpec  │  ...          │
         │  │(lint)    │  │(clippy)  │  │(audit)   │               │
         │  └────┬─────┘  └────┬─────┘  └────┬─────┘               │
         │       │             │             │                      │
         │       ▼             ▼             ▼                      │
         │  Receipt +      Receipt +     Receipt +                  │
         │  Artifacts      Artifacts     Artifacts                  │
         └──────────────────────────────────────────────────────────┘
              │              │          │          │
              ▼              ▼          ▼          ▼
         ┌──────────────────────────────────────────────────────────┐
         │              Security Policy                             │
         │  Trust Levels · Secret Refs · Capability Grants          │
         │  Auth Methods · Identity Verification                    │
         └──────────────────────────────────────────────────────────┘
```

**Key relationships:**

- A **FleetRun** owns one or more **FleetTaskSpec** entries and zero or more
  **FleetWorkerSpec** entries.
- Workers lease tasks from the **FleetInbox**; each lease is tracked as a
  sequenced **FleetWorkerEvent** stream.
- When a task completes, a **FleetReceipt** is produced with artifacts,
  scores, and a pass/fail result.
- The **FleetSecurityPolicy** (optional per-run) governs trust levels,
  allowed secrets, and capability grants for all workers in that run.
- **FleetExecConfig** (in `[fleet.exec]`) applies global hard limits on
  tool calls, turns, and spawn depth that task specs can tighten but not
  loosen.

---

## Protocol Version & Root Identifier

| Type | Kind | Source |
|------|------|--------|
| `FLEET_PROTOCOL_VERSION` | `&str` = `"0.1.0"` | `fleet.rs:18` |
| `FleetRunId` | newtype `String` (globally unique) | `fleet.rs:21-34` |

`FleetRunId` implements `From<String>`, `From<&str>`, and derives
`Serialize`/`Deserialize`/`PartialEq`/`Eq`/`Hash`.

---

## Core Run Types

### `FleetRun` — Top-level run handle

`fleet.rs:36-55`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | `FleetRunId` | ✅ | Globally unique run identifier |
| `name` | `String` | ✅ | Human-readable run name |
| `status` | `FleetRunStatus` | ✅ | Current lifecycle status |
| `task_specs` | `Vec<FleetTaskSpec>` | default `[]` | Task definitions for this run |
| `worker_specs` | `Vec<FleetWorkerSpec>` | default `[]` | Worker host/trust definitions |
| `labels` | `BTreeMap<String, String>` | default `{}` | Arbitrary key-value labels |
| `security_policy` | `Option<FleetSecurityPolicy>` | optional | Per-run security policy |
| `created_at` | `String` | ✅ | ISO-8601 creation timestamp |
| `updated_at` | `Option<String>` | optional | Last mutation timestamp |
| `completed_at` | `Option<String>` | optional | Terminal timestamp |

### `FleetRunStatus` — Lifecycle enum

`fleet.rs:57-68` — `#[serde(rename_all = "snake_case")]`

| Variant | Wire | Meaning |
|---------|------|---------|
| `Pending` | `"pending"` | Run defined but not yet queued |
| `Queued` | `"queued"` | Run enqueued, waiting for worker slots |
| `Running` | `"running"` | At least one task is actively executing |
| `Paused` | `"paused"` | Operator paused the run |
| `Completed` | `"completed"` | All tasks finished successfully |
| `Failed` | `"failed"` | One or more tasks failed terminally |
| `Cancelled` | `"cancelled"` | Operator cancelled the run |

**Lifecycle transitions:**

```
Pending ──► Queued ──► Running ──► Completed
                │          │
                │          ├──► Failed
                │          │
                │          ├──► Paused ──► Running  (resume)
                │          │
                └──────────┴──► Cancelled
```

---

## Task Specification Types

### `FleetTaskSpec` — Unit of work

`fleet.rs:70-107`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | `String` | ✅ | Task identifier within the run |
| `name` | `String` | ✅ | Short human-readable name |
| `description` | `Option<String>` | optional | Longer prose description |
| `objective` | `Option<String>` | optional | Goal statement for the worker |
| `instructions` | `String` | ✅ | Concrete instructions for the worker |
| `worker` | `Option<FleetTaskWorkerProfile>` | optional | Role/tool expectations |
| `workspace` | `Option<FleetWorkspaceRequirements>` | optional | Environment constraints |
| `input_files` | `Vec<PathBuf>` | default `[]` | Files the task should read |
| `context` | `Vec<String>` | default `[]` | Additional context strings |
| `budget` | `Option<FleetTaskBudget>` | optional | Token/tool/time limits |
| `tags` | `Vec<String>` | default `[]` | Arbitrary tags for filtering |
| `expected_artifacts` | `Vec<FleetArtifactKind>` | default `[]` | Artifacts the task should produce |
| `scorer` | `Option<FleetScorerSpec>` | optional | Verification rule |
| `retry_policy` | `Option<FleetRetryPolicy>` | optional | Retry behaviour |
| `alert_policy` | `Option<FleetAlertPolicy>` | optional | Escalation rules |
| `timeout_seconds` | `Option<u64>` | optional | Hard wall-clock timeout |
| `metadata` | `BTreeMap<String, Value>` | default `{}` | Free-form JSON metadata |

### `FleetTaskWorkerProfile` — Worker role for a task

`fleet.rs:109-122`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `role` | `Option<String>` | optional | Role name resolved from preset registry |
| `tool_profile` | `Option<String>` | optional | `"read-only"`, `"read-write"`, `"custom"` |
| `tools` | `Vec<String>` | default `[]` | Explicit tool allowlist |
| `capabilities` | `Vec<String>` | default `[]` | Required capability tags |

### `FleetWorkspaceRequirements` — Environment constraints

`fleet.rs:124-137`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `root` | `Option<PathBuf>` | optional | Workspace root (default: cwd) |
| `required_files` | `Vec<PathBuf>` | default `[]` | Files that must exist before start |
| `writable_paths` | `Vec<PathBuf>` | default `[]` | Paths the worker may write to |
| `environment` | `Option<FleetEnvironmentRequirements>` | optional | Env-var constraints |

### `FleetEnvironmentRequirements` — Env-var policy

`fleet.rs:139-148`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `required` | `Vec<String>` | default `[]` | Variables that must be set |
| `allowlist` | `Vec<String>` | default `[]` | Variables that may be forwarded |

### `FleetTaskBudget` — Resource limits

`fleet.rs:150-159`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `max_tokens` | `Option<u64>` | optional | LLM token budget ceiling |
| `max_tool_calls` | `Option<u32>` | optional | Maximum tool invocations |
| `max_seconds` | `Option<u64>` | optional | Wall-clock time budget |

---

## Artifact Types

### `FleetArtifactRef` — Produced/consumed artifact reference

`fleet.rs:161-172`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `kind` | `FleetArtifactKind` | ✅ | Category of artifact |
| `path` | `PathBuf` | ✅ | File path under `.codewhale/fleet/` |
| `checksum` | `Option<String>` | optional | Content hash (e.g. `"sha256:..."`) |
| `mime_type` | `Option<String>` | optional | MIME type hint |
| `size_bytes` | `Option<u64>` | optional | File size in bytes |

### `FleetArtifactKind` — Enum (flat-string wire format)

`fleet.rs:174-210`

| Variant | Wire String | Description |
|---------|-------------|-------------|
| `Log` | `"log"` | Bounded worker log |
| `Patch` | `"patch"` | Diff/patch file |
| `TestResult` | `"test_result"` | Test output |
| `Report` | `"report"` | Worker-generated report |
| `Checkpoint` | `"checkpoint"` | Savepoint for resume |
| `Receipt` | `"receipt"` | Signed task receipt |
| `Other(String)` | any other string | Custom artifact kind |

---

## Scorer Types

### `FleetScorerSpec` — Verification rule (tagged enum)

`fleet.rs:231-256` — `#[serde(tag = "kind")]`

| Variant | Fields | Description |
|---------|--------|-------------|
| `ExitCode` | *(none)* | Pass if worker exits 0 |
| `FileExists` | `path: PathBuf` | Pass if file exists at path |
| `RegexMatch` | `path: PathBuf`, `pattern: String` | Pass if file matches regex |
| `JsonPath` | `path: PathBuf`, `expression: String` | Pass if JSONPath expression matches |
| `Command` | `command: String`, `args: Vec<String>` | Pass if shell command exits 0 |
| `CodeWhaleVerifierPrompt` | `prompt: String` | Delegate to a verifier-model prompt |
| `Manual` | *(none)* | Always records partial; needs human |

The first four scorers are **deterministic built-ins** (`ExitCode`,
`FileExists`, `RegexMatch`, `JsonPath`). `Command`, `CodeWhaleVerifierPrompt`,
and `Manual` record a partial receipt until an explicit verifier pass completes.

---

## Worker Specification Types

### `FleetWorkerSpec` — Worker definition

`fleet.rs:258-273`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | `String` | ✅ | Worker identifier |
| `name` | `String` | ✅ | Human-readable name |
| `host` | `FleetHostSpec` | ✅ | Where the worker runs |
| `trust_level` | `Option<FleetTrustLevel>` | optional | Override trust level |
| `labels` | `BTreeMap<String, String>` | default `{}` | Key-value labels |
| `capabilities` | `Vec<String>` | default `[]` | Capability tags |
| `max_concurrent_tasks` | `Option<usize>` | optional | Concurrency limit |

### `FleetHostSpec` — Host target (tagged enum)

`fleet.rs:275-311` — `#[serde(tag = "kind")]`

| Variant | Key Fields | Description |
|---------|------------|-------------|
| `Local` | *(none)* | Child process on local machine |
| `Ssh` | `host`, `port`, `user`, `identity`, `known_hosts`, `host_key_fingerprint`, `working_directory`, `env_allowlist`, `codewhale_binary` | Remote via SSH with host-key verification |
| `Docker` | `image`, `args` | Containerised worker (aliases: `"container"`, `"Container"`) |

### `FleetWorkerStatus` — Runtime status enum

`fleet.rs:566-577` — `#[serde(rename_all = "snake_case")]`

| Variant | Wire | Meaning |
|---------|------|---------|
| `Unknown` | `"unknown"` | Status not yet determined |
| `Online` | `"online"` | Worker connected and idle |
| `Busy` | `"busy"` | Worker executing a task |
| `Offline` | `"offline"` | Worker disconnected |
| `Unhealthy` | `"unhealthy"` | Worker reporting errors |
| `Draining` | `"draining"` | Finishing current task, not accepting new |
| `Retired` | `"retired"` | Permanently removed |

### `FleetWorkerAuth` — Authentication method (tagged enum)

`fleet.rs:511-544` — `#[serde(tag = "method")]`

| Variant | Fields | Description |
|---------|--------|-------------|
| `None` | *(none)* | Local workers sharing same uid |
| `SshKey` | `identity`, `known_hosts`, `host_key_fingerprint`, `user` | SSH key-based with host-key pinning |
| `Token` | `token_ref: FleetSecretRef` | Bearer token from secret store |
| `Mtls` | `cert_path`, `key_ref: FleetSecretRef` | Mutual TLS certificate |

---

## Worker Event Stream

### `FleetWorkerEvent` — Event envelope

`fleet.rs:592-605`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `seq` | `u64` | ✅ | Monotonic sequence number |
| `run_id` | `FleetRunId` | ✅ | Owning run |
| `worker_id` | `String` | ✅ | Emitting worker |
| `task_id` | `String` | ✅ | Current task |
| `timestamp` | `String` | ✅ | ISO-8601 event time |
| `payload` | `FleetWorkerEventPayload` | ✅ (flattened) | Event body |
| `extra` | `BTreeMap<String, Value>` | default `{}` | Extension data |

### `FleetWorkerEventPayload` — Event union (tagged enum)

`fleet.rs:607-669` — `#[serde(tag = "state")]`

| Variant | Fields | Description |
|---------|--------|-------------|
| `Queued` | *(none)* | Task enqueued for this worker |
| `Leased` | `lease_expires_at` | Worker acquired lease |
| `Starting` | *(none)* | Worker process starting |
| `Running` | *(none)* | Worker executing |
| `ModelWait` | `model` | Waiting on LLM inference |
| `RunningTool` | `tool`, `call_id` | Executing a specific tool |
| `Heartbeat` | `cpu_percent`, `memory_mb` | Periodic liveness |
| `Artifact` | `FleetArtifactRef` | Artifact produced |
| `Completed` | `exit_code`, `summary` | Task finished successfully |
| `Failed` | `reason`, `recoverable` | Task failed |
| `Cancelled` | `cancelled_by` | Task cancelled |
| `Interrupted` | `signal` | OS signal received |
| `Stale` | `last_heartbeat_at` | Heartbeat timeout |
| `Restarted` | `restart_count` | Worker restarted |
| `Escalated` | `channel`, `alert_id` | Alert sent |

**Typical happy-path event sequence:**

```
Queued → Leased → Starting → Running → RunningTool* → Completed
                                              ↑
                                         Heartbeat* (periodic)
```

---

## Inbox / Queue Types

### `FleetInboxEntry` — Durable task lease record

`fleet.rs:579-590`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `run_id` | `FleetRunId` | ✅ | Owning run |
| `task_id` | `String` | ✅ | Task identifier |
| `priority` | `i32` | ✅ | Scheduling priority (higher = sooner) |
| `enqueued_at` | `String` | ✅ | ISO-8601 enqueue time |
| `lease_deadline` | `Option<String>` | optional | Lease expiry |
| `attempts` | `u32` | default `0` | Retry counter |

---

## Receipt & Scoring Types

### `FleetReceipt` — Task completion record

`fleet.rs:818-832`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `run_id` | `FleetRunId` | ✅ | Owning run |
| `task_id` | `String` | ✅ | Task identifier |
| `worker_id` | `String` | ✅ | Worker that executed the task |
| `completed_at` | `String` | ✅ | ISO-8601 completion time |
| `result` | `FleetTaskResult` | ✅ | Pass / Partial / Fail / Skip / Timeout |
| `failure_kind` | `Option<FleetTaskFailureKind>` | optional | Failure source classification |
| `artifacts` | `Vec<FleetArtifactRef>` | default `[]` | Produced artifacts |
| `score` | `Option<FleetScore>` | optional | Numeric score |

### `FleetTaskResult` — Outcome enum

`fleet.rs:834-842` — `#[serde(rename_all = "snake_case")]`

| Variant | Wire | Meaning |
|---------|------|---------|
| `Pass` | `"pass"` | Task succeeded |
| `Partial` | `"partial"` | Task finished but incomplete |
| `Fail` | `"fail"` | Task failed |
| `Skip` | `"skip"` | Task was skipped |
| `Timeout` | `"timeout"` | Task exceeded budget |

### `FleetTaskFailureKind` — Failure source

`fleet.rs:844-851` — `#[serde(rename_all = "snake_case")]`

| Variant | Wire | Meaning |
|---------|------|---------|
| `Transport` | `"transport"` | Network/SSH/connection failure |
| `Task` | `"task"` | Worker reported a domain error |
| `Verifier` | `"verifier"` | Scorer/verifier disagreed or failed |

### `FleetScore` — Numeric result

`fleet.rs:853-860`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `value` | `f64` | ✅ | Actual score |
| `max` | `Option<f64>` | optional | Maximum possible score |
| `notes` | `Option<String>` | optional | Human-readable notes |

---

## Retry & Alert Types

### `FleetRetryPolicy` — Retry behaviour

`fleet.rs:671-709`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_attempts` | `u32` | `3` | Maximum total attempts |
| `initial_backoff_seconds` | `u64` | `5` | First backoff delay |
| `max_backoff_seconds` | `u64` | `300` | Backoff cap (5 minutes) |
| `backoff_multiplier` | `u32` | `2` | Exponential factor |

Implements `Default` with the values above. Missing fields in JSON deserialize
to their defaults (non-zero), so an empty `{}` is a valid retry policy.

### `FleetAlertPolicy` — Escalation rules

`fleet.rs:711-723`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `events` | `Vec<FleetAlertEventClass>` | default `[]` | Event classes that trigger alerts |
| `channels` | `Vec<FleetAlertChannel>` | default `[]` | Where to send alerts |
| `after_attempts` | `Option<u32>` | optional | Only alert after N retries |
| `after_minutes_stale` | `Option<u64>` | optional | Only alert after staleness threshold |

### `FleetAlertEventClass` — Trigger classes

`fleet.rs:725-734` — `#[serde(rename_all = "snake_case")]`

| Variant | Wire | Trigger |
|---------|------|---------|
| `Stale` | `"stale"` | Worker heartbeat timeout |
| `RestartExhausted` | `"restart_exhausted"` | Retry budget exhausted |
| `NeedsHuman` | `"needs_human"` | Decision required |
| `BudgetExceeded` | `"budget_exceeded"` | Token/tool/time budget hit |
| `VerifierFailed` | `"verifier_failed"` | Scorer disagreed with receipt |
| `RunCompleted` | `"run_completed"` | All tasks finished |

### `FleetAlertChannel` — Delivery target (tagged enum)

`fleet.rs:736-754` — `#[serde(tag = "kind")]`

| Variant | Fields | Description |
|---------|--------|-------------|
| `Slack` | `webhook: FleetAlertEndpoint` | Slack incoming webhook |
| `Webhook` | `endpoint: FleetAlertEndpoint` | Generic HTTP webhook |
| `PagerDuty` | `routing_key: String`, `severity: String` | PagerDuty integration (aliases: `"pager_duty"`, `"pagerduty"`) |

### `FleetAlertEndpoint` — Webhook URL (inline or secret-backed)

`fleet.rs:756-816`

| Field | Type | Required | Aliases | Description |
|-------|------|----------|---------|-------------|
| `url` | `Option<String>` | optional | `webhook_url`, `endpoint_url` | Inline URL (non-sensitive only) |
| `url_ref` | `Option<FleetSecretRef>` | optional | `webhook_url_ref`, `webhook_ref`, `url_secret_ref` | Secret-backed URL |
| `secret_ref` | `Option<FleetSecretRef>` | optional | `secret`, `webhook_secret`, `signing_secret` | HMAC signing secret ref |

---

## Security Model

### Trust Levels

`fleet.rs:315-359`

`FleetTrustLevel` is a `Copy` ordinal enum with discriminant values that
reflect increasing privilege:

| Level | Discriminant | Secrets | Network | Workspace Writes | Requires |
|-------|:-----------:|:-------:|:-------:|:----------------:|----------|
| `Sandbox` | 0 | ❌ | ❌ | `.codewhale/fleet/` only | Nothing (default) |
| `Local` | 1 | ✅ | ✅ | ✅ (gated) | Local process, same uid |
| `RemoteVerified` | 2 | ✅ | ✅ | ❌ | SSH host-key verification |
| `Operator` | 3 | ✅ | ✅ | ✅ | Operator-owned machine |

**Ordinal invariant:** `Operator > RemoteVerified > Local > Sandbox`.

**Helper methods** (on `FleetTrustLevel`):
- `may_access_secrets() -> bool` — `Operator | RemoteVerified | Local`
- `may_write_workspace() -> bool` — `Operator | Local`
- `may_access_network() -> bool` — `Operator | RemoteVerified | Local`

### `FleetSecurityPolicy` — Per-run security policy

`fleet.rs:361-412`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `default_trust_level` | `FleetTrustLevel` | `Sandbox` | Trust for workers without explicit level |
| `allowed_secrets` | `Vec<FleetSecretRef>` | `[]` | Secrets workers may resolve (empty = none) |
| `capability_grants` | `Vec<FleetCapabilityGrant>` | `[]` | Additive capability grants |
| `max_trust_level` | `FleetTrustLevel` | `Operator` | Ceiling (workers requesting higher are clamped) |
| `require_identity_verification` | `bool` | `false` | SSH workers must pass host-key check |
| `allow_parallel_reads` | `bool` | `false` | Batch independent read-only tools in concurrent turns |

The `Default` impl is intentionally conservative: `Sandbox` trust, no secrets,
no grants, no identity verification required.

### `FleetSecretRef` — Secret reference (never plaintext)

`fleet.rs:414-509`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `key` | `String` | ✅ | Secret key name (e.g. `"GH_TOKEN"`) |
| `source` | `Option<String>` | optional | Resolution hint: `"env"`, `"keyring"`, `"file"`, or absent (try all) |

Secret refs support two wire shapes:
- **Plain string:** `"CODEWHALE_API_KEY"` (desugars to `{key, source: None}`)
- **Structured:** `{"key": "GH_TOKEN", "source": "env"}`

`Display` and `redacted()` always show the redacted form: `<secret:env.GH_TOKEN>`.

### `FleetCapabilityGrant` — Additive permission

`fleet.rs:546-564`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `capability` | `String` | ✅ | Capability name (`"network"`, `"git-push"`, `"provider-secrets"`, `"release"`, `"workspace-write"`) |
| `scope` | `Option<String>` | optional | Bounding scope (e.g. `"github.com"`, `"crates/tui/**"`) |
| `reason` | `Option<String>` | optional | Audit justification |

---

## Configuration Types

### `FleetConfigToml` — On-disk `[fleet]` table

`crates/config/src/lib.rs:1295-1322`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `default_trust_level` | `String` | `"sandbox"` | One of `"sandbox"`, `"local"`, `"remote-verified"`, `"operator"` |
| `require_identity_verification` | `bool` | `true` | Require SSH host-key verification |
| `max_trust_level` | `String` | `"operator"` | Ceiling trust level |
| `roles` | `BTreeMap<String, FleetRolePreset>` | `{}` | User-defined role presets |
| `exec` | `FleetExecConfig` | *(see below)* | Headless worker execution hardening |

### `FleetRolePreset` — Named role preset

`crates/config/src/lib.rs:1411-1431`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `description` | `Option<String>` | optional | Human description |
| `tool_profile` | `Option<String>` | optional | `"read-only"`, `"read-write"`, `"custom"` |
| `tools` | `Vec<String>` | default `[]` | Default tool names |
| `capabilities` | `Vec<String>` | default `[]` | Default capability tags |
| `timeout_seconds` | `Option<u64>` | optional | Default timeout |
| `trust_level` | `Option<String>` | optional | Trust level override |

**Built-in roles** (always available without config):

| Role | Tool Profile | Timeout | Trust |
|------|-------------|:-------:|-------|
| `smoke-runner` | `read-only` | 300s | `local` |
| `reviewer` | `read-only` | 600s | *(inherit)* |
| `builder` | `read-write` | 1800s | `local` |
| `read-only` | `read-only` | *(none)* | *(inherit)* |

User-defined roles in `[fleet.roles]` override built-in defaults with the
same name. Resolution: user roles first, then built-in fallback.

### `FleetExecConfig` — Headless worker execution constraints

`crates/config/src/lib.rs:1345-1376` — applies to all fleet workers and sub-agents

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `allowed_tools` | `Vec<String>` | `[]` | Always-allowed tools (regardless of role) |
| `disallowed_tools` | `Vec<String>` | `[]` | Always-forbidden tools (overrides role) |
| `max_turns` | `u32` | `u32::MAX` | Hard ceiling on tool calls + model turns |
| `max_spawn_depth` | `u32` | `3` (`DEFAULT_SPAWN_DEPTH`) | Recursive child-agent budget; clamped to `MAX_SPAWN_DEPTH_CEILING` (3) |
| `append_system_prompt` | `String` | `""` | Extra system prompt injected into every worker |
| `output_format` | `String` | `"text"` | `"text"` or `"stream-json"` |

**Key design invariant:** Fleet workers and standalone sub-agents share **one**
recursion axis. `DEFAULT_SPAWN_DEPTH = 3` is the single source of truth;
`MAX_SPAWN_DEPTH_CEILING = 3` is the hard safety cap. Setting
`max_spawn_depth = 0` blocks child `agent` calls but the root worker still
runs. Task specs can **tighten** these limits but not loosen them.

---

## Experimental Feature Flag

In the TUI config view, fleet is surfaced under the `whaleflow` experimental
feature key (`crates/tui/src/tui/views/mod.rs:1203`):

```
section: ConfigSection::Experimental
key: "whaleflow"
value: "preview overlay for workflow/fleet runs (not stable; see #3154/#3178)"
```

Both WhaleFlow and Fleet are gated behind the same experimental flag until
the full durable-worker substrate stabilises.

---

## Source Index

| What | File | Lines |
|------|------|-------|
| All protocol types + enums | `crates/protocol/src/fleet.rs` | 1–1270 |
| Protocol version constant | `crates/protocol/src/fleet.rs` | 18 |
| `FleetRun` + `FleetRunStatus` | `crates/protocol/src/fleet.rs` | 36–68 |
| `FleetTaskSpec` + sub-types | `crates/protocol/src/fleet.rs` | 70–159 |
| `FleetArtifactRef` + `FleetArtifactKind` | `crates/protocol/src/fleet.rs` | 161–229 |
| `FleetScorerSpec` | `crates/protocol/src/fleet.rs` | 231–256 |
| `FleetWorkerSpec` + `FleetHostSpec` | `crates/protocol/src/fleet.rs` | 258–311 |
| `FleetTrustLevel` + security model | `crates/protocol/src/fleet.rs` | 313–412 |
| `FleetSecretRef` | `crates/protocol/src/fleet.rs` | 414–509 |
| `FleetWorkerAuth` | `crates/protocol/src/fleet.rs` | 511–544 |
| `FleetCapabilityGrant` | `crates/protocol/src/fleet.rs` | 546–564 |
| `FleetWorkerStatus` | `crates/protocol/src/fleet.rs` | 566–577 |
| `FleetInboxEntry` | `crates/protocol/src/fleet.rs` | 579–590 |
| `FleetWorkerEvent` + payloads | `crates/protocol/src/fleet.rs` | 592–669 |
| `FleetRetryPolicy` | `crates/protocol/src/fleet.rs` | 671–709 |
| `FleetAlertPolicy` + alert types | `crates/protocol/src/fleet.rs` | 711–816 |
| `FleetReceipt` + result/scoring | `crates/protocol/src/fleet.rs` | 818–860 |
| `FleetConfigToml` + `FleetRolePreset` | `crates/config/src/lib.rs` | 1295–1467 |
| Built-in role presets | `crates/config/src/lib.rs` | 1469–1530 |
| `FleetExecConfig` + spawn-depth constants | `crates/config/src/lib.rs` | 1324–1401 |
| TUI experimental feature flag | `crates/tui/src/tui/views/mod.rs` | 1195–1207 |
| Existing user-facing docs | `docs/FLEET.md` | 1–534 |
