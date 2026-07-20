//! Engine-native routing for large tool results.
//!
//! The ordinary tool path owns this policy. A tool still returns one raw
//! [`ToolResult`], then the engine projects that result into three views:
//!
//! * an exact session artifact (the durable evidence),
//! * a bounded, stable observation for the model, and
//! * presentation metadata for the transcript/details UI.
//!
//! Workflow-JS and RLM are consumers of the same native handle protocol; they
//! are deliberately not required to activate this path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::context_budget::PressureLevel;
use crate::tools::handle::{ArtifactTextBacking, SharedHandleStore};
use crate::tools::spec::{ToolError, ToolResult};

/// Default token threshold above which a tool result leaves the root context.
pub const DEFAULT_LARGE_OUTPUT_THRESHOLD_TOKENS: usize = 4_096;
/// Default threshold for omitting even the bounded head/tail preview.
pub const DEFAULT_HANDLE_ONLY_THRESHOLD_TOKENS: usize = 256 * 1_024;

const CHARS_PER_TOKEN_ESTIMATE: usize = 3;
const HYBRID_HEAD_CHARS: usize = 900;
const HYBRID_TAIL_CHARS: usize = 900;
const HANDLE_ONLY_PREVIEW_CHARS: usize = 240;
const PREVIEW_REDACTION_OVERLAP_CHARS: usize = 4 * 1024;
const MAX_FACTS: usize = 10;
const MAX_FACT_CHARS: usize = 320;

/// Compatibility mode for the legacy `[workshop]` configuration surface.
///
/// The table name is retained so existing configuration keeps parsing. The
/// default is now the engine-native adaptive path; `classic` preserves the
/// old 100-KiB spill behavior as an explicit fallback.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputRoutingMode {
    #[default]
    Adaptive,
    Classic,
}

/// `[workshop]` remains the compatibility config surface for large outputs.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkshopConfig {
    /// `adaptive` (default) or the legacy `classic` spill path.
    #[serde(default)]
    pub mode: OutputRoutingMode,
    /// Global threshold for leaving small outputs unchanged.
    #[serde(default)]
    pub large_output_threshold_tokens: Option<usize>,
    /// Per-tool threshold overrides.
    #[serde(default)]
    pub per_tool_thresholds: Option<HashMap<String, usize>>,
    /// Threshold at which a repetitive success may omit its head/tail preview.
    #[serde(default)]
    pub handle_only_threshold_tokens: Option<usize>,
}

impl WorkshopConfig {
    #[must_use]
    pub fn threshold_for(&self, tool_name: &str) -> usize {
        if let Some(per_tool) = self.per_tool_thresholds.as_ref()
            && let Some(&limit) = per_tool.get(tool_name)
        {
            return limit.max(1);
        }
        self.large_output_threshold_tokens
            .unwrap_or(DEFAULT_LARGE_OUTPUT_THRESHOLD_TOKENS)
            .max(1)
    }

    #[must_use]
    fn handle_only_threshold(&self) -> usize {
        self.handle_only_threshold_tokens
            .unwrap_or(DEFAULT_HANDLE_ONLY_THRESHOLD_TOKENS)
            .max(1)
    }
}

#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(CHARS_PER_TOKEN_ESTIMATE)
}

/// Explicit policy result. The raw payload is not modified while deciding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteDecision {
    Inline,
    Hybrid {
        estimated_tokens: usize,
        threshold: usize,
    },
    HandleOnly {
        estimated_tokens: usize,
        threshold: usize,
    },
}

impl RouteDecision {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Hybrid { .. } => "hybrid",
            Self::HandleOnly { .. } => "handle_only",
        }
    }
}

/// Pure adaptive policy. Persistence and projection live in
/// [`LargeOutputBroker`].
#[derive(Debug, Clone, Default)]
pub struct LargeOutputRouter {
    config: WorkshopConfig,
}

impl LargeOutputRouter {
    #[must_use]
    pub fn new(config: WorkshopConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub fn mode(&self) -> OutputRoutingMode {
        self.config.mode
    }

    /// Decide how much of a result belongs in root context.
    #[must_use]
    pub fn route(
        &self,
        tool_name: &str,
        result: &ToolResult,
        validated_bounded_projection: bool,
        pressure: PressureLevel,
        route_context_window: Option<u32>,
    ) -> RouteDecision {
        if self.config.mode == OutputRoutingMode::Classic || validated_bounded_projection {
            return RouteDecision::Inline;
        }

        let configured = self.config.threshold_for(tool_name);
        let route_limit = route_context_window
            .and_then(|window| usize::try_from(window).ok())
            .map(|window| (window / 100).max(512));
        let mut threshold = route_limit.map_or(configured, |limit| configured.min(limit));
        threshold = match pressure {
            PressureLevel::Low => threshold,
            PressureLevel::Medium => threshold.saturating_mul(3).div_ceil(4),
            PressureLevel::High => threshold.div_ceil(2),
            PressureLevel::Critical => threshold.div_ceil(4),
        }
        .max(256);

        let estimated_tokens = estimate_tokens(&result.content);
        if estimated_tokens <= threshold {
            return RouteDecision::Inline;
        }

        // Failures retain deterministic facts plus a bounded preview. A large
        // failure must not flood context merely because the command failed.
        if !result.success {
            return RouteDecision::Hybrid {
                estimated_tokens,
                threshold,
            };
        }

        let handle_only_threshold = self.config.handle_only_threshold();
        if estimated_tokens >= handle_only_threshold
            || (estimated_tokens >= threshold.saturating_mul(8)
                && looks_repetitive(&result.content))
        {
            RouteDecision::HandleOnly {
                estimated_tokens,
                threshold,
            }
        } else {
            RouteDecision::Hybrid {
                estimated_tokens,
                threshold,
            }
        }
    }
}

/// Context captured once per model/tool batch and cloned into parallel calls.
#[derive(Clone)]
pub struct LargeOutputBroker {
    router: LargeOutputRouter,
    session_id: String,
    workspace: PathBuf,
    handle_store: SharedHandleStore,
    pressure: PressureLevel,
    route_context_window: Option<u32>,
}

impl LargeOutputBroker {
    #[must_use]
    pub fn new(
        config: WorkshopConfig,
        session_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        handle_store: SharedHandleStore,
        pressure: PressureLevel,
        route_context_window: Option<u32>,
    ) -> Self {
        Self {
            router: LargeOutputRouter::new(config),
            session_id: session_id.into(),
            workspace: workspace.into(),
            handle_store,
            pressure,
            route_context_window,
        }
    }

    /// Project one raw result at the engine boundary.
    ///
    /// Small results are byte-for-byte unchanged. Adaptive large results are
    /// written once under the session artifact root and represented by an
    /// artifact-backed `var_handle`. Classic mode delegates to the existing
    /// spill implementation.
    pub async fn project(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        _input: &Value,
        result: &mut ToolResult,
    ) -> ProjectionOutcome {
        if self.router.mode() == OutputRoutingMode::Classic {
            return match crate::tools::truncate::apply_spillover_with_artifact(
                result,
                tool_call_id,
                tool_name,
                &self.session_id,
            ) {
                Some(path) => ProjectionOutcome::ClassicSpill { path },
                None => ProjectionOutcome::Inline,
            };
        }

        let validated_bounded_projection =
            is_validated_bounded_handle_read_projection(tool_name, result);
        let decision = self.router.route(
            tool_name,
            result,
            validated_bounded_projection,
            self.pressure,
            self.route_context_window,
        );
        if decision == RouteDecision::Inline {
            return ProjectionOutcome::Inline;
        }

        let raw = std::mem::take(&mut result.content);
        let sha256 = crate::hashing::sha256_hex(raw.as_bytes());
        let bytes = raw.len() as u64;
        let chars = raw.chars().count();
        let lines = raw.lines().count();
        let estimated_tokens = estimate_tokens(&raw);
        let facts = deterministic_facts(result, &raw, &self.workspace, decision);
        let structured_summary = stable_structured_summary(tool_name, &raw, &self.workspace);
        let payload_kind = payload_kind(tool_name, &raw);
        let (head, tail) = safe_preview(&raw, decision, &self.workspace);
        // The model sees only this opaque, current-session alias. Hashing the
        // call identity preserves one handle per result without leaking raw
        // provider/tool-call UUIDs into prompt context.
        let call_digest = crate::hashing::sha256_hex(tool_call_id.as_bytes());
        let handle_name = format!("output_{sha256}_{}", &call_digest[..12]);
        let artifact_id = format!("art_{handle_name}");

        let write_session_id = self.session_id.clone();
        let write_artifact_id = artifact_id.clone();
        let raw = std::sync::Arc::new(raw);
        let write_raw = std::sync::Arc::clone(&raw);
        let stored = tokio::task::spawn_blocking(move || {
            crate::artifacts::write_session_artifact(
                &write_session_id,
                &write_artifact_id,
                write_raw.as_str(),
            )
        })
        .await
        .unwrap_or_else(|error| {
            Err(std::io::Error::other(format!(
                "artifact writer task failed: {error}"
            )))
        });
        let (absolute_path, relative_path) = match stored {
            Ok(paths) => paths,
            Err(err) => {
                // Never trade exact evidence for a tidy envelope. If the
                // canonical write fails, return the original bytes inline and
                // mark detail storage unavailable. `Arc` keeps ownership on
                // this task even if the blocking writer itself fails.
                result.content = std::sync::Arc::try_unwrap(raw)
                    .unwrap_or_else(|shared| shared.as_ref().clone());
                let display = format!(
                    "{} output returned inline; artifact store unavailable",
                    crate::artifacts::format_byte_size(bytes)
                );
                attach_projection_metadata(
                    result,
                    ProjectionMetadata {
                        route: decision.label(),
                        artifact_id: None,
                        session_id: None,
                        absolute_path: None,
                        relative_path: None,
                        sha256: Some(sha256.clone()),
                        bytes,
                        status: status_label(result.success),
                        failure_count: failure_count(result.success, &facts),
                        handle: None,
                        display,
                        primary: primary_fact(&facts),
                        available: false,
                        store_error: Some(io_error_class(&err)),
                    },
                );
                if let Some(metadata) = result.metadata.as_mut() {
                    metadata["truncated"] = json!(false);
                    metadata["adaptive_evidence"]["inline_fallback"] = json!(true);
                }
                return ProjectionOutcome::Unavailable {
                    decision,
                    bytes,
                    sha256,
                };
            }
        };

        let handle = {
            let backing = ArtifactTextBacking {
                relative_path: relative_path.clone(),
                byte_length: bytes,
                char_length: chars,
                line_count: Some(lines),
                sha256: sha256.clone(),
            };
            let mut store = self.handle_store.lock().await;
            store.insert_artifact_text(
                self.session_id.clone(),
                handle_name.clone(),
                backing,
                head.clone().unwrap_or_default(),
            )
        };
        let display = display_summary(true, result.success, bytes, &facts);
        let primary = primary_fact(&facts);
        let envelope = EvidenceEnvelope {
            schema: EVIDENCE_SCHEMA,
            status: status_label(result.success),
            tool: tool_name.to_string(),
            payload_kind,
            bytes,
            estimated_tokens,
            handle: Some(handle_name),
            sha256: Some(sha256.clone()),
            facts,
            structured_summary,
            preview: EvidencePreview { head, tail },
            inspect: Some(EvidenceInspection {
                tool: "handle_read",
                operations: &["count", "slice", "range", "search", "introspect"],
            }),
            evidence_available: true,
        };
        result.content = serde_json::to_string(&envelope).unwrap_or_else(|_| {
            format!(
                "{{\"schema\":\"{EVIDENCE_SCHEMA}\",\"status\":\"{}\",\"evidence_available\":true}}",
                status_label(result.success)
            )
        });
        attach_projection_metadata(
            result,
            ProjectionMetadata {
                route: decision.label(),
                artifact_id: Some(artifact_id.clone()),
                session_id: Some(self.session_id.clone()),
                absolute_path: Some(absolute_path.clone()),
                relative_path: Some(relative_path.clone()),
                sha256: Some(sha256.clone()),
                bytes,
                status: status_label(result.success),
                failure_count: failure_count(result.success, &envelope.facts),
                handle: Some(handle),
                display,
                primary,
                available: true,
                store_error: None,
            },
        );

        ProjectionOutcome::Stored {
            decision,
            artifact_id,
            path: absolute_path,
            bytes,
            sha256,
            model_bytes: result.content.len(),
        }
    }

    /// Project either a normal tool result or a genuinely large execution
    /// error. Small `ToolError`s retain their typed error semantics; when an
    /// error itself exceeds the adaptive threshold it is normalized to a
    /// failed `ToolResult` so the canonical artifact, handle metadata, and TUI
    /// detail path can travel together.
    pub async fn project_terminal_result(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        input: &Value,
        terminal: &mut Result<ToolResult, ToolError>,
    ) -> ProjectionOutcome {
        if let Ok(result) = terminal.as_mut() {
            return self.project(tool_call_id, tool_name, input, result).await;
        }

        let ToolError::ExecutionFailed { message } =
            terminal.as_ref().expect_err("checked error result")
        else {
            return ProjectionOutcome::Inline;
        };
        let mut normalized = ToolResult::error(message.clone()).with_metadata(json!({
            "normalized_tool_error": true,
        }));
        let outcome = self
            .project(tool_call_id, tool_name, input, &mut normalized)
            .await;
        if !matches!(outcome, ProjectionOutcome::Inline) {
            *terminal = Ok(normalized);
        }
        outcome
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionOutcome {
    Inline,
    ClassicSpill {
        path: PathBuf,
    },
    Stored {
        decision: RouteDecision,
        artifact_id: String,
        path: PathBuf,
        bytes: u64,
        sha256: String,
        model_bytes: usize,
    },
    Unavailable {
        decision: RouteDecision,
        bytes: u64,
        sha256: String,
    },
}

impl ProjectionOutcome {
    #[must_use]
    pub fn audit_payload(
        &self,
        tool_id: &str,
        tool_name: &str,
        source: Option<&str>,
    ) -> Option<Value> {
        let (route, bytes, model_bytes, artifact_id, available) = match self {
            Self::Inline => return None,
            Self::ClassicSpill { .. } => ("classic", None, None, None, true),
            Self::Stored {
                decision,
                artifact_id,
                bytes,
                model_bytes,
                ..
            } => (
                decision.label(),
                Some(*bytes),
                Some(*model_bytes as u64),
                Some(artifact_id.as_str()),
                true,
            ),
            Self::Unavailable {
                decision, bytes, ..
            } => (decision.label(), Some(*bytes), None, None, false),
        };
        Some(json!({
            "event": "tool.evidence_projected",
            "tool_id": tool_id,
            "tool_name": tool_name,
            "route": route,
            "stored_bytes": bytes,
            "model_bytes": model_bytes,
            "artifact_id": artifact_id,
            "evidence_available": available,
            "source": source,
        }))
    }
}

pub const EVIDENCE_SCHEMA: &str = "codewhale.tool_evidence.v1";

#[derive(Debug, Clone, Serialize)]
struct EvidenceEnvelope {
    schema: &'static str,
    status: &'static str,
    tool: String,
    payload_kind: &'static str,
    bytes: u64,
    estimated_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    handle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    facts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    structured_summary: Option<String>,
    preview: EvidencePreview,
    #[serde(skip_serializing_if = "Option::is_none")]
    inspect: Option<EvidenceInspection>,
    evidence_available: bool,
}

#[derive(Debug, Clone, Serialize)]
struct EvidencePreview {
    #[serde(skip_serializing_if = "Option::is_none")]
    head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct EvidenceInspection {
    tool: &'static str,
    operations: &'static [&'static str],
}

struct ProjectionMetadata {
    route: &'static str,
    artifact_id: Option<String>,
    session_id: Option<String>,
    absolute_path: Option<PathBuf>,
    relative_path: Option<PathBuf>,
    sha256: Option<String>,
    bytes: u64,
    status: &'static str,
    failure_count: Option<usize>,
    handle: Option<crate::tools::handle::VarHandle>,
    display: String,
    primary: Option<String>,
    available: bool,
    store_error: Option<&'static str>,
}

fn attach_projection_metadata(result: &mut ToolResult, projection: ProjectionMetadata) {
    let metadata = result.metadata.get_or_insert_with(|| json!({}));
    if !metadata.is_object() {
        let prior = std::mem::replace(metadata, json!({}));
        metadata["_prior"] = prior;
    }
    metadata["adaptive_evidence"] = json!({
        "schema": EVIDENCE_SCHEMA,
        "route": projection.route,
        "artifact_id": projection.artifact_id.clone(),
        "artifact_session_id": projection.session_id.clone(),
        "artifact_relative_path": projection.relative_path.as_deref().map(crate::artifacts::format_artifact_relative_path),
        "artifact_path": projection.absolute_path.as_ref().map(|path| path.display().to_string()),
        "sha256": projection.sha256.clone(),
        "bytes": projection.bytes,
        "status": projection.status,
        "failure_count": projection.failure_count,
        "handle": projection.handle.clone(),
        "display_summary": projection.display.clone(),
        "display_primary": projection.primary.clone(),
        "details_available": projection.available,
        "store_error": projection.store_error,
    });

    // Compatibility fields keep the existing artifact index, session save,
    // and details UI working while the typed metadata consumers migrate.
    if let Some(path) = projection.absolute_path {
        metadata["spillover_path"] = json!(path.display().to_string());
        metadata["artifact_path"] = json!(path.display().to_string());
    }
    if let Some(path) = projection.relative_path {
        metadata["artifact_relative_path"] =
            json!(crate::artifacts::format_artifact_relative_path(&path));
    }
    if let Some(id) = projection.artifact_id {
        metadata["artifact_id"] = json!(id);
    }
    if let Some(session_id) = projection.session_id {
        metadata["artifact_session_id"] = json!(session_id);
    }
    metadata["artifact_byte_size"] = json!(projection.bytes);
    if let Some(sha256) = projection.sha256 {
        metadata["content_digest"] = json!(format!("sha256:{sha256}"));
        metadata["artifact_sha256"] = json!(sha256);
    }
    metadata["original_byte_count"] = json!(projection.bytes);
    metadata["artifact_preview"] = json!(match projection.primary {
        Some(primary) => format!("{} · {primary}", projection.display),
        None => projection.display,
    });
    metadata["truncated"] = json!(true);
}

fn status_label(success: bool) -> &'static str {
    if success { "succeeded" } else { "failed" }
}

fn payload_kind(tool_name: &str, raw: &str) -> &'static str {
    if serde_json::from_str::<Value>(raw).is_ok() {
        "json"
    } else if raw.lines().take(5).any(|line| {
        let line = line.trim();
        line.starts_with("diff --git") || line.starts_with("@@")
    }) {
        "diff"
    } else if matches!(
        tool_name,
        "exec_shell"
            | "exec_shell_wait"
            | "task_shell_wait"
            | "run_tests"
            | "run_verifiers"
            | "task_gate_run"
    ) {
        "command_output"
    } else {
        "text"
    }
}

fn stable_structured_summary(tool_name: &str, raw: &str, workspace: &Path) -> Option<String> {
    const MAX_STRUCTURED_SUMMARY_CHARS: usize = 3_000;
    let summary = match tool_name {
        "agent" => crate::core::engine::compact_subagent_tool_result_for_context(tool_name, raw),
        _ => None,
    }?;
    Some(truncate_chars(
        &normalize_visible(&summary, workspace),
        MAX_STRUCTURED_SUMMARY_CHARS,
    ))
}

fn is_validated_bounded_handle_read_projection(tool_name: &str, result: &ToolResult) -> bool {
    const MAX_SERIALIZED_PROJECTION_BYTES: usize = 512 * 1024;
    if tool_name != "handle_read"
        || !result.success
        || result.content.len() > MAX_SERIALIZED_PROJECTION_BYTES
    {
        return false;
    }
    let Ok(Value::Object(output)) = serde_json::from_str::<Value>(&result.content) else {
        return false;
    };
    matches!(
        output.get("projection").and_then(Value::as_str),
        Some("count" | "slice" | "range" | "search" | "jsonpath" | "introspect")
    )
}

fn safe_preview(
    raw: &str,
    decision: RouteDecision,
    workspace: &Path,
) -> (Option<String>, Option<String>) {
    let (head_chars, tail_chars) = match decision {
        RouteDecision::Inline => return (None, None),
        RouteDecision::Hybrid { threshold, .. } => {
            let preview_budget = threshold
                .saturating_mul(CHARS_PER_TOKEN_ESTIMATE)
                .div_ceil(3)
                .clamp(192, HYBRID_HEAD_CHARS + HYBRID_TAIL_CHARS);
            (preview_budget.div_ceil(2), preview_budget / 2)
        }
        RouteDecision::HandleOnly { threshold, .. } => (
            threshold
                .saturating_mul(CHARS_PER_TOKEN_ESTIMATE)
                .div_ceil(6)
                .clamp(96, HANDLE_ONLY_PREVIEW_CHARS),
            0,
        ),
    };
    let raw_chars = raw.chars().count();
    let head_sample_chars = head_chars
        .saturating_add(PREVIEW_REDACTION_OVERLAP_CHARS)
        .min(raw_chars);
    let mut head_sample = raw.chars().take(head_sample_chars).collect::<String>();
    // If the overlap still ends inside one enormous token/path, omit that
    // trailing token rather than exposing a prefix that the redactor cannot
    // recognize as a whole workspace/home path.
    if raw_chars > head_sample_chars
        && head_sample
            .chars()
            .next_back()
            .is_some_and(|character| !character.is_whitespace())
    {
        if let Some((index, character)) = head_sample
            .char_indices()
            .rfind(|(_, character)| character.is_whitespace())
        {
            head_sample.truncate(index + character.len_utf8());
        } else {
            head_sample.clear();
        }
    }
    let head = preview_head(&normalize_visible(&head_sample, workspace), head_chars);
    let tail = if tail_chars == 0 {
        String::new()
    } else {
        let tail_sample_chars = tail_chars
            .saturating_add(PREVIEW_REDACTION_OVERLAP_CHARS)
            .min(raw_chars);
        let mut tail = raw
            .chars()
            .rev()
            .take(tail_sample_chars)
            .collect::<Vec<_>>();
        tail.reverse();
        let mut tail = tail.into_iter().collect::<String>();
        // The sampled prefix may begin halfway through a secret/path token.
        // Drop that partial token before redaction; if there is no boundary,
        // an empty preview is safer than an unverifiable fragment.
        if raw_chars > tail_sample_chars {
            if let Some((index, character)) = tail
                .char_indices()
                .find(|(_, character)| character.is_whitespace())
            {
                tail.drain(..index + character.len_utf8());
            } else {
                tail.clear();
            }
        }
        preview_tail(&normalize_visible(&tail, workspace), tail_chars)
    };
    (
        (!head.is_empty()).then_some(head),
        (!tail.is_empty()).then_some(tail),
    )
}

fn preview_head(text: &str, max_chars: usize) -> String {
    let mut end = max_chars.min(text.chars().count());
    let marker = codewhale_config::persistence::REDACTED;
    let marker_chars = marker.chars().count();
    for (byte_index, _) in text.match_indices(marker) {
        let start = text[..byte_index].chars().count();
        let marker_end = start.saturating_add(marker_chars);
        if start < end && marker_end > end {
            // A complete redaction marker is safer and clearer than exposing
            // a truncated fragment such as `[r`. The bounded overrun is at
            // most the marker length and contains no source payload bytes.
            end = marker_end;
            break;
        }
    }
    text.chars().take(end).collect()
}

fn preview_tail(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    let mut start = total.saturating_sub(max_chars);
    let marker = codewhale_config::persistence::REDACTED;
    let marker_chars = marker.chars().count();
    for (byte_index, _) in text.match_indices(marker) {
        let marker_start = text[..byte_index].chars().count();
        let marker_end = marker_start.saturating_add(marker_chars);
        if marker_start < start && marker_end > start {
            start = marker_start;
            break;
        }
    }
    text.chars().skip(start).collect()
}

fn deterministic_facts(
    result: &ToolResult,
    raw: &str,
    workspace: &Path,
    decision: RouteDecision,
) -> Vec<String> {
    let mut facts = Vec::new();
    let (max_facts, max_fact_chars) = match decision {
        RouteDecision::Inline => (MAX_FACTS, MAX_FACT_CHARS),
        RouteDecision::Hybrid { threshold, .. } | RouteDecision::HandleOnly { threshold, .. } => {
            if threshold <= 512 {
                (3, 140)
            } else if threshold <= 2_048 {
                (6, 220)
            } else {
                (MAX_FACTS, MAX_FACT_CHARS)
            }
        }
    };
    let metadata = result.metadata.as_ref();
    if let Some(exit_code) = metadata.and_then(|value| value.get("exit_code"))
        && !exit_code.is_null()
    {
        push_fact(
            &mut facts,
            format!("exit code: {exit_code}"),
            workspace,
            max_facts,
            max_fact_chars,
        );
    }
    if let Some(failed_children) = metadata
        .and_then(|value| value.get("failure_count"))
        .and_then(Value::as_u64)
        .filter(|count| *count > 0)
    {
        push_fact(
            &mut facts,
            format!("{failed_children} failed child results"),
            workspace,
            max_facts,
            max_fact_chars,
        );
    }

    if let Some(cargo) = metadata.and_then(|value| value.get("cargo_failure_summary")) {
        for key in ["final_error"] {
            if let Some(value) = cargo.get(key).and_then(Value::as_str) {
                push_fact(
                    &mut facts,
                    value.to_string(),
                    workspace,
                    max_facts,
                    max_fact_chars,
                );
            }
        }
        for key in [
            "error_codes",
            "failing_tests",
            "primary_errors",
            "panic_locations",
        ] {
            if let Some(values) = cargo.get(key).and_then(Value::as_array) {
                for value in values.iter().filter_map(Value::as_str) {
                    push_fact(
                        &mut facts,
                        value.to_string(),
                        workspace,
                        max_facts,
                        max_fact_chars,
                    );
                }
            }
        }
        for key in ["test_result", "summary"] {
            if let Some(value) = cargo.get(key).and_then(Value::as_str) {
                push_fact(
                    &mut facts,
                    value.to_string(),
                    workspace,
                    max_facts,
                    max_fact_chars,
                );
            }
        }
    }

    let mut primary_signals: Vec<(String, usize)> = Vec::new();
    let mut secondary_signals: Vec<(String, usize)> = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        let Some(priority) = signal_priority(trimmed) else {
            continue;
        };
        let normalized = normalize_visible(trimmed, workspace);
        let signals = if priority {
            &mut primary_signals
        } else {
            &mut secondary_signals
        };
        if let Some((_, count)) = signals.iter_mut().find(|(line, _)| line == &normalized) {
            *count = count.saturating_add(1);
        } else if signals.len() < max_facts.saturating_mul(3) {
            signals.push((normalized, 1));
        }
    }
    for (signal, count) in primary_signals.into_iter().chain(secondary_signals) {
        let rendered = if count > 1 {
            format!("{signal} (repeated {count} times)")
        } else {
            signal
        };
        push_fact(&mut facts, rendered, workspace, max_facts, max_fact_chars);
        if facts.len() >= max_facts {
            break;
        }
    }

    facts
}

fn push_fact(
    facts: &mut Vec<String>,
    value: String,
    workspace: &Path,
    max_facts: usize,
    max_fact_chars: usize,
) {
    if facts.len() >= max_facts {
        return;
    }
    let value = truncate_chars(&normalize_visible(&value, workspace), max_fact_chars);
    if !value.is_empty() && !facts.iter().any(|existing| existing == &value) {
        facts.push(value);
    }
}

fn primary_fact(facts: &[String]) -> Option<String> {
    facts
        .iter()
        .find(|fact| {
            let lower = fact.to_ascii_lowercase();
            lower.contains("error")
                || lower.contains("failed")
                || lower.contains("panic")
                || lower.contains("exception")
        })
        .or_else(|| facts.first())
        .cloned()
}

fn display_summary(available: bool, success: bool, bytes: u64, facts: &[String]) -> String {
    let size = crate::artifacts::format_byte_size(bytes);
    if !available {
        return format!("{size} output could not be kept; bounded evidence shown");
    }
    let failures = failure_count(success, facts);
    match failures {
        Some(1) => format!("1 failure · {size} output kept"),
        Some(count) => format!("{count} failures · {size} output kept"),
        None => format!("{size} output kept for inspection"),
    }
}

fn failure_count(success: bool, facts: &[String]) -> Option<usize> {
    if success {
        return None;
    }
    facts
        .iter()
        .find_map(|fact| parse_failed_count(fact))
        .or(Some(1))
}

fn parse_failed_count(text: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    // Libtest begins its standard summary with `test result: FAILED.` before
    // the numeric `7 failed` field. Select the final occurrence so the compact
    // receipt reports the real count instead of falling back to one failure.
    let failed = lower.rfind(" failed")?;
    lower[..failed]
        .split(|ch: char| !ch.is_ascii_digit())
        .rfind(|part| !part.is_empty())?
        .parse()
        .ok()
}

fn normalize_visible(text: &str, workspace: &Path) -> String {
    let mut normalized = text.to_string();
    let mut roots = vec![workspace.to_path_buf()];
    if let Ok(canonical) = workspace.canonicalize()
        && canonical != workspace
    {
        roots.push(canonical);
    }
    for root in roots {
        let display = root.display().to_string();
        if !display.is_empty() {
            normalized = normalized.replace(&display, ".");
            normalized = normalized.replace(&display.replace('\\', "/"), ".");
        }
    }
    if let Some(home) = dirs::home_dir() {
        let display = home.display().to_string();
        if !display.is_empty() {
            normalized = normalized.replace(&display, "~");
            normalized = normalized.replace(&display.replace('\\', "/"), "~");
        }
    }
    normalized = normalize_volatile_host_details(&normalized);
    let redacted = codewhale_config::persistence::redact_secrets(&normalized);
    redacted
        .chars()
        .filter_map(|character| match character {
            '\n' | '\t' => Some(character),
            '\r' => None,
            character if character.is_control() => Some(' '),
            character => Some(character),
        })
        .collect()
}

fn normalize_volatile_host_details(text: &str) -> String {
    static FILE_URI: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static UNIX_PATH: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static WINDOWS_PATH: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static UUID: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static ISO_TIMESTAMP: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

    let file_uri = FILE_URI.get_or_init(|| {
        regex::Regex::new(r#"(?i)\bfile:///[^\s\"'()\[\]{}<>]+"#).expect("static file URI regex")
    });
    let unix_path = UNIX_PATH.get_or_init(|| {
        regex::Regex::new(r#"(?m)(^|[\s\"'(\[=])(/[^\s\"'()\[\]{}<>]+)"#)
            .expect("static Unix path regex")
    });
    let windows_path = WINDOWS_PATH.get_or_init(|| {
        regex::Regex::new(r#"(?mi)(^|[\s\"'(\[=])([a-z]:[\\/][^\s\"'()\[\]{}<>]+)"#)
            .expect("static Windows path regex")
    });
    let uuid = UUID.get_or_init(|| {
        regex::Regex::new(r"(?i)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
            .expect("static UUID regex")
    });
    let timestamp = ISO_TIMESTAMP.get_or_init(|| {
        regex::Regex::new(
            r"\b\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?\b",
        )
        .expect("static timestamp regex")
    });

    let normalized = file_uri.replace_all(text, "file://<host-path>");
    let normalized = unix_path.replace_all(&normalized, "$1<host-path>");
    let normalized = windows_path.replace_all(&normalized, "$1<host-path>");
    let normalized = uuid.replace_all(&normalized, "<id>");
    timestamp
        .replace_all(&normalized, "<timestamp>")
        .into_owned()
}

fn looks_repetitive(text: &str) -> bool {
    let mut sampled = 0usize;
    let mut unique: Vec<&str> = Vec::new();
    for line in text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(2_000)
    {
        sampled += 1;
        if unique.len() < 512 && !unique.contains(&line) {
            unique.push(line);
        }
    }
    sampled >= 64 && unique.len().saturating_mul(5) < sampled
}

fn signal_priority(line: &str) -> Option<bool> {
    if line.is_empty() {
        return None;
    }
    let lower = line.to_ascii_lowercase();
    let primary = [
        "error",
        "failed",
        "failure",
        "panic",
        "exception",
        "traceback",
        "assertion",
        "exit code",
        "test result",
        "thread '",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if primary {
        Some(true)
    } else if lower.contains("warning") || lower.contains(" --> ") {
        Some(false)
    } else {
        None
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn io_error_class(error: &std::io::Error) -> &'static str {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::NotFound => "not_found",
        ErrorKind::PermissionDenied => "permission_denied",
        ErrorKind::AlreadyExists => "already_exists",
        ErrorKind::InvalidInput | ErrorKind::InvalidData => "invalid_data",
        ErrorKind::WriteZero | ErrorKind::UnexpectedEof => "incomplete_write",
        _ => "io_error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::handle::{HandleReadTool, new_shared_handle_store};
    use crate::tools::spec::{ToolContext, ToolSpec};
    use tempfile::tempdir;

    fn result(content: &str, success: bool) -> ToolResult {
        ToolResult {
            content: content.to_string(),
            success,
            metadata: None,
        }
    }

    #[test]
    fn small_output_stays_inline() {
        let router = LargeOutputRouter::default();
        assert_eq!(
            router.route(
                "read_file",
                &result("small", true),
                false,
                PressureLevel::Low,
                Some(128_000),
            ),
            RouteDecision::Inline
        );
    }

    #[test]
    fn large_success_and_failure_are_hybrid() {
        let router = LargeOutputRouter::default();
        let raw = "line\n".repeat(3_000);
        assert!(matches!(
            router.route(
                "exec_shell",
                &result(&raw, true),
                false,
                PressureLevel::Low,
                Some(128_000),
            ),
            RouteDecision::Hybrid { .. }
        ));
        assert!(matches!(
            router.route(
                "exec_shell",
                &result(&raw, false),
                false,
                PressureLevel::Low,
                Some(128_000),
            ),
            RouteDecision::Hybrid { .. }
        ));
    }

    #[test]
    fn successful_tool_does_not_inherit_failure_count_from_payload_text() {
        let facts = vec!["test result: FAILED. 0 passed; 7 failed".to_string()];

        assert_eq!(failure_count(true, &facts), None);
        let display = display_summary(true, true, 2_000_000, &facts);
        assert!(display.contains("output kept for inspection"), "{display}");
        assert!(!display.contains("7 failures"), "{display}");
    }

    #[test]
    fn libtest_summary_uses_the_numeric_failed_count() {
        let facts =
            vec!["test result: FAILED. 12 passed; 7 failed; 1 ignored; 0 measured".to_string()];

        assert_eq!(failure_count(false, &facts), Some(7));
        assert!(display_summary(true, false, 2_100_000, &facts).starts_with("7 failures ·"),);
    }

    #[test]
    fn only_validated_bounded_projection_and_classic_mode_stay_inline_at_policy_layer() {
        let raw = result(&"line\n".repeat(3_000), true);
        let adaptive = LargeOutputRouter::default();
        assert!(matches!(
            adaptive.route(
                "exec_shell",
                &raw,
                false,
                PressureLevel::Critical,
                Some(32_000),
            ),
            RouteDecision::Hybrid { .. } | RouteDecision::HandleOnly { .. }
        ));
        assert_eq!(
            adaptive.route(
                "handle_read",
                &raw,
                true,
                PressureLevel::Critical,
                Some(32_000),
            ),
            RouteDecision::Inline
        );
        let classic = LargeOutputRouter::new(WorkshopConfig {
            mode: OutputRoutingMode::Classic,
            ..WorkshopConfig::default()
        });
        assert_eq!(
            classic.route(
                "exec_shell",
                &raw,
                false,
                PressureLevel::Critical,
                Some(32_000),
            ),
            RouteDecision::Inline
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bounded_handle_read_projection_does_not_recursively_offload() {
        let temp = tempdir().expect("tempdir");
        let broker = LargeOutputBroker::new(
            WorkshopConfig {
                large_output_threshold_tokens: Some(100),
                ..WorkshopConfig::default()
            },
            "bounded-read-session",
            temp.path(),
            new_shared_handle_store(),
            PressureLevel::Critical,
            Some(32_000),
        );
        let content = serde_json::to_string(&json!({
            "handle": "output_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa_bbbbbbbbbbbb",
            "projection": "range",
            "content": "bounded projection line\n".repeat(1_000),
            "truncated": false,
        }))
        .expect("projection JSON");
        let mut result = ToolResult::success(content.clone());

        let outcome = broker
            .project(
                "bounded-read-call",
                "handle_read",
                &json!({"range": {"start": 1, "end": 1_000}, "max_chars": 50_000}),
                &mut result,
            )
            .await;

        assert_eq!(outcome, ProjectionOutcome::Inline);
        assert_eq!(result.content, content);
        assert!(result.metadata.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)] // Serializes the process-global artifact-root override.
    async fn ordinary_raw_or_materialize_flag_cannot_bypass_canonical_storage() {
        let _root_guard = crate::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let temp = tempdir().expect("tempdir");
        let prior =
            crate::artifacts::set_test_artifact_sessions_root(Some(temp.path().join("sessions")));
        struct RestoreRoot(Option<PathBuf>);
        impl Drop for RestoreRoot {
            fn drop(&mut self) {
                crate::artifacts::set_test_artifact_sessions_root(self.0.take());
            }
        }
        let _restore = RestoreRoot(prior);
        let broker = LargeOutputBroker::new(
            WorkshopConfig {
                large_output_threshold_tokens: Some(100),
                ..WorkshopConfig::default()
            },
            "ordinary-materialize-session",
            temp.path(),
            new_shared_handle_store(),
            PressureLevel::Low,
            Some(128_000),
        );
        let raw = "ordinary raw sentinel\n".repeat(10_000);
        let mut result = ToolResult::success(raw.clone());

        let outcome = broker
            .project(
                "ordinary-materialize-call",
                "plugin_run",
                &json!({"raw": true, "materialize": true}),
                &mut result,
            )
            .await;
        let ProjectionOutcome::Stored { path, .. } = outcome else {
            panic!("ordinary flags must not disable adaptive storage")
        };

        assert!(crate::core::engine::is_adaptive_evidence_envelope(
            &result.content
        ));
        assert_eq!(std::fs::read_to_string(path).unwrap(), raw);
    }

    #[test]
    fn pressure_tightens_the_inline_threshold() {
        let router = LargeOutputRouter::new(WorkshopConfig {
            large_output_threshold_tokens: Some(4_096),
            ..WorkshopConfig::default()
        });
        let raw = result(&"x".repeat(5_000), true);
        assert_eq!(
            router.route(
                "read_file",
                &raw,
                false,
                PressureLevel::Low,
                Some(1_000_000),
            ),
            RouteDecision::Inline
        );
        assert!(matches!(
            router.route(
                "read_file",
                &raw,
                false,
                PressureLevel::Critical,
                Some(1_000_000),
            ),
            RouteDecision::Hybrid { .. }
        ));
    }

    #[test]
    fn workspace_paths_normalize_stably() {
        let a = normalize_visible(
            "/tmp/one/project/src/lib.rs:10: error[E0382]",
            Path::new("/tmp/one/project"),
        );
        let b = normalize_visible(
            "/var/two/project/src/lib.rs:10: error[E0382]",
            Path::new("/var/two/project"),
        );
        assert_eq!(a, b);
        assert_eq!(a, "./src/lib.rs:10: error[E0382]");
    }

    #[test]
    fn model_visible_evidence_removes_external_paths_uuids_and_timestamps() {
        let volatile = "error: /private/var/folders/zz/build/output.rs:41 \
                        file:///private/var/folders/zz/other.log \
                        agent 550e8400-e29b-41d4-a716-446655440000 \
                        at 2026-07-20T15:03:42.123Z and 2026-07-20 15:03:42";
        let normalized = normalize_visible(volatile, Path::new("/workspace/project"));

        assert!(!normalized.contains("/private/var"), "{normalized}");
        assert!(!normalized.contains("550e8400"), "{normalized}");
        assert!(!normalized.contains("2026-07-20T"), "{normalized}");
        assert!(!normalized.contains("2026-07-20 15"), "{normalized}");
        assert!(!normalized.contains("file:///private"), "{normalized}");
        assert!(normalized.contains("<host-path>"), "{normalized}");
        assert!(normalized.contains("<id>"), "{normalized}");
        assert!(normalized.contains("<timestamp>"), "{normalized}");

        let raw = format!("{volatile}\n{}", "ordinary line\n".repeat(1_000));
        let decision = RouteDecision::Hybrid {
            estimated_tokens: 10_000,
            threshold: 900,
        };
        let result = ToolResult::error(raw.clone());
        let facts = deterministic_facts(&result, &raw, Path::new("/workspace/project"), decision);
        let (head, tail) = safe_preview(&raw, decision, Path::new("/workspace/project"));
        let visible = format!("{facts:?} {head:?} {tail:?}");
        assert!(!visible.contains("/private/var"), "{visible}");
        assert!(!visible.contains("550e8400"), "{visible}");
        assert!(!visible.contains("2026-07-20T"), "{visible}");
    }

    #[test]
    fn model_visible_evidence_redacts_home_and_terminal_controls() {
        let home = dirs::home_dir().expect("test host has a home directory");
        let raw = format!(
            "{}\u{1b}[31m/.cargo/registry/src/lib.rs:10: error\r\n",
            home.display()
        );
        let normalized = normalize_visible(&raw, Path::new("/workspace/elsewhere"));
        assert!(!normalized.contains(&home.display().to_string()));
        assert!(normalized.contains('~'));
        assert!(!normalized.contains('\u{1b}'));
        assert!(!normalized.contains('\r'));
    }

    #[test]
    fn preview_redacts_paths_and_tokens_crossing_display_cutoff() {
        let workspace = Path::new("/Volumes/VIXinSSD/CW/private-project");
        let decision = RouteDecision::Hybrid {
            estimated_tokens: 10_000,
            threshold: 900,
        };
        let path_raw = format!(
            "{}{}{}",
            "x".repeat(440),
            workspace.join("src/private.rs").display(),
            " ordinary tail ".repeat(600)
        );
        let (path_head, _) = safe_preview(&path_raw, decision, workspace);
        let path_head = path_head.expect("head preview");
        assert!(!path_head.contains("/Volumes/"), "{path_head}");
        assert!(!path_head.contains("VIXinSSD"), "{path_head}");

        let secret_raw = format!(
            "{} sk-super-secret-provider-token{}",
            "y".repeat(447),
            " safe tail ".repeat(600)
        );
        let (secret_head, _) = safe_preview(&secret_raw, decision, workspace);
        let secret_head = secret_head.expect("secret preview");
        assert!(!secret_head.contains("sk-super"), "{secret_head}");
        assert!(secret_head.contains("[redacted]"), "{secret_head}");

        let tail_secret_raw = format!(
            "{} sk-super-secret-provider-token {}",
            "safe head ".repeat(600),
            "z".repeat(447)
        );
        let (_, secret_tail) = safe_preview(&tail_secret_raw, decision, workspace);
        let secret_tail = secret_tail.expect("secret tail preview");
        assert!(!secret_tail.contains("sk-super"), "{secret_tail}");
        assert!(secret_tail.contains("[redacted]"), "{secret_tail}");
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)] // Serializes the process-global artifact-root override.
    async fn engine_native_broker_keeps_exact_failure_and_bounded_root_observation() {
        let _root_guard = crate::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let temp = tempdir().expect("tempdir");
        let prior =
            crate::artifacts::set_test_artifact_sessions_root(Some(temp.path().join("sessions")));
        struct RestoreRoot(Option<PathBuf>);
        impl Drop for RestoreRoot {
            fn drop(&mut self) {
                crate::artifacts::set_test_artifact_sessions_root(self.0.take());
            }
        }
        let _restore = RestoreRoot(prior);

        let sentinel = "CW_EXACT_SENTINEL_7f4e";
        let mut raw = String::with_capacity(2_200_000);
        for index in 0..18_000 {
            raw.push_str(&format!("warning: dependency note {index:05}\n"));
        }
        raw.push_str(sentinel);
        raw.push('\n');
        raw.push_str("error[E0382]: borrow of moved value: evidence\n");
        while raw.len() < 2_100_000 {
            raw.push_str("build trace line with ordinary diagnostic context\n");
        }

        let store = new_shared_handle_store();
        // No Workflow-JS/RLM runtime or activation flag is constructed here:
        // this is the ordinary engine policy surface itself.
        let broker = LargeOutputBroker::new(
            WorkshopConfig::default(),
            "session-test",
            temp.path(),
            store.clone(),
            PressureLevel::Low,
            Some(128_000),
        );
        let mut small = ToolResult::success("small byte-for-byte result");
        assert_eq!(
            broker
                .project("small-call", "read_file", &json!({}), &mut small)
                .await,
            ProjectionOutcome::Inline
        );
        assert_eq!(small.content, "small byte-for-byte result");
        assert!(small.metadata.is_none());
        let mut first = ToolResult {
            content: raw.clone(),
            success: false,
            metadata: Some(json!({
                "exit_code": 101,
                "cargo_failure_summary": {
                    "primary_errors": ["error[E0382]: borrow of moved value: evidence"],
                    "final_error": "error: could not compile evidence-fixture"
                }
            })),
        };
        let projection_started = std::time::Instant::now();
        let outcome = broker
            .project("call-provider-uuid-a", "exec_shell", &json!({}), &mut first)
            .await;
        let first_useful_result_ms = projection_started.elapsed().as_millis();
        let ProjectionOutcome::Stored {
            path,
            bytes,
            model_bytes,
            sha256,
            ..
        } = outcome
        else {
            panic!("large failed shell result must be stored");
        };
        assert_eq!(bytes, raw.len() as u64);
        assert_eq!(model_bytes, first.content.len());
        assert!(
            model_bytes < raw.len() / 100,
            "root={model_bytes} stored={}",
            raw.len()
        );
        assert!(!first.content.contains(sentinel));
        assert!(first.content.contains("E0382"));
        assert!(!first.content.contains("session-test"));
        assert!(!first.content.contains("call-provider-uuid-a"));
        assert_eq!(std::fs::read_to_string(&path).expect("artifact"), raw);
        assert_eq!(
            crate::hashing::sha256_hex(std::fs::read(&path).expect("bytes")),
            sha256
        );

        let envelope: Value = serde_json::from_str(&first.content).expect("envelope");
        let handle = envelope["handle"].as_str().expect("opaque handle");
        assert!(handle.starts_with("output_"));
        let mut context = ToolContext::new(temp.path()).with_state_namespace("session-test");
        context.runtime.handle_store = store.clone();
        let search = HandleReadTool
            .execute(
                json!({"handle": handle, "search": {"query": sentinel}}),
                &context,
            )
            .await
            .expect("bounded artifact search");
        assert!(search.content.contains(sentinel));

        let mut second = ToolResult {
            content: raw.clone(),
            success: false,
            metadata: first.metadata.clone(),
        };
        let second_outcome = broker
            .project(
                "call-provider-uuid-b",
                "exec_shell",
                &json!({}),
                &mut second,
            )
            .await;
        assert!(matches!(second_outcome, ProjectionOutcome::Stored { .. }));
        let second_envelope: Value =
            serde_json::from_str(&second.content).expect("second envelope");
        assert_ne!(envelope["handle"], second_envelope["handle"]);
        assert_eq!(std::fs::read_to_string(path).expect("first remains"), raw);

        // Clear the in-memory store: the opaque, content-addressed alias still
        // resolves the canonical session file and verifies its full digest.
        *store.lock().await = crate::tools::handle::HandleStore::default();
        let resumed = HandleReadTool
            .execute(
                json!({"handle": handle, "range": {"start": 18001, "end": 18003}}),
                &context,
            )
            .await
            .expect("restart fallback");
        assert!(resumed.content.contains(sentinel));

        let metric_receipt = format!(
            "stored_bytes={bytes} root_visible_bytes={model_bytes} ratio={:.5} first_useful_result_ms={first_useful_result_ms} retrieval_operations=2",
            model_bytes as f64 / bytes as f64,
        );
        assert!(metric_receipt.contains("retrieval_operations=2"));
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)] // Serializes the process-global artifact-root override.
    async fn brokered_agent_result_keeps_stable_parent_summary_and_exact_child_projection() {
        let _root_guard = crate::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let temp = tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace-private");
        std::fs::create_dir_all(&workspace).unwrap();
        let prior =
            crate::artifacts::set_test_artifact_sessions_root(Some(temp.path().join("sessions")));
        struct RestoreRoot(Option<PathBuf>);
        impl Drop for RestoreRoot {
            fn drop(&mut self) {
                crate::artifacts::set_test_artifact_sessions_root(self.0.take());
            }
        }
        let _restore = RestoreRoot(prior);

        let raw_uuid = "550e8400-e29b-41d4-a716-446655440000";
        let raw_timestamp = "2026-07-20T08:03:42Z";
        let sentinel = "CW_PRIVATE_CHILD_TRANSCRIPT_SENTINEL";
        let raw = json!({
            "name": "evidence-audit",
            "agent_id": format!("agent_{raw_uuid}"),
            "status": "completed",
            "transcript_handle": {
                "kind": "var_handle",
                "session_id": format!("agent:agent_{raw_uuid}"),
                "name": "full_transcript"
            },
            "snapshot": {
                "agent_id": format!("agent_{raw_uuid}"),
                "agent_type": "review",
                "assignment": {
                    "objective": "Audit the adaptive evidence cutover independently."
                },
                "status": "Completed",
                "result": format!(
                    "Verified the canonical artifact and advised cargo test. Receipt at {}/receipts/audit.txt on {raw_timestamp}.",
                    workspace.display()
                ),
                "steps_taken": 9,
                "duration_ms": 4567
            },
            "private_transcript": format!(
                "{}\n{sentinel}\n{}",
                "private child exchange\n".repeat(4_000),
                "private child exchange\n".repeat(4_000)
            )
        })
        .to_string();
        let broker = LargeOutputBroker::new(
            WorkshopConfig {
                large_output_threshold_tokens: Some(100),
                ..WorkshopConfig::default()
            },
            "agent-parent-session",
            &workspace,
            new_shared_handle_store(),
            PressureLevel::Low,
            Some(128_000),
        );
        let mut result = ToolResult::success(raw.clone());

        let outcome = broker
            .project(
                "call-agent-provider-uuid",
                "agent",
                &json!({"action": "wait"}),
                &mut result,
            )
            .await;
        let ProjectionOutcome::Stored { path, .. } = outcome else {
            panic!("large agent result must be stored")
        };
        assert_eq!(std::fs::read_to_string(path).unwrap(), raw);
        let parent_context = crate::core::engine::compact_tool_result_for_context(
            "deepseek-v4-pro",
            "agent",
            &result,
        );
        assert_eq!(parent_context, result.content);
        assert!(crate::core::engine::is_adaptive_evidence_envelope(
            &parent_context
        ));
        assert!(!parent_context.contains(sentinel));
        assert!(!parent_context.contains(raw_uuid));
        assert!(!parent_context.contains(raw_timestamp));
        assert!(!parent_context.contains(&workspace.display().to_string()));
        assert!(
            parent_context.len() < 12_000,
            "{} bytes",
            parent_context.len()
        );

        let envelope: Value = serde_json::from_str(&parent_context).unwrap();
        let summary = envelope["structured_summary"]
            .as_str()
            .expect("bounded structured child summary");
        assert!(summary.contains("self-reports"), "{summary}");
        assert!(summary.contains("verify side effects"), "{summary}");
        assert!(summary.contains("read_file") && summary.contains("list_dir"));
        assert!(summary.contains("handle_read") && summary.contains("transcript_handle"));
        assert!(summary.contains("Audit the adaptive evidence cutover"));
        assert!(summary.contains("Verified the canonical artifact"));
        assert!(!summary.contains(raw_uuid));
        assert!(!summary.contains(raw_timestamp));
        assert!(!summary.contains(&workspace.display().to_string()));
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)] // Serializes the process-global artifact-root override.
    async fn critical_pressure_envelope_stays_under_two_kib() {
        let _root_guard = crate::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let temp = tempdir().expect("tempdir");
        let prior =
            crate::artifacts::set_test_artifact_sessions_root(Some(temp.path().join("sessions")));
        struct RestoreRoot(Option<PathBuf>);
        impl Drop for RestoreRoot {
            fn drop(&mut self) {
                crate::artifacts::set_test_artifact_sessions_root(self.0.take());
            }
        }
        let _restore = RestoreRoot(prior);
        let broker = LargeOutputBroker::new(
            WorkshopConfig {
                large_output_threshold_tokens: Some(1_024),
                ..WorkshopConfig::default()
            },
            "critical-session",
            temp.path(),
            new_shared_handle_store(),
            PressureLevel::Critical,
            Some(32_000),
        );
        let mut failed = result(
            &format!(
                "{}error: primary failure\n{}",
                "warning: noisy\n".repeat(5_000),
                "tail\n".repeat(5_000)
            ),
            false,
        );
        let outcome = broker
            .project("critical-call", "exec_shell", &json!({}), &mut failed)
            .await;
        assert!(matches!(outcome, ProjectionOutcome::Stored { .. }));
        assert!(
            failed.content.len() < 2_048,
            "{} bytes",
            failed.content.len()
        );
        assert!(crate::core::engine::is_adaptive_evidence_envelope(
            &failed.content
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)] // Serializes the process-global artifact-root override.
    async fn large_typed_execution_error_becomes_artifact_backed_failed_result() {
        let _root_guard = crate::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let temp = tempdir().expect("tempdir");
        let prior =
            crate::artifacts::set_test_artifact_sessions_root(Some(temp.path().join("sessions")));
        struct RestoreRoot(Option<PathBuf>);
        impl Drop for RestoreRoot {
            fn drop(&mut self) {
                crate::artifacts::set_test_artifact_sessions_root(self.0.take());
            }
        }
        let _restore = RestoreRoot(prior);
        let sentinel = "CW_PLUGIN_PRIVATE_SENTINEL";
        let error_text = format!(
            "{}\n{sentinel}\n{}",
            "plugin stderr\n".repeat(10_000),
            "plugin stderr\n".repeat(10_000)
        );
        let broker = LargeOutputBroker::new(
            WorkshopConfig {
                large_output_threshold_tokens: Some(100),
                ..WorkshopConfig::default()
            },
            "typed-error-session",
            temp.path(),
            new_shared_handle_store(),
            PressureLevel::Low,
            Some(128_000),
        );
        let mut terminal = Err(ToolError::execution_failed(error_text.clone()));

        let outcome = broker
            .project_terminal_result("plugin-call", "plugin_run", &json!({}), &mut terminal)
            .await;
        let ProjectionOutcome::Stored { path, .. } = outcome else {
            panic!("large execution error should be stored")
        };
        let normalized = terminal.expect("large error becomes metadata-bearing result");
        assert!(!normalized.success);
        assert!(crate::core::engine::is_adaptive_evidence_envelope(
            &normalized.content
        ));
        assert!(!normalized.content.contains(sentinel));
        assert_eq!(
            std::fs::read_to_string(path).expect("exact typed error"),
            error_text
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn artifact_write_failure_returns_original_bytes_inline() {
        let temp = tempdir().expect("tempdir");
        let raw = "CW_STORE_FAILURE_EXACT\n".repeat(10_000);
        let broker = LargeOutputBroker::new(
            WorkshopConfig {
                large_output_threshold_tokens: Some(1),
                ..WorkshopConfig::default()
            },
            "../invalid-session",
            temp.path(),
            new_shared_handle_store(),
            PressureLevel::Low,
            Some(128_000),
        );
        let mut result = ToolResult::error(raw.clone());

        let outcome = broker
            .project("store-failure", "exec_shell", &json!({}), &mut result)
            .await;

        assert!(matches!(outcome, ProjectionOutcome::Unavailable { .. }));
        assert_eq!(result.content, raw);
        assert_eq!(result.metadata.as_ref().unwrap()["truncated"], false);
        assert_eq!(
            result.metadata.as_ref().unwrap()["adaptive_evidence"]["inline_fallback"],
            true
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oversized_authority_error_keeps_typed_terminal_semantics() {
        let temp = tempdir().expect("tempdir");
        let broker = LargeOutputBroker::new(
            WorkshopConfig {
                large_output_threshold_tokens: Some(1),
                ..WorkshopConfig::default()
            },
            "authority-session",
            temp.path(),
            new_shared_handle_store(),
            PressureLevel::Low,
            Some(128_000),
        );
        let mut terminal = Err(ToolError::permission_denied("denied ".repeat(20_000)));

        let outcome = broker
            .project_terminal_result("denied-call", "exec_shell", &json!({}), &mut terminal)
            .await;

        assert_eq!(outcome, ProjectionOutcome::Inline);
        assert!(matches!(terminal, Err(ToolError::PermissionDenied { .. })));
    }
}
