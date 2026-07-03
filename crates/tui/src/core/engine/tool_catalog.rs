//! Deferred tool catalog operational helpers.
//!
//! The streaming turn loop owns when tools are offered or executed. This module
//! owns the operational side of the catalog — tool search, missing tool
//! suggestions, deferred tool hydration, and catalog consistency checks.
//! Catalog building, deferral, and filtering policy lives in
//! `tool_catalog_policy`.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::dependencies::ExternalTool;
use crate::mcp::McpPool;
use crate::models::Tool;
use crate::tools::spec::{ToolError, ToolResult, optional_str, optional_u64, required_str};

// Re-export policy items so engine.rs and other callers continue to import
// from `self::tool_catalog::*` unchanged (#3940).
pub(super) use super::tool_catalog_policy::{
    CODE_EXECUTION_TOOL_NAME, DEFAULT_ACTIVE_NATIVE_TOOLS, JS_EXECUTION_TOOL_NAME,
    MULTI_TOOL_PARALLEL_NAME, REQUEST_USER_INPUT_NAME, TOOL_SEARCH_MAX_RESULTS_LIMIT,
    TOOL_SEARCH_NAME, ToolSurfacePolicy, ensure_advanced_tooling, is_tool_search_tool,
};
#[cfg(test)]
pub(super) use super::tool_catalog_policy::{
    build_model_tool_catalog, build_model_tool_catalog_with_surface, filter_tool_catalog_for_gates,
    should_default_defer_tool,
};

// Items used within this module that are not re-exported to engine.rs.
use super::tool_catalog_policy::{
    CORE_ACTION_TOOL_FALLBACKS, CoreActionToolFallback, LEGACY_TOOL_SEARCH_BM25_NAME,
    LEGACY_TOOL_SEARCH_REGEX_NAME, TOOL_SEARCH_DEFAULT_MAX_RESULTS,
};

pub(super) fn initial_active_tools(catalog: &[Tool]) -> HashSet<String> {
    let mut active = HashSet::new();
    for tool in catalog {
        if !tool.defer_loading.unwrap_or(false) || is_tool_search_tool(&tool.name) {
            active.insert(tool.name.clone());
        }
    }
    if active.is_empty()
        && !catalog.is_empty()
        && let Some(first) = catalog.first()
    {
        active.insert(first.name.clone());
    }
    active
}

fn active_tool_list_from_catalog(catalog: &[Tool], active: &HashSet<String>) -> Vec<Tool> {
    // Two-pass for prefix-cache stability (#263). Always-loaded tools come
    // first in their stable catalog order; tools that started life deferred
    // and were activated mid-conversation by ToolSearch get appended at the
    // tail. Otherwise activating a deferred tool shifts every later tool's
    // byte offset and busts the cached prefix from that point onwards.
    let mut head: Vec<Tool> = Vec::new();
    let mut tail: Vec<Tool> = Vec::new();
    for tool in catalog {
        if !active.contains(&tool.name) {
            continue;
        }
        if tool.defer_loading.unwrap_or(false) {
            tail.push(tool.clone());
        } else {
            head.push(tool.clone());
        }
    }
    head.extend(tail);
    head
}

pub(super) fn active_tools_for_step(
    catalog: &[Tool],
    active: &HashSet<String>,
    force_update_plan: bool,
) -> Vec<Tool> {
    // DeepSeek reasoning models reject explicit named tool_choice forcing here,
    // so for obvious quick-plan asks we narrow the first-step tool surface to
    // update_plan instead.
    if force_update_plan {
        let forced: Vec<_> = catalog
            .iter()
            .filter(|tool| tool.name == "update_plan")
            .cloned()
            .collect();
        if !forced.is_empty() {
            return forced;
        }
    }

    active_tool_list_from_catalog(catalog, active)
}

fn tool_search_haystack(tool: &Tool) -> String {
    format!(
        "{}\n{}\n{}",
        tool.name.to_lowercase(),
        tool.description.to_lowercase(),
        tool.input_schema.to_string().to_lowercase()
    )
}

fn tool_search_fallback_haystack(fallback: CoreActionToolFallback) -> String {
    format!(
        "{}\n{}\n{}",
        fallback.name.to_lowercase(),
        fallback.description.to_lowercase(),
        fallback.unavailable_reason.to_lowercase()
    )
}

fn catalog_contains_tool(catalog: &[Tool], name: &str) -> bool {
    catalog.iter().any(|tool| tool.name == name)
}

fn unavailable_core_action_tools_with_regex(
    catalog: &[Tool],
    query: &str,
    max_results: usize,
) -> Result<Vec<CoreActionToolFallback>, ToolError> {
    if max_results == 0 {
        return Ok(Vec::new());
    }
    let regex = regex::Regex::new(query)
        .map_err(|err| ToolError::invalid_input(format!("Invalid regex query: {err}")))?;
    Ok(CORE_ACTION_TOOL_FALLBACKS
        .iter()
        .copied()
        .filter(|fallback| !catalog_contains_tool(catalog, fallback.name))
        .filter(|fallback| regex.is_match(&tool_search_fallback_haystack(*fallback)))
        .take(max_results)
        .collect())
}

fn unavailable_core_action_tools_with_bm25_like(
    catalog: &[Tool],
    query: &str,
    max_results: usize,
) -> Vec<CoreActionToolFallback> {
    if max_results == 0 {
        return Vec::new();
    }
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|term| term.trim().to_lowercase())
        .filter(|term| !term.is_empty())
        .collect();
    if terms.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(i64, CoreActionToolFallback)> = Vec::new();
    for fallback in CORE_ACTION_TOOL_FALLBACKS {
        if catalog_contains_tool(catalog, fallback.name) {
            continue;
        }
        let hay = tool_search_fallback_haystack(*fallback);
        let name = fallback.name.to_lowercase();
        let mut score = 0i64;
        for term in &terms {
            if hay.contains(term) {
                score += 1;
            }
            if name.contains(term) {
                score += 2;
            }
        }
        if score > 0 {
            scored.push((score, *fallback));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(b.1.name)));
    scored
        .into_iter()
        .take(max_results)
        .map(|(_, fallback)| fallback)
        .collect()
}

fn discover_tools_with_regex(
    catalog: &[Tool],
    query: &str,
    max_results: usize,
) -> Result<Vec<String>, ToolError> {
    let regex = regex::Regex::new(query)
        .map_err(|err| ToolError::invalid_input(format!("Invalid regex query: {err}")))?;

    let mut matches = Vec::new();
    for tool in catalog {
        if is_tool_search_tool(&tool.name) {
            continue;
        }
        let hay = tool_search_haystack(tool);
        if regex.is_match(&hay) {
            matches.push(tool.name.clone());
        }
        if matches.len() >= max_results {
            break;
        }
    }
    Ok(matches)
}

fn discover_tools_with_bm25_like(catalog: &[Tool], query: &str, max_results: usize) -> Vec<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|term| term.trim().to_lowercase())
        .filter(|term| !term.is_empty())
        .collect();
    if terms.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(i64, String)> = Vec::new();
    for tool in catalog {
        if is_tool_search_tool(&tool.name) {
            continue;
        }
        let hay = tool_search_haystack(tool);
        let mut score = 0i64;
        for term in &terms {
            if hay.contains(term) {
                score += 1;
            }
            if tool.name.to_lowercase().contains(term) {
                score += 2;
            }
        }
        if score > 0 {
            scored.push((score, tool.name.clone()));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(max_results)
        .map(|(_, name)| name)
        .collect()
}

fn edit_distance(a: &str, b: &str) -> usize {
    if a == b {
        return 0;
    }
    if a.is_empty() {
        return b.chars().count();
    }
    if b.is_empty() {
        return a.chars().count();
    }

    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut curr = vec![0usize; b_chars.len() + 1];

    for (i, a_ch) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, b_ch) in b_chars.iter().enumerate() {
            let cost = if a_ch == *b_ch { 0 } else { 1 };
            let delete = prev[j + 1] + 1;
            let insert = curr[j] + 1;
            let substitute = prev[j] + cost;
            curr[j + 1] = delete.min(insert).min(substitute);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b_chars.len()]
}

fn suggest_tool_names(catalog: &[Tool], requested: &str, limit: usize) -> Vec<String> {
    let requested = requested.trim().to_ascii_lowercase();
    if requested.is_empty() || limit == 0 {
        return Vec::new();
    }

    let mut candidates: Vec<(u8, usize, String)> = Vec::new();
    for tool in catalog {
        let candidate = tool.name.to_ascii_lowercase();
        let prefix_match = candidate.starts_with(&requested) || requested.starts_with(&candidate);
        let contains_match = candidate.contains(&requested) || requested.contains(&candidate);
        let distance = edit_distance(&candidate, &requested);
        let close_typo = distance <= 3;

        if !(prefix_match || contains_match || close_typo) {
            continue;
        }

        let rank = if prefix_match {
            0
        } else if contains_match {
            1
        } else {
            2
        };
        candidates.push((rank, distance, tool.name.clone()));
    }

    candidates.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    candidates.dedup_by(|a, b| a.2 == b.2);
    candidates
        .into_iter()
        .take(limit)
        .map(|(_, _, name)| name)
        .collect()
}

fn is_synthetic_catalog_tool(name: &str) -> bool {
    is_tool_search_tool(name)
        || matches!(name, CODE_EXECUTION_TOOL_NAME | JS_EXECUTION_TOOL_NAME)
        || McpPool::is_mcp_tool(name)
}

pub(super) fn tool_catalog_consistency_issues(
    catalog: &[Tool],
    registry: &crate::tools::ToolRegistry,
) -> Vec<String> {
    let catalog_names = catalog
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<HashSet<_>>();
    let registry_api_tools = registry.to_api_tools();
    let registry_model_visible_names = registry_api_tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<HashSet<_>>();
    let mut issues = Vec::new();

    for tool in catalog {
        if is_synthetic_catalog_tool(&tool.name) {
            continue;
        }
        if !registry.contains(&tool.name) {
            issues.push(format!(
                "catalog advertises '{}' but no registered handler exists",
                tool.name
            ));
        }
    }

    for name in DEFAULT_ACTIVE_NATIVE_TOOLS {
        if registry_model_visible_names.contains(name) && !catalog_names.contains(name) {
            issues.push(format!(
                "registered core tool '{name}' is missing from the model/search catalog"
            ));
        }
    }

    issues.sort();
    issues
}

pub(super) fn missing_tool_error_message(tool_name: &str, catalog: &[Tool]) -> String {
    let suggestions = suggest_tool_names(catalog, tool_name, 3);
    let shell_hint = if is_shell_tool_name(tool_name) {
        Some(shell_tool_allow_shell_hint())
    } else {
        None
    };
    if suggestions.is_empty() {
        if let Some(shell_hint) = shell_hint {
            return format!(
                "Tool '{tool_name}' is not available in the current tool catalog. \
                 {shell_hint}, or use {TOOL_SEARCH_NAME} with a short query."
            );
        }
        return format!(
            "Tool '{tool_name}' is not available in the current tool catalog. \
             Verify mode/feature flags, or use {TOOL_SEARCH_NAME} with a short query."
        );
    }

    let suggestion_text = format!("Did you mean: {}?", suggestions.join(", "));
    if let Some(shell_hint) = shell_hint {
        return format!(
            "Tool '{tool_name}' is not available in the current tool catalog. \
             {suggestion_text} {shell_hint}. \
             You can also use {TOOL_SEARCH_NAME} to discover tools."
        );
    }

    format!(
        "Tool '{tool_name}' is not available in the current tool catalog. \
         {suggestion_text} You can also use {TOOL_SEARCH_NAME} to discover tools."
    )
}

fn shell_tool_allow_shell_hint() -> &'static str {
    "Shell tools are absent because this session or profile disabled shell access, \
     commonly via top-level `allow_shell = false` or Plan mode. \
     Interactive Agent mode exposes shell by default with approval gating unless disabled. \
     Run `/config allow_shell true` for this session or add `--save` for future sessions; \
     the next turn will expose shell again"
}

fn is_shell_tool_name(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "exec_shell"
            | "exec_shell_wait"
            | "exec_shell_interact"
            | "task_shell_start"
            | "task_shell_wait"
    )
}

#[cfg(test)]
pub(super) fn maybe_activate_requested_deferred_tool(
    tool_name: &str,
    catalog: &[Tool],
    active_tools: &mut HashSet<String>,
) -> bool {
    let Some(def) = catalog.iter().find(|def| def.name == tool_name) else {
        return false;
    };

    if !def.defer_loading.unwrap_or(false) || active_tools.contains(tool_name) {
        return false;
    }

    active_tools.insert(tool_name.to_string())
}

pub(super) fn maybe_hydrate_requested_deferred_tool(
    tool_name: &str,
    tool_input: &Value,
    catalog: &[Tool],
    active_tools_at_batch_start: &HashSet<String>,
    hydrated_tools_this_batch: &mut HashSet<String>,
) -> Option<ToolResult> {
    let def = catalog.iter().find(|def| def.name == tool_name)?;

    if !def.defer_loading.unwrap_or(false) || active_tools_at_batch_start.contains(tool_name) {
        return None;
    }

    hydrated_tools_this_batch.insert(tool_name.to_string());
    Some(deferred_tool_schema_hydration_result(def, tool_input))
}

#[cfg(test)]
pub(super) fn preflight_requested_deferred_tool(
    tool_name: &str,
    tool_input: &Value,
    catalog: &[Tool],
    active_tools: &mut HashSet<String>,
) -> Option<ToolResult> {
    let active_tools_at_batch_start = active_tools.clone();
    let mut hydrated_tools_this_batch = HashSet::new();
    let result = maybe_hydrate_requested_deferred_tool(
        tool_name,
        tool_input,
        catalog,
        &active_tools_at_batch_start,
        &mut hydrated_tools_this_batch,
    );
    active_tools.extend(hydrated_tools_this_batch);
    result
}

fn deferred_tool_schema_hydration_result(tool: &Tool, tool_input: &Value) -> ToolResult {
    let expected = schema_fields(&tool.input_schema);
    let required = schema_required_fields(&tool.input_schema);
    let received = received_field_names(tool_input);
    let missing = required
        .iter()
        .filter(|field| !received.contains(field))
        .cloned()
        .collect::<Vec<_>>();
    let unexpected = received
        .iter()
        .filter(|field| !expected.iter().any(|expected| &expected.name == *field))
        .cloned()
        .collect::<Vec<_>>();
    let corrections = likely_field_corrections(&received, &expected, &tool.name);

    let mut lines = vec![
        format!("Tool `{}` was deferred and has now been loaded.", tool.name),
        String::new(),
        "The tool was not executed. Retry with the loaded schema.".to_string(),
        String::new(),
        "Expected fields:".to_string(),
    ];
    if expected.is_empty() {
        lines.push("  (none)".to_string());
    } else {
        for field in &expected {
            let required_marker = if required.contains(&field.name) {
                " required"
            } else {
                ""
            };
            lines.push(format!(
                "  {}: {}{}",
                field.name, field.kind, required_marker
            ));
        }
    }
    lines.push(String::new());
    lines.push("Received fields:".to_string());
    if received.is_empty() {
        lines.push("  (none)".to_string());
    } else {
        lines.push(format!("  {}", received.join(", ")));
    }
    if !missing.is_empty() {
        lines.push(String::new());
        lines.push("Missing required fields:".to_string());
        lines.push(format!("  {}", missing.join(", ")));
    }
    if !unexpected.is_empty() {
        lines.push(String::new());
        lines.push("Unexpected fields:".to_string());
        lines.push(format!("  {}", unexpected.join(", ")));
    }
    if !corrections.is_empty() {
        lines.push(String::new());
        lines.push("Likely corrections:".to_string());
        for correction in &corrections {
            lines.push(format!("  {correction}"));
        }
    }

    ToolResult::success(lines.join("\n")).with_metadata(json!({
        "event": "tool.schema_hydrated",
        "tool": tool.name,
        "executed": false,
        "retry_required": true,
        "reason": "deferred_tool_first_use",
        "deferred_tool_loaded": true,
        "tool_name": tool.name,
        "expected_fields": expected.iter().map(|field| field.name.clone()).collect::<Vec<_>>(),
        "received_fields": received,
        "missing_required_fields": missing,
        "unexpected_fields": unexpected,
        "likely_corrections": corrections,
    }))
}

#[derive(Debug, Clone)]
struct SchemaField {
    name: String,
    kind: String,
}

fn schema_fields(schema: &Value) -> Vec<SchemaField> {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut fields = properties
        .iter()
        .map(|(name, spec)| SchemaField {
            name: name.clone(),
            kind: schema_type_label(spec),
        })
        .collect::<Vec<_>>();
    fields.sort_by(|a, b| a.name.cmp(&b.name));
    fields
}

fn schema_required_fields(schema: &Value) -> Vec<String> {
    let mut required = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect::<Vec<_>>();
    required.sort();
    required
}

fn schema_type_label(spec: &Value) -> String {
    let Some(kind) = spec.get("type").and_then(Value::as_str) else {
        return "value".to_string();
    };
    if let Some(values) = spec.get("enum").and_then(Value::as_array) {
        let labels = values.iter().filter_map(Value::as_str).collect::<Vec<_>>();
        if !labels.is_empty() {
            return format!("{kind} ({})", labels.join(" | "));
        }
    }
    kind.to_string()
}

fn received_field_names(input: &Value) -> Vec<String> {
    let mut fields = input
        .as_object()
        .map(|object| object.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    fields.sort();
    fields
}

fn likely_field_corrections(
    received: &[String],
    expected: &[SchemaField],
    tool_name: &str,
) -> Vec<String> {
    let has_expected = |name: &str| expected.iter().any(|field| field.name == name);
    let has_received = |name: &str| received.iter().any(|field| field == name);
    let mut corrections = Vec::new();

    if has_received("old_string") && has_expected("search") {
        corrections.push("old_string -> search".to_string());
    } else if has_received("old_str") && has_expected("search") {
        corrections.push("old_str -> search".to_string());
    }
    if has_received("new_string") && has_expected("replace") {
        corrections.push("new_string -> replace".to_string());
    } else if has_received("new_str") && has_expected("replace") {
        corrections.push("new_str -> replace".to_string());
    } else if has_received("replacement") && has_expected("replace") {
        corrections.push("replacement -> replace".to_string());
    }
    if tool_name == "checklist_update" && has_received("todos") {
        corrections.push(
            "Use checklist_write to replace the full list, or retry checklist_update with id and status."
                .to_string(),
        );
    }
    // RLM source fields are easy to misname (#2659). rlm_open takes exactly one
    // of file_path / content / url / session_object; nudge common wrong names
    // toward those.
    if tool_name == "rlm_open" {
        for wrong in [
            "prompt",
            "resident_file",
            "text",
            "body",
            "path",
            "file",
            "source",
        ] {
            if has_received(wrong)
                && !has_received("file_path")
                && !has_received("content")
                && !has_received("url")
                && !has_received("session_object")
            {
                corrections.push(format!("{wrong} -> file_path (local file), content (inline text), url, or session_object"));
            }
        }
    }
    corrections
}

pub(super) fn execute_tool_search(
    tool_name: &str,
    input: &serde_json::Value,
    catalog: &[Tool],
    active_tools: &mut HashSet<String>,
) -> Result<ToolResult, ToolError> {
    let query = required_str(input, "query")?;
    let match_kind = match tool_name {
        LEGACY_TOOL_SEARCH_REGEX_NAME => "regex",
        LEGACY_TOOL_SEARCH_BM25_NAME => "bm25",
        _ => optional_str(input, "match").unwrap_or("bm25"),
    };
    if !matches!(match_kind, "bm25" | "regex") {
        return Err(ToolError::invalid_input(format!(
            "Unsupported match algorithm '{match_kind}'. Expected one of: bm25, regex"
        )));
    }
    let max_results = usize::try_from(optional_u64(
        input,
        "max_results",
        TOOL_SEARCH_DEFAULT_MAX_RESULTS as u64,
    ))
    .unwrap_or(TOOL_SEARCH_DEFAULT_MAX_RESULTS)
    .clamp(1, TOOL_SEARCH_MAX_RESULTS_LIMIT);
    let discovered = if match_kind == "regex" {
        discover_tools_with_regex(catalog, query, max_results)?
    } else {
        discover_tools_with_bm25_like(catalog, query, max_results)
    };
    let remaining_results = max_results.saturating_sub(discovered.len());
    let unavailable = if match_kind == "regex" {
        unavailable_core_action_tools_with_regex(catalog, query, remaining_results)?
    } else {
        unavailable_core_action_tools_with_bm25_like(catalog, query, remaining_results)
    };

    for name in &discovered {
        active_tools.insert(name.clone());
    }

    let references = discovered
        .iter()
        .map(|name| json!({"type": "tool_reference", "tool_name": name}))
        .collect::<Vec<_>>();
    let unavailable_references = unavailable
        .iter()
        .map(|fallback| {
            json!({
                "type": "unavailable_tool_reference",
                "tool_name": fallback.name,
                "reason": fallback.unavailable_reason,
            })
        })
        .collect::<Vec<_>>();

    let payload = json!({
        "type": "tool_search_tool_search_result",
        "tool_references": references,
        "unavailable_tool_references": unavailable_references.clone(),
    });

    Ok(ToolResult {
        content: serde_json::to_string(&payload).unwrap_or_else(|_| payload.to_string()),
        success: true,
        metadata: Some(json!({
            "tool_references": discovered,
            "unavailable_tool_references": unavailable_references,
        })),
    })
}

pub(super) async fn execute_code_execution_tool(
    input: &serde_json::Value,
    workspace: &Path,
) -> Result<ToolResult, ToolError> {
    let code = required_str(input, "code")?;

    // Resolve the locally-installed Python interpreter we cached at
    // catalog-build time. If it's absent now (somehow registered but
    // disappeared between startup and this call — concurrent uninstall,
    // PATH change, etc.) the ExternalTool::tokio_command() will return
    // None and we fail fast with a clear message.
    //
    // Write the code to a temp file and execute it as a script rather
    // than passing it via `-c "<code>"`. Reasons:
    //   * `-c` has length limits (argv) on Windows.
    //   * Multiline code with quote nesting is brittle through `-c`.
    //   * Tracebacks reference a real filename instead of `<string>`,
    //     so the model can interpret line numbers correctly.
    // Tempfile lives only for the duration of this execution; Drop
    // removes it. We use `.py` so any shebang / encoding-sniffer
    // logic in the interpreter behaves normally.
    let temp_dir = tempfile::tempdir()
        .map_err(|e| ToolError::execution_failed(format!("tempdir failed: {e}")))?;
    let script_path = temp_dir.path().join("code_execution.py");
    tokio::fs::write(&script_path, code)
        .await
        .map_err(|e| ToolError::execution_failed(format!("tempfile write failed: {e}")))?;

    let mut cmd = crate::dependencies::Python::tokio_command().ok_or_else(|| {
        ToolError::execution_failed(
            "code_execution: Python interpreter became unavailable".to_string(),
        )
    })?;
    cmd.arg(&script_path).current_dir(workspace);

    let output = tokio::time::timeout(Duration::from_secs(120), cmd.output())
        .await
        .map_err(|_| ToolError::Timeout { seconds: 120 })
        .and_then(|res| res.map_err(|e| ToolError::execution_failed(e.to_string())))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let return_code = output.status.code().unwrap_or(-1);
    let success = output.status.success();
    let payload = json!({
        "type": "code_execution_result",
        "stdout": stdout,
        "stderr": stderr,
        "return_code": return_code,
        "content": [],
    });

    Ok(ToolResult {
        content: serde_json::to_string(&payload).unwrap_or_else(|_| payload.to_string()),
        success,
        metadata: Some(payload),
    })
}
