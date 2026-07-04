//! Optional advisor watcher state and model-visible configuration tool.
//!
//! This first slice deliberately does not add a second consumer to the UI's
//! event channel. Instead, the engine records bounded tool/turn facts into a
//! small workspace-scoped watcher and emits concise advisory status notes at
//! turn boundaries.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, required_str,
};
use crate::core::events::TurnOutcomeStatus;

const DEFAULT_MAX_EVENTS: usize = 12;
const DEFAULT_MAX_INPUT_BYTES: usize = 512;
const DEFAULT_MIN_INTERVAL_MS: u64 = 30_000;
const MAX_ROSTER: usize = 8;

static ADVISOR_STATE: OnceLock<Mutex<HashMap<String, WorkspaceAdvisorState>>> = OnceLock::new();

pub struct AdvisorTool;

#[async_trait]
impl ToolSpec for AdvisorTool {
    fn name(&self) -> &'static str {
        "advisor"
    }

    fn description(&self) -> &'static str {
        "Configure optional advisor watchers for the current workspace/session. Advisors are off by default; when enabled they observe bounded tool summaries and emit concise, rate-limited notes at turn boundaries without gaining extra tool access."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["enable", "disable", "status"],
                    "description": "Advisor configuration operation."
                },
                "roster": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Watcher labels for this session/workspace, e.g. reviewer, verifier, safety."
                },
                "max_events": {
                    "type": "integer",
                    "description": "Maximum recent tool events included in a turn summary. Defaults to 12."
                },
                "max_input_bytes": {
                    "type": "integer",
                    "description": "Maximum serialized input bytes kept per tool-start event. Defaults to 512."
                },
                "min_interval_ms": {
                    "type": "integer",
                    "description": "Minimum interval between advisory notes. Defaults to 30000."
                }
            },
            "required": ["operation"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Sandboxable]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let operation = required_str(&input, "operation")?;
        let value = match operation {
            "enable" => {
                let config = AdvisorConfig {
                    enabled: true,
                    roster: roster_from_input(&input)?,
                    max_events: usize_from_input(&input, "max_events", DEFAULT_MAX_EVENTS)?,
                    max_input_bytes: usize_from_input(
                        &input,
                        "max_input_bytes",
                        DEFAULT_MAX_INPUT_BYTES,
                    )?,
                    min_interval_ms: u64_from_input(
                        &input,
                        "min_interval_ms",
                        DEFAULT_MIN_INTERVAL_MS,
                    )?,
                };
                enable_for_workspace(&context.workspace, config)
            }
            "disable" => disable_for_workspace(&context.workspace),
            "status" => status_for_workspace(&context.workspace),
            other => {
                return Err(ToolError::invalid_input(format!(
                    "unknown advisor operation `{other}`"
                )));
            }
        };
        ToolResult::json(&value).map_err(|err| ToolError::execution_failed(err.to_string()))
    }
}

pub fn start_turn(workspace: &Path, turn_id: &str) {
    with_state_mut(workspace, |state| {
        if !state.config.enabled {
            return;
        }
        state.turn = Some(AdvisorTurn {
            turn_id: turn_id.to_string(),
            events: Vec::new(),
        });
    });
}

pub fn record_tool_started(workspace: &Path, name: &str, input: &Value) {
    with_state_mut(workspace, |state| {
        if !state.config.enabled {
            return;
        }
        let Some(turn) = state.turn.as_mut() else {
            return;
        };
        let summary = bounded_json(input, state.config.max_input_bytes);
        push_event(
            turn,
            state.config.max_events,
            AdvisorEvent {
                kind: "tool_started".to_string(),
                name: name.to_string(),
                ok: None,
                summary,
            },
        );
    });
}

pub fn record_tool_completed(workspace: &Path, name: &str, ok: bool, summary: impl Into<String>) {
    with_state_mut(workspace, |state| {
        if !state.config.enabled {
            return;
        }
        let Some(turn) = state.turn.as_mut() else {
            return;
        };
        push_event(
            turn,
            state.config.max_events,
            AdvisorEvent {
                kind: "tool_completed".to_string(),
                name: name.to_string(),
                ok: Some(ok),
                summary: truncate_utf8(&summary.into(), state.config.max_input_bytes),
            },
        );
    });
}

pub fn finish_turn(
    workspace: &Path,
    status: TurnOutcomeStatus,
    error: Option<&str>,
) -> Option<String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        finish_turn_inner(workspace, status, error)
    }))
    .ok()
    .flatten()
}

fn finish_turn_inner(
    workspace: &Path,
    status: TurnOutcomeStatus,
    error: Option<&str>,
) -> Option<String> {
    with_state_mut(workspace, |state| {
        if !state.config.enabled {
            return None;
        }
        #[cfg(test)]
        if state.fail_next_finish {
            state.fail_next_finish = false;
            panic!("simulated advisor failure");
        }
        let turn = state.turn.take()?;
        if turn.events.is_empty() {
            return None;
        }
        let now = Instant::now();
        if let Some(last_emit) = state.last_emit
            && now.duration_since(last_emit) < Duration::from_millis(state.config.min_interval_ms)
        {
            return None;
        }
        let note = render_advice(&state.config.roster, &turn, status, error);
        if state.last_note.as_deref() == Some(note.as_str()) {
            return None;
        }
        state.last_emit = Some(now);
        state.last_note = Some(note.clone());
        Some(note)
    })
}

fn render_advice(
    roster: &[String],
    turn: &AdvisorTurn,
    status: TurnOutcomeStatus,
    error: Option<&str>,
) -> String {
    let watcher = roster.join(", ");
    let total = turn.events.len();
    let failures = turn
        .events
        .iter()
        .filter(|event| event.ok == Some(false))
        .count();
    let unique_tools = {
        let mut names = turn
            .events
            .iter()
            .map(|event| event.name.as_str())
            .collect::<Vec<_>>();
        names.sort_unstable();
        names.dedup();
        names.join(", ")
    };

    if failures > 0 || status == TurnOutcomeStatus::Failed {
        let cause = error.unwrap_or("one or more tool calls failed");
        format!(
            "Advisor ({watcher}): {failures} failed event(s) across {total} bounded tool events in {}; verify the failing path before continuing. Cause: {}",
            if unique_tools.is_empty() {
                "this turn"
            } else {
                unique_tools.as_str()
            },
            truncate_utf8(cause, 180)
        )
    } else {
        format!(
            "Advisor ({watcher}): observed {total} bounded tool events for turn {}; no blocker detected, but keep final verification tied to the changed surface.",
            turn.turn_id
        )
    }
}

fn enable_for_workspace(workspace: &Path, config: AdvisorConfig) -> Value {
    let snapshot = with_state_mut(workspace, |state| {
        state.config = config;
        state.turn = None;
        state.snapshot()
    });
    json!({ "advisor": snapshot })
}

fn disable_for_workspace(workspace: &Path) -> Value {
    let snapshot = with_state_mut(workspace, |state| {
        state.config.enabled = false;
        state.turn = None;
        state.snapshot()
    });
    json!({ "advisor": snapshot })
}

fn status_for_workspace(workspace: &Path) -> Value {
    let snapshot = with_state_mut(workspace, |state| state.snapshot());
    json!({ "advisor": snapshot })
}

fn with_state_mut<R>(workspace: &Path, f: impl FnOnce(&mut WorkspaceAdvisorState) -> R) -> R {
    let key = workspace_key(workspace);
    let mut states = advisor_state()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let state = states.entry(key).or_default();
    f(state)
}

fn advisor_state() -> &'static Mutex<HashMap<String, WorkspaceAdvisorState>> {
    ADVISOR_STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn workspace_key(workspace: &Path) -> String {
    workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf())
        .display()
        .to_string()
}

fn push_event(turn: &mut AdvisorTurn, max_events: usize, event: AdvisorEvent) {
    if max_events == 0 {
        return;
    }
    turn.events.push(event);
    if turn.events.len() > max_events {
        let overflow = turn.events.len() - max_events;
        turn.events.drain(..overflow);
    }
}

fn bounded_json(value: &Value, max_bytes: usize) -> String {
    let raw = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
    truncate_utf8(&raw, max_bytes)
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    if max_bytes == 0 {
        return "[truncated]".to_string();
    }
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...[truncated]", &value[..end])
}

fn roster_from_input(input: &Value) -> Result<Vec<String>, ToolError> {
    let Some(raw) = input.get("roster") else {
        return Ok(vec!["reviewer".to_string()]);
    };
    let values = raw
        .as_array()
        .ok_or_else(|| ToolError::invalid_input("`roster` must be an array".to_string()))?;
    let mut roster = Vec::new();
    for value in values.iter().take(MAX_ROSTER) {
        let item = value.as_str().ok_or_else(|| {
            ToolError::invalid_input("`roster` must contain only strings".to_string())
        })?;
        let trimmed = item.trim();
        if !trimmed.is_empty() && !roster.iter().any(|existing| existing == trimmed) {
            roster.push(trimmed.to_string());
        }
    }
    if roster.is_empty() {
        roster.push("reviewer".to_string());
    }
    Ok(roster)
}

fn usize_from_input(input: &Value, key: &str, default: usize) -> Result<usize, ToolError> {
    let Some(value) = input.get(key) else {
        return Ok(default);
    };
    let number = value
        .as_u64()
        .ok_or_else(|| ToolError::invalid_input(format!("`{key}` must be a positive integer")))?;
    usize::try_from(number)
        .map_err(|_| ToolError::invalid_input(format!("`{key}` exceeds platform range")))
}

fn u64_from_input(input: &Value, key: &str, default: u64) -> Result<u64, ToolError> {
    let Some(value) = input.get(key) else {
        return Ok(default);
    };
    value
        .as_u64()
        .ok_or_else(|| ToolError::invalid_input(format!("`{key}` must be a positive integer")))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdvisorConfig {
    enabled: bool,
    roster: Vec<String>,
    max_events: usize,
    max_input_bytes: usize,
    min_interval_ms: u64,
}

impl Default for AdvisorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            roster: vec!["reviewer".to_string()],
            max_events: DEFAULT_MAX_EVENTS,
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
            min_interval_ms: DEFAULT_MIN_INTERVAL_MS,
        }
    }
}

#[derive(Debug, Default)]
struct WorkspaceAdvisorState {
    config: AdvisorConfig,
    turn: Option<AdvisorTurn>,
    last_note: Option<String>,
    last_emit: Option<Instant>,
    #[cfg(test)]
    fail_next_finish: bool,
}

impl WorkspaceAdvisorState {
    fn snapshot(&self) -> AdvisorSnapshot {
        AdvisorSnapshot {
            enabled: self.config.enabled,
            roster: self.config.roster.clone(),
            max_events: self.config.max_events,
            max_input_bytes: self.config.max_input_bytes,
            min_interval_ms: self.config.min_interval_ms,
            active_turn_id: self.turn.as_ref().map(|turn| turn.turn_id.clone()),
            buffered_events: self
                .turn
                .as_ref()
                .map(|turn| turn.events.len())
                .unwrap_or(0),
        }
    }
}

#[derive(Debug, Clone)]
struct AdvisorTurn {
    turn_id: String,
    events: Vec<AdvisorEvent>,
}

#[derive(Debug, Clone)]
struct AdvisorEvent {
    #[allow(dead_code)]
    kind: String,
    name: String,
    ok: Option<bool>,
    #[allow(dead_code)]
    summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdvisorSnapshot {
    enabled: bool,
    roster: Vec<String>,
    max_events: usize,
    max_input_bytes: usize,
    min_interval_ms: u64,
    active_turn_id: Option<String>,
    buffered_events: usize,
}

#[cfg(test)]
fn reset_for_test(workspace: &Path) {
    let key = workspace_key(workspace);
    advisor_state()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .remove(&key);
}

#[cfg(test)]
fn fail_next_finish_for_test(workspace: &Path) {
    with_state_mut(workspace, |state| {
        state.fail_next_finish = true;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn advisor_is_off_by_default() {
        let tmp = tempdir().expect("tempdir");
        reset_for_test(tmp.path());

        start_turn(tmp.path(), "turn-1");
        record_tool_started(tmp.path(), "read_file", &json!({ "path": "a.rs" }));

        assert_eq!(
            finish_turn(tmp.path(), TurnOutcomeStatus::Completed, None),
            None
        );
    }

    #[test]
    fn enable_disable_controls_advice() {
        let tmp = tempdir().expect("tempdir");
        reset_for_test(tmp.path());
        enable_for_workspace(
            tmp.path(),
            AdvisorConfig {
                enabled: true,
                roster: vec!["reviewer".to_string(), "verifier".to_string()],
                max_events: 4,
                max_input_bytes: 128,
                min_interval_ms: 0,
            },
        );

        start_turn(tmp.path(), "turn-2");
        record_tool_started(tmp.path(), "edit_file", &json!({ "path": "a.rs" }));
        record_tool_completed(tmp.path(), "edit_file", true, "ok");

        let note = finish_turn(tmp.path(), TurnOutcomeStatus::Completed, None).expect("note");
        assert!(note.contains("reviewer, verifier"));
        assert!(note.contains("turn-2"));

        disable_for_workspace(tmp.path());
        start_turn(tmp.path(), "turn-3");
        record_tool_started(tmp.path(), "edit_file", &json!({ "path": "a.rs" }));
        assert_eq!(
            finish_turn(tmp.path(), TurnOutcomeStatus::Completed, None),
            None
        );
    }

    #[test]
    fn bounded_input_and_event_limit_are_enforced() {
        let tmp = tempdir().expect("tempdir");
        reset_for_test(tmp.path());
        enable_for_workspace(
            tmp.path(),
            AdvisorConfig {
                enabled: true,
                roster: vec!["reviewer".to_string()],
                max_events: 2,
                max_input_bytes: 10,
                min_interval_ms: 0,
            },
        );

        start_turn(tmp.path(), "turn-4");
        record_tool_started(
            tmp.path(),
            "first",
            &json!({ "long": "abcdefghijklmnopqrstuvwxyz" }),
        );
        record_tool_started(tmp.path(), "second", &json!({ "path": "b.rs" }));
        record_tool_started(tmp.path(), "third", &json!({ "path": "c.rs" }));

        let snapshot = status_for_workspace(tmp.path());
        assert_eq!(snapshot["advisor"]["buffered_events"], json!(2));
        let summary = with_state_mut(tmp.path(), |state| {
            state
                .turn
                .as_ref()
                .and_then(|turn| turn.events.first())
                .map(|event| event.summary.clone())
                .unwrap()
        });
        assert!(summary.len() <= 24, "summary should be bounded: {summary}");
    }

    #[test]
    fn rate_limit_and_dedup_suppress_noise() {
        let tmp = tempdir().expect("tempdir");
        reset_for_test(tmp.path());
        enable_for_workspace(
            tmp.path(),
            AdvisorConfig {
                enabled: true,
                roster: vec!["reviewer".to_string()],
                max_events: 4,
                max_input_bytes: 128,
                min_interval_ms: 60_000,
            },
        );

        start_turn(tmp.path(), "turn-5");
        record_tool_started(tmp.path(), "read_file", &json!({ "path": "a.rs" }));
        let first = finish_turn(tmp.path(), TurnOutcomeStatus::Completed, None);
        assert!(first.is_some());

        start_turn(tmp.path(), "turn-6");
        record_tool_started(tmp.path(), "read_file", &json!({ "path": "a.rs" }));
        let second = finish_turn(tmp.path(), TurnOutcomeStatus::Completed, None);
        assert_eq!(second, None);
    }

    #[test]
    fn finish_failures_are_isolated_from_parent_turn() {
        let tmp = tempdir().expect("tempdir");
        reset_for_test(tmp.path());
        enable_for_workspace(
            tmp.path(),
            AdvisorConfig {
                enabled: true,
                roster: vec!["reviewer".to_string()],
                max_events: 4,
                max_input_bytes: 128,
                min_interval_ms: 0,
            },
        );
        start_turn(tmp.path(), "turn-7");
        record_tool_started(tmp.path(), "read_file", &json!({ "path": "a.rs" }));
        fail_next_finish_for_test(tmp.path());

        assert_eq!(
            finish_turn(tmp.path(), TurnOutcomeStatus::Completed, None),
            None
        );
    }

    #[tokio::test]
    async fn advisor_tool_configures_workspace_state() {
        let tmp = tempdir().expect("tempdir");
        reset_for_test(tmp.path());
        let ctx = ToolContext::new(tmp.path().to_path_buf());

        let enabled = AdvisorTool
            .execute(
                json!({
                    "operation": "enable",
                    "roster": ["reviewer", "verifier"],
                    "max_events": 3
                }),
                &ctx,
            )
            .await
            .expect("enable");
        assert!(enabled.content.contains("\"enabled\": true"));
        assert!(enabled.content.contains("\"verifier\""));

        let status = AdvisorTool
            .execute(json!({ "operation": "status" }), &ctx)
            .await
            .expect("status");
        assert!(status.content.contains("\"max_events\": 3"));
    }
}
