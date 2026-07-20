//! Symbolic handle storage and bounded reads.
//!
//! `var_handle` is the shared protocol that lets expensive environments
//! (RLM sessions, sub-agent transcripts, large artifacts) hand the parent a
//! small symbolic reference instead of copying the whole payload into the
//! parent transcript.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

const DEFAULT_MAX_CHARS: usize = 12_000;
const HARD_MAX_CHARS: usize = 50_000;
#[allow(dead_code)] // Used by producers as they begin returning var_handle records.
const REPR_PREVIEW_CHARS: usize = 160;
const READ_CHUNK_BYTES: usize = 16 * 1024;
const DEFAULT_MAX_MATCHES: usize = 20;
const HARD_MAX_MATCHES: usize = 100;

pub type SharedHandleStore = Arc<Mutex<HandleStore>>;

#[must_use]
pub fn new_shared_handle_store() -> SharedHandleStore {
    Arc::new(Mutex::new(HandleStore::default()))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VarHandle {
    pub kind: String,
    pub session_id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub length: usize,
    pub repr_preview: String,
    pub sha256: String,
}

impl VarHandle {
    #[must_use]
    pub fn key(&self) -> HandleKey {
        HandleKey {
            session_id: self.session_id.clone(),
            name: self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HandleKey {
    pub session_id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct HandleRecord {
    pub handle: VarHandle,
    pub value: HandleValue,
}

/// Exact text owned by the session artifact store. Only metadata and a safe
/// preview remain in memory; projections stream from the canonical file.
#[derive(Debug, Clone)]
pub struct ArtifactTextBacking {
    pub relative_path: PathBuf,
    pub byte_length: u64,
    pub char_length: usize,
    pub line_count: Option<usize>,
    pub sha256: String,
}

#[allow(dead_code)] // Producers land in later v0.8.33 slices; handle_read is first.
#[derive(Debug, Clone)]
pub enum HandleValue {
    Text(String),
    Json(Value),
    ArtifactText(ArtifactTextBacking),
}

#[allow(dead_code)] // Foundation methods used by upcoming RLM/agent session producers.
impl HandleValue {
    fn length(&self) -> usize {
        match self {
            Self::Text(text) => text.chars().count(),
            Self::Json(Value::Array(items)) => items.len(),
            Self::Json(Value::Object(map)) => map.len(),
            Self::Json(value) => value.to_string().chars().count(),
            Self::ArtifactText(backing) => backing.char_length,
        }
    }

    fn type_name(&self) -> String {
        match self {
            Self::Text(_) => "str".to_string(),
            Self::Json(Value::Array(_)) => "list".to_string(),
            Self::Json(Value::Object(_)) => "dict".to_string(),
            Self::Json(Value::String(_)) => "str".to_string(),
            Self::Json(Value::Bool(_)) => "bool".to_string(),
            Self::Json(Value::Number(_)) => "number".to_string(),
            Self::Json(Value::Null) => "null".to_string(),
            Self::ArtifactText(_) => "str".to_string(),
        }
    }

    fn stable_bytes(&self) -> Vec<u8> {
        match self {
            Self::Text(text) => text.as_bytes().to_vec(),
            Self::Json(value) => serde_json::to_vec(value).unwrap_or_default(),
            Self::ArtifactText(backing) => backing.sha256.as_bytes().to_vec(),
        }
    }

    fn repr_preview(&self) -> String {
        match self {
            Self::Text(text) => truncate_chars(text, REPR_PREVIEW_CHARS),
            Self::Json(value) => truncate_chars(&value.to_string(), REPR_PREVIEW_CHARS),
            Self::ArtifactText(_) => String::new(),
        }
    }
}

#[derive(Debug, Default)]
pub struct HandleStore {
    records: HashMap<HandleKey, HandleRecord>,
}

#[allow(dead_code)] // Insertors are for producer tools; this PR wires the reader first.
impl HandleStore {
    #[must_use]
    pub fn insert_text(
        &mut self,
        session_id: impl Into<String>,
        name: impl Into<String>,
        text: impl Into<String>,
    ) -> VarHandle {
        self.insert(session_id, name, HandleValue::Text(text.into()))
    }

    #[must_use]
    pub fn insert_json(
        &mut self,
        session_id: impl Into<String>,
        name: impl Into<String>,
        value: Value,
    ) -> VarHandle {
        self.insert(session_id, name, HandleValue::Json(value))
    }

    #[must_use]
    pub fn get(&self, handle: &VarHandle) -> Option<&HandleRecord> {
        self.records.get(&handle.key())
    }

    #[must_use]
    pub fn insert_artifact_text(
        &mut self,
        session_id: impl Into<String>,
        name: impl Into<String>,
        backing: ArtifactTextBacking,
        safe_preview: impl Into<String>,
    ) -> VarHandle {
        let session_id = session_id.into();
        let name = name.into();
        let handle = VarHandle {
            kind: "var_handle".to_string(),
            session_id: session_id.clone(),
            name: name.clone(),
            type_name: "str".to_string(),
            length: backing.char_length,
            repr_preview: truncate_chars(&safe_preview.into(), REPR_PREVIEW_CHARS),
            sha256: backing.sha256.clone(),
        };
        self.records.insert(
            HandleKey { session_id, name },
            HandleRecord {
                handle: handle.clone(),
                value: HandleValue::ArtifactText(backing),
            },
        );
        handle
    }

    fn insert(
        &mut self,
        session_id: impl Into<String>,
        name: impl Into<String>,
        value: HandleValue,
    ) -> VarHandle {
        let session_id = session_id.into();
        let name = name.into();
        let handle = VarHandle {
            kind: "var_handle".to_string(),
            session_id: session_id.clone(),
            name: name.clone(),
            type_name: value.type_name(),
            length: value.length(),
            repr_preview: value.repr_preview(),
            sha256: sha256_hex(&value.stable_bytes()),
        };
        let key = HandleKey { session_id, name };
        self.records.insert(
            key,
            HandleRecord {
                handle: handle.clone(),
                value,
            },
        );
        handle
    }
}

pub struct HandleReadTool;

#[async_trait]
impl ToolSpec for HandleReadTool {
    fn name(&self) -> &'static str {
        "handle_read"
    }

    fn description(&self) -> &'static str {
        "Read a bounded projection from a var_handle returned by tools such \
         as ordinary tool calls, RLM sessions, or sub-agents. Opaque \
         `output_...` aliases resolve within the current session. This does not read artifact ids \
         (`art_...`), tool-call ids (`call_...`), SHA refs, or files; use \
         retrieve_tool_result for spilled tool results/artifacts and \
         read_file for workspace files. Provide \
         exactly one projection: `slice` for char/line slices, `range` for \
         one-based line ranges, `search` for bounded text matches, `count` for metadata counts, or `jsonpath` \
         for a small JSON-path projection. This retrieves from the handle's \
         backing environment instead of asking the parent transcript to hold \
         the full payload."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["handle"],
            "properties": {
                "handle": {
                    "description": "A var_handle object, or a compact `session_id/name` string. Not an `art_...`, `call_...`, SHA, or file path ref.",
                    "oneOf": [
                        {
                            "type": "object",
                            "required": ["kind", "session_id", "name"],
                            "properties": {
                                "kind": { "type": "string", "const": "var_handle" },
                                "session_id": { "type": "string" },
                                "name": { "type": "string" },
                                "type": { "type": "string" },
                                "length": { "type": "integer" },
                                "repr_preview": { "type": "string" },
                                "sha256": { "type": "string" }
                            }
                        },
                        { "type": "string" }
                    ]
                },
                "slice": {
                    "type": "object",
                    "description": "Zero-based half-open slice over chars or lines.",
                    "properties": {
                        "start": { "type": "integer", "minimum": 0 },
                        "end": { "type": "integer", "minimum": 0 },
                        "unit": { "type": "string", "enum": ["chars", "lines"], "default": "chars" }
                    }
                },
                "range": {
                    "type": "object",
                    "description": "One-based inclusive line range.",
                    "required": ["start", "end"],
                    "properties": {
                        "start": { "type": "integer", "minimum": 1 },
                        "end": { "type": "integer", "minimum": 1 }
                    }
                },
                "search": {
                    "type": "object",
                    "description": "Bounded substring search over text handles.",
                    "required": ["query"],
                    "properties": {
                        "query": { "type": "string", "maxLength": 512 },
                        "case_sensitive": { "type": "boolean", "default": false },
                        "max_matches": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }
                    }
                },
                "count": {
                    "type": "boolean",
                    "description": "Return counts for the handle payload."
                },
                "jsonpath": {
                    "type": "string",
                    "description": "Small JSONPath subset: $, .field, [index], [*], and ['field']."
                },
                "introspect": {
                    "type": "boolean",
                    "description": "Return supported projections, size hints, and copy-pasteable examples for this handle."
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Maximum characters to return in this projection. Defaults to 12000; hard-capped at 50000."
                }
            }
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let handle = parse_handle(
            input
                .get("handle")
                .ok_or_else(|| ToolError::missing_field("handle"))?,
            &context.state_namespace,
        )?;
        let projection = parse_projection(&input)?;
        let max_chars = input
            .get("max_chars")
            .and_then(Value::as_u64)
            .map(|n| (n as usize).min(HARD_MAX_CHARS))
            .unwrap_or(DEFAULT_MAX_CHARS);

        let record = {
            let store = context.runtime.handle_store.lock().await;
            store.get(&handle).cloned()
        }
        .or_else(|| artifact_record_after_restart(&handle, &context.state_namespace))
        .ok_or_else(|| {
            ToolError::invalid_input(format!(
                "handle_read: no payload found for handle {}/{}",
                handle.session_id, handle.name
            ))
        })?;
        if matches!(record.value, HandleValue::ArtifactText(_))
            && record.handle.session_id != context.state_namespace
        {
            return Err(ToolError::invalid_input(
                "handle_read: artifact-backed handles are scoped to the current session",
            ));
        }
        if !handle.sha256.is_empty() && handle.sha256 != record.handle.sha256 {
            return Err(ToolError::invalid_input(
                "handle_read: handle sha256 does not match stored payload",
            ));
        }

        let output = tokio::task::spawn_blocking(move || {
            projection_for_record(&record, &projection, max_chars)
        })
        .await
        .map_err(|error| {
            ToolError::execution_failed(format!("handle_read worker failed: {error}"))
        })??;

        ToolResult::json(&output).map_err(|e| ToolError::execution_failed(e.to_string()))
    }
}

#[derive(Debug, Clone, Copy)]
enum SliceUnit {
    Chars,
    Lines,
}

#[derive(Debug, Clone)]
enum Projection {
    Count,
    Slice {
        start: usize,
        end: Option<usize>,
        unit: SliceUnit,
    },
    Range {
        start: usize,
        end: usize,
    },
    Search {
        query: String,
        case_sensitive: bool,
        max_matches: usize,
    },
    JsonPath(String),
    Introspect,
}

fn parse_handle(value: &Value, current_namespace: &str) -> Result<VarHandle, ToolError> {
    if let Some(raw) = value.as_str() {
        if is_opaque_output_alias(raw) {
            return Ok(VarHandle {
                kind: "var_handle".to_string(),
                session_id: current_namespace.to_string(),
                name: raw.to_string(),
                type_name: String::new(),
                length: 0,
                repr_preview: String::new(),
                sha256: String::new(),
            });
        }
        if looks_like_tool_result_ref(raw) {
            return Err(ToolError::invalid_input(
                "handle_read only accepts var_handle objects or `session_id/name` strings. \
                 This looks like an artifact/tool-result ref; use `retrieve_tool_result` instead.",
            ));
        }
        let Some((session_id, name)) = raw.rsplit_once('/') else {
            return Err(ToolError::invalid_input(
                "handle_read: string handles must use `session_id/name`. \
                 For `art_...`, `call_...`, SHA, or file refs, use `retrieve_tool_result`.",
            ));
        };
        return Ok(VarHandle {
            kind: "var_handle".to_string(),
            session_id: session_id.to_string(),
            name: name.to_string(),
            type_name: String::new(),
            length: 0,
            repr_preview: String::new(),
            sha256: String::new(),
        });
    }

    let mut handle: VarHandle = serde_json::from_value(value.clone()).map_err(|e| {
        ToolError::invalid_input(format!("handle_read: invalid var_handle object: {e}"))
    })?;
    if handle.kind != "var_handle" {
        return Err(ToolError::invalid_input(
            "handle_read: handle.kind must be `var_handle`",
        ));
    }
    if handle.session_id.trim().is_empty() || handle.name.trim().is_empty() {
        return Err(ToolError::invalid_input(
            "handle_read: handle.session_id and handle.name must be non-empty",
        ));
    }
    if let Some(encoded_sha256) = opaque_output_sha256(&handle.name) {
        if !handle.sha256.is_empty() && handle.sha256 != encoded_sha256 {
            return Err(ToolError::invalid_input(
                "handle_read: handle sha256 does not match its opaque output alias",
            ));
        }
        handle.sha256 = encoded_sha256.to_string();
    }
    Ok(handle)
}

fn is_opaque_output_alias(value: &str) -> bool {
    opaque_output_sha256(value).is_some()
}

fn opaque_output_sha256(value: &str) -> Option<&str> {
    let digests = value.strip_prefix("output_")?;
    let (content, occurrence) = digests.split_once('_')?;
    (content.len() == 64
        && occurrence.len() == 12
        && content.chars().all(|ch| ch.is_ascii_hexdigit())
        && occurrence.chars().all(|ch| ch.is_ascii_hexdigit()))
    .then_some(content)
}

fn looks_like_tool_result_ref(raw: &str) -> bool {
    let trimmed = raw.trim();
    let sha_candidate = trimmed
        .strip_prefix("sha:")
        .or_else(|| trimmed.strip_prefix("sha_"))
        .unwrap_or(trimmed);
    trimmed.starts_with("art_")
        || trimmed.starts_with("call_")
        || trimmed.starts_with("tool_result:")
        || trimmed.ends_with(".txt")
        || crate::tools::truncate::is_valid_sha256(&sha_candidate.to_ascii_lowercase())
}

fn parse_projection(input: &Value) -> Result<Projection, ToolError> {
    let mut count = 0usize;
    count += usize::from(input.get("slice").is_some());
    count += usize::from(input.get("range").is_some());
    count += usize::from(input.get("search").is_some());
    count += usize::from(input.get("count").and_then(Value::as_bool).unwrap_or(false));
    count += usize::from(input.get("jsonpath").is_some());
    count += usize::from(
        input
            .get("introspect")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    );
    if count != 1 {
        return Err(ToolError::invalid_input(projection_usage_hint()));
    }

    if input
        .get("introspect")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(Projection::Introspect);
    }
    if input.get("count").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(Projection::Count);
    }
    if let Some(path) = input.get("jsonpath") {
        let path = path
            .as_str()
            .ok_or_else(|| ToolError::invalid_input("handle_read: jsonpath must be a string"))?
            .trim();
        if path.is_empty() {
            return Err(ToolError::invalid_input(
                "handle_read: jsonpath must not be empty",
            ));
        }
        return Ok(Projection::JsonPath(path.to_string()));
    }
    if let Some(search) = input.get("search") {
        let query = search
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::missing_field("search.query"))?
            .to_string();
        if query.is_empty() {
            return Err(ToolError::invalid_input(
                "handle_read: search.query must not be empty",
            ));
        }
        if query.chars().count() > 512 {
            return Err(ToolError::invalid_input(
                "handle_read: search.query must be at most 512 characters",
            ));
        }
        let case_sensitive = search
            .get("case_sensitive")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let max_matches = search
            .get("max_matches")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_MAX_MATCHES, |value| value as usize)
            .clamp(1, HARD_MAX_MATCHES);
        return Ok(Projection::Search {
            query,
            case_sensitive,
            max_matches,
        });
    }
    if let Some(slice) = input.get("slice") {
        let start = slice.get("start").and_then(Value::as_u64).unwrap_or(0) as usize;
        let end = slice.get("end").and_then(Value::as_u64).map(|n| n as usize);
        if let Some(end) = end
            && end < start
        {
            return Err(ToolError::invalid_input(
                "handle_read: slice.end must be greater than or equal to slice.start",
            ));
        }
        let unit = match slice.get("unit").and_then(Value::as_str).unwrap_or("chars") {
            "chars" => SliceUnit::Chars,
            "lines" => SliceUnit::Lines,
            other => {
                return Err(ToolError::invalid_input(format!(
                    "handle_read: unsupported slice.unit `{other}`"
                )));
            }
        };
        return Ok(Projection::Slice { start, end, unit });
    }
    let range = input
        .get("range")
        .ok_or_else(|| ToolError::invalid_input("handle_read: missing projection"))?;
    let start = range
        .get("start")
        .and_then(Value::as_u64)
        .ok_or_else(|| ToolError::missing_field("range.start"))? as usize;
    let end = range
        .get("end")
        .and_then(Value::as_u64)
        .ok_or_else(|| ToolError::missing_field("range.end"))? as usize;
    if start == 0 || end == 0 || end < start {
        return Err(ToolError::invalid_input(
            "handle_read: range is one-based inclusive and end must be >= start",
        ));
    }
    Ok(Projection::Range { start, end })
}

fn projection_usage_hint() -> String {
    "handle_read: provide exactly one projection: `slice`, `range`, `search`, `count: true`, `jsonpath`, or `introspect: true`. \
     Examples: {\"handle\":{\"kind\":\"var_handle\",\"session_id\":\"rlm:abc\",\"name\":\"final_1\"},\"slice\":{\"start\":0,\"end\":500}}; \
     {\"handle\":\"rlm:abc/final_1\",\"count\":true}; \
     {\"handle\":\"rlm:abc/final_1\",\"introspect\":true}."
        .to_string()
}

fn artifact_record_after_restart(
    handle: &VarHandle,
    current_namespace: &str,
) -> Option<HandleRecord> {
    if handle.session_id != current_namespace || !is_opaque_output_alias(&handle.name) {
        return None;
    }
    let relative_path =
        crate::artifacts::session_artifact_relative_path(&format!("art_{}", handle.name));
    let absolute =
        crate::artifacts::resolve_session_artifact_for_read(current_namespace, &relative_path)
            .ok()?;
    let byte_length = std::fs::metadata(absolute).ok()?.len();
    let sha256 = opaque_output_sha256(&handle.name)?.to_string();
    let fallback_handle = VarHandle {
        kind: "var_handle".to_string(),
        session_id: current_namespace.to_string(),
        name: handle.name.clone(),
        type_name: "str".to_string(),
        length: handle.length,
        repr_preview: String::new(),
        sha256: sha256.clone(),
    };
    Some(HandleRecord {
        handle: fallback_handle,
        value: HandleValue::ArtifactText(ArtifactTextBacking {
            relative_path,
            byte_length,
            char_length: handle.length,
            line_count: None,
            sha256,
        }),
    })
}

fn projection_for_record(
    record: &HandleRecord,
    projection: &Projection,
    max_chars: usize,
) -> Result<Value, ToolError> {
    let HandleValue::ArtifactText(backing) = &record.value else {
        return match projection {
            Projection::Count => Ok(count_projection(record)),
            Projection::Slice { start, end, unit } => {
                Ok(slice_projection(record, *start, *end, *unit, max_chars))
            }
            Projection::Range { start, end } => {
                Ok(line_range_projection(record, *start, *end, max_chars))
            }
            Projection::Search {
                query,
                case_sensitive,
                max_matches,
            } => Ok(memory_search_projection(
                record,
                query,
                *case_sensitive,
                *max_matches,
                max_chars,
            )),
            Projection::JsonPath(path) => jsonpath_projection(record, path, max_chars),
            Projection::Introspect => Ok(introspect_projection(record)),
        };
    };

    // Traverse and open the session tree once without following any path
    // component, then verify and project from that same descriptor.
    let mut file = crate::artifacts::open_session_artifact_for_read(
        &record.handle.session_id,
        &backing.relative_path,
    )
    .map_err(|error| artifact_read_error("unavailable", error))?;
    let stats = verify_artifact(&mut file, backing)
        .map_err(|error| artifact_read_error("could not be verified", error))?;

    match projection {
        Projection::Count => Ok(json!({
            "handle": record.handle.name,
            "projection": "count",
            "chars": stats.chars,
            "lines": stats.lines,
            "bytes": stats.bytes,
            "sha256": stats.sha256,
        })),
        Projection::Slice { start, end, unit } => match unit {
            SliceUnit::Chars => {
                let end = end.unwrap_or(stats.chars).min(stats.chars);
                let start = (*start).min(stats.chars);
                let content = stream_char_slice(&mut file, start, end, max_chars)
                    .map_err(|error| artifact_read_error("could not be read", error))?;
                let shown = content.chars().count();
                Ok(json!({
                    "handle": record.handle.name,
                    "projection": "slice",
                    "content": content,
                    "truncated": shown < end.saturating_sub(start),
                    "shown_chars": shown,
                    "omitted_chars": end.saturating_sub(start).saturating_sub(shown),
                    "meta": {"unit": "chars", "start": start, "end": end, "total_chars": stats.chars},
                }))
            }
            SliceUnit::Lines => artifact_line_projection(
                record,
                &mut file,
                "slice",
                (*start).min(stats.lines),
                end.unwrap_or(stats.lines).min(stats.lines),
                max_chars,
                json!({
                    "unit": "lines",
                    "start": (*start).min(stats.lines),
                    "end": end.unwrap_or(stats.lines).min(stats.lines),
                    "total_lines": stats.lines,
                }),
            ),
        },
        Projection::Range { start, end } => artifact_line_projection(
            record,
            &mut file,
            "range",
            start.saturating_sub(1).min(stats.lines),
            (*end).min(stats.lines),
            max_chars,
            json!({
                "start": start,
                "end": end,
                "shown_start": start.saturating_sub(1).min(stats.lines).saturating_add(1),
                "shown_end": (*end).min(stats.lines),
                "total_lines": stats.lines,
            }),
        ),
        Projection::Search {
            query,
            case_sensitive,
            max_matches,
        } => artifact_search_projection(
            record,
            &mut file,
            query,
            *case_sensitive,
            *max_matches,
            max_chars,
        ),
        Projection::JsonPath(_) => Err(ToolError::invalid_input(
            "handle_read: jsonpath projection requires a JSON handle",
        )),
        Projection::Introspect => Ok(artifact_introspect_projection(record, &stats)),
    }
}

fn artifact_read_error(action: &str, error: io::Error) -> ToolError {
    ToolError::execution_failed(format!("handle_read: exact evidence is {action}: {error}"))
}

struct ArtifactStats {
    bytes: u64,
    chars: usize,
    lines: usize,
    sha256: String,
}

#[cfg(test)]
fn open_artifact_no_follow(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?
    };
    #[cfg(not(unix))]
    let file = {
        if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "artifact symlink refused",
            ));
        }
        File::open(path)?
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "artifact is not a regular file",
        ));
    }
    Ok(file)
}

fn verify_artifact(file: &mut File, backing: &ArtifactTextBacking) -> io::Result<ArtifactStats> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut bytes = 0u64;
    let mut chars = 0usize;
    let mut newlines = 0usize;
    let mut saw_byte = false;
    let mut ended_in_newline = false;
    let mut pending = Vec::with_capacity(READ_CHUNK_BYTES + 4);
    let mut chunk = [0u8; READ_CHUNK_BYTES];
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        saw_byte = true;
        ended_in_newline = chunk[read - 1] == b'\n';
        bytes = bytes.saturating_add(read as u64);
        hasher.update(&chunk[..read]);
        newlines = newlines.saturating_add(chunk[..read].iter().filter(|&&b| b == b'\n').count());
        pending.extend_from_slice(&chunk[..read]);
        let valid_up_to = match std::str::from_utf8(&pending) {
            Ok(text) => {
                chars = chars.saturating_add(text.chars().count());
                pending.clear();
                continue;
            }
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
        };
        let text = std::str::from_utf8(&pending[..valid_up_to])
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        chars = chars.saturating_add(text.chars().count());
        pending.drain(..valid_up_to);
    }
    if !pending.is_empty() {
        let text = std::str::from_utf8(&pending)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        chars = chars.saturating_add(text.chars().count());
    }
    let sha256 = crate::hashing::hex_bytes(hasher.finalize());
    if bytes != backing.byte_length {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "byte length changed (expected {}, found {bytes})",
                backing.byte_length
            ),
        ));
    }
    if !backing.sha256.is_empty() && sha256 != backing.sha256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sha256 mismatch",
        ));
    }
    let lines = if saw_byte {
        newlines + usize::from(!ended_in_newline)
    } else {
        0
    };
    if backing.line_count.is_some_and(|expected| expected != lines) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "line count changed",
        ));
    }
    Ok(ArtifactStats {
        bytes,
        chars,
        lines,
        sha256,
    })
}

fn stream_chars(file: &mut File, mut visit: impl FnMut(char) -> bool) -> io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    let mut pending = Vec::with_capacity(READ_CHUNK_BYTES + 4);
    let mut chunk = [0u8; READ_CHUNK_BYTES];
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        pending.extend_from_slice(&chunk[..read]);
        let valid_up_to = match std::str::from_utf8(&pending) {
            Ok(text) => {
                for ch in text.chars() {
                    if !visit(ch) {
                        return Ok(());
                    }
                }
                pending.clear();
                continue;
            }
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
        };
        let text = std::str::from_utf8(&pending[..valid_up_to])
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        for ch in text.chars() {
            if !visit(ch) {
                return Ok(());
            }
        }
        pending.drain(..valid_up_to);
    }
    if !pending.is_empty() {
        let text = std::str::from_utf8(&pending)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        for ch in text.chars() {
            if !visit(ch) {
                break;
            }
        }
    }
    Ok(())
}

fn stream_char_slice(
    file: &mut File,
    start: usize,
    end: usize,
    max_chars: usize,
) -> io::Result<String> {
    let mut index = 0usize;
    let mut shown = 0usize;
    let mut output = String::new();
    stream_chars(file, |ch| {
        if index >= start && index < end && shown < max_chars {
            output.push(ch);
            shown += 1;
        }
        index = index.saturating_add(1);
        index < end && shown < max_chars
    })?;
    Ok(output)
}

fn artifact_line_projection(
    record: &HandleRecord,
    file: &mut File,
    projection: &str,
    start: usize,
    end: usize,
    max_chars: usize,
    meta: Value,
) -> Result<Value, ToolError> {
    let mut line = 0usize;
    let mut shown = 0usize;
    let mut content = String::new();
    stream_chars(file, |ch| {
        if line >= end || shown >= max_chars {
            return false;
        }
        if ch == '\n' {
            if line >= start && line + 1 < end && shown < max_chars {
                content.push('\n');
                shown += 1;
            }
            line = line.saturating_add(1);
        } else if line >= start && line < end {
            content.push(ch);
            shown += 1;
        }
        true
    })
    .map_err(|error| artifact_read_error("could not be read", error))?;
    Ok(json!({
        "handle": record.handle.name,
        "projection": projection,
        "content": content,
        "truncated": shown >= max_chars && line < end,
        "shown_chars": shown,
        "meta": meta,
    }))
}

fn artifact_search_projection(
    record: &HandleRecord,
    file: &mut File,
    query: &str,
    case_sensitive: bool,
    max_matches: usize,
    max_chars: usize,
) -> Result<Value, ToolError> {
    use std::collections::VecDeque;
    let needle = if case_sensitive {
        query.as_bytes().to_vec()
    } else {
        query
            .bytes()
            .map(|byte| byte.to_ascii_lowercase())
            .collect()
    };
    file.seek(SeekFrom::Start(0))
        .map_err(|error| artifact_read_error("could not be searched", error))?;
    let mut chunk = [0u8; READ_CHUNK_BYTES];
    let mut window = VecDeque::with_capacity(needle.len());
    let mut match_offsets = Vec::new();
    let mut offset = 0u64;
    let mut line = 1usize;
    'read: loop {
        let read = file
            .read(&mut chunk)
            .map_err(|error| artifact_read_error("could not be searched", error))?;
        if read == 0 {
            break;
        }
        for byte in &chunk[..read] {
            let comparable = if case_sensitive {
                *byte
            } else {
                byte.to_ascii_lowercase()
            };
            window.push_back(comparable);
            if window.len() > needle.len() {
                window.pop_front();
            }
            if window.len() == needle.len() && window.iter().copied().eq(needle.iter().copied()) {
                match_offsets.push((
                    line,
                    offset.saturating_add(1).saturating_sub(needle.len() as u64),
                ));
                if match_offsets.len() >= max_matches {
                    break 'read;
                }
            }
            if *byte == b'\n' {
                line = line.saturating_add(1);
                window.clear();
            }
            offset = offset.saturating_add(1);
        }
    }
    let found_count = match_offsets.len();
    let mut matches = Vec::new();
    let mut used_chars = 0usize;
    for (line, byte_offset) in match_offsets {
        if used_chars >= max_chars {
            break;
        }
        let remaining = max_chars.saturating_sub(used_chars);
        let excerpt = bounded_file_excerpt(file, byte_offset, needle.len(), 200)
            .map(|excerpt| truncate_chars(&excerpt, remaining))
            .unwrap_or_else(|_| truncate_chars("(excerpt unavailable)", remaining));
        used_chars = used_chars.saturating_add(excerpt.chars().count());
        matches.push(json!({
            "line": line,
            "byte_offset": byte_offset,
            "excerpt": excerpt,
        }));
    }
    let match_count = matches.len();
    Ok(json!({
        "handle": record.handle.name,
        "projection": "search",
        "query": query,
        "matches": matches,
        "match_count": match_count,
        "truncated": found_count >= max_matches || match_count < found_count || used_chars >= max_chars,
    }))
}

fn bounded_file_excerpt(
    file: &mut File,
    byte_offset: u64,
    match_bytes: usize,
    max_bytes: usize,
) -> io::Result<String> {
    let before = max_bytes.saturating_sub(match_bytes).div_ceil(2);
    let start = byte_offset.saturating_sub(before as u64);
    file.seek(SeekFrom::Start(start))?;
    let mut buffer = vec![0u8; max_bytes.max(match_bytes)];
    let read = file.read(&mut buffer)?;
    buffer.truncate(read);
    Ok(String::from_utf8_lossy(&buffer).replace(['\n', '\r'], " "))
}

fn memory_search_projection(
    record: &HandleRecord,
    query: &str,
    case_sensitive: bool,
    max_matches: usize,
    max_chars: usize,
) -> Value {
    let text = record_text(record);
    let needle = if case_sensitive {
        query.to_string()
    } else {
        query.to_ascii_lowercase()
    };
    let mut matches = Vec::new();
    let mut used = 0usize;
    for (index, line) in text.lines().enumerate() {
        let haystack = if case_sensitive {
            line.to_string()
        } else {
            line.to_ascii_lowercase()
        };
        if haystack.contains(&needle) && matches.len() < max_matches && used < max_chars {
            let excerpt = truncate_chars(line, (max_chars - used).min(500));
            used += excerpt.chars().count();
            matches.push(json!({"line": index + 1, "excerpt": excerpt}));
        }
    }
    json!({
        "handle": record.handle,
        "projection": "search",
        "query": query,
        "match_count": matches.len(),
        "matches": matches,
        "truncated": matches.len() >= max_matches || used >= max_chars,
    })
}

fn artifact_introspect_projection(record: &HandleRecord, stats: &ArtifactStats) -> Value {
    json!({
        "handle": record.handle.name,
        "projection": "introspect",
        "value_type": "text",
        "length": stats.chars,
        "bytes": stats.bytes,
        "projections": ["count", "slice_chars", "range_lines", "search"],
    })
}

fn count_projection(record: &HandleRecord) -> Value {
    match &record.value {
        HandleValue::Text(text) => json!({
            "handle": record.handle,
            "projection": "count",
            "chars": text.chars().count(),
            "lines": text.lines().count(),
            "bytes": text.len(),
        }),
        HandleValue::Json(value) => {
            let bytes = {
                let mut cw = crate::utils::CountingWriter::new();
                let _ = serde_json::to_writer(&mut cw, value);
                cw.count()
            };
            json!({
                "handle": record.handle,
                "projection": "count",
                "json_type": json_type(value),
                "length": record.handle.length,
                "bytes": bytes,
            })
        }
        HandleValue::ArtifactText(_) => unreachable!("artifact projections use the streaming path"),
    }
}

fn introspect_projection(record: &HandleRecord) -> Value {
    let string_handle = format!("{}/{}", record.handle.session_id, record.handle.name);
    let object_handle = json!(record.handle.clone());
    let mut projections = vec![
        json!({"name": "count", "example": {"handle": string_handle, "count": true}}),
        json!({"name": "slice_chars", "example": {"handle": object_handle.clone(), "slice": {"start": 0, "end": 500}}}),
        json!({"name": "range_lines", "example": {"handle": object_handle.clone(), "range": {"start": 1, "end": 20}}}),
    ];
    if matches!(record.value, HandleValue::Json(_)) {
        projections.push(
            json!({"name": "jsonpath", "example": {"handle": object_handle, "jsonpath": "$"}}),
        );
    }

    json!({
        "handle": record.handle,
        "projection": "introspect",
        "value_type": match &record.value {
            HandleValue::Text(_) => "text",
            HandleValue::Json(value) => json_type(value),
            HandleValue::ArtifactText(_) => "text",
        },
        "length": record.handle.length,
        "repr_preview": record.handle.repr_preview,
        "projections": projections,
    })
}

fn slice_projection(
    record: &HandleRecord,
    start: usize,
    end: Option<usize>,
    unit: SliceUnit,
    max_chars: usize,
) -> Value {
    let text = record_text(record);
    match unit {
        SliceUnit::Chars => {
            let total = text.chars().count();
            let end = end.unwrap_or(total).min(total);
            let raw = char_slice(&text, start.min(total), end);
            bounded_text_projection(
                record,
                "slice",
                raw,
                max_chars,
                json!({
                    "unit": "chars",
                    "start": start.min(total),
                    "end": end,
                    "total_chars": total,
                }),
            )
        }
        SliceUnit::Lines => {
            let lines: Vec<&str> = text.lines().collect();
            let total = lines.len();
            let end = end.unwrap_or(total).min(total);
            let raw = if start >= end {
                String::new()
            } else {
                lines[start.min(total)..end].join("\n")
            };
            bounded_text_projection(
                record,
                "slice",
                raw,
                max_chars,
                json!({
                    "unit": "lines",
                    "start": start.min(total),
                    "end": end,
                    "total_lines": total,
                }),
            )
        }
    }
}

fn line_range_projection(
    record: &HandleRecord,
    start: usize,
    end: usize,
    max_chars: usize,
) -> Value {
    let text = record_text(record);
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let zero_start = start.saturating_sub(1).min(total);
    let zero_end = end.min(total);
    let raw = if zero_start >= zero_end {
        String::new()
    } else {
        lines[zero_start..zero_end].join("\n")
    };
    bounded_text_projection(
        record,
        "range",
        raw,
        max_chars,
        json!({
            "start": start,
            "end": end,
            "shown_start": zero_start + 1,
            "shown_end": zero_end,
            "total_lines": total,
        }),
    )
}

fn jsonpath_projection(
    record: &HandleRecord,
    path: &str,
    max_chars: usize,
) -> Result<Value, ToolError> {
    let HandleValue::Json(value) = &record.value else {
        return Err(ToolError::invalid_input(
            "handle_read: jsonpath projection requires a JSON handle",
        ));
    };
    let matches = query_jsonpath(value, path)
        .map_err(|e| ToolError::invalid_input(format!("handle_read: {e}")))?;
    let mut payload = json!({
        "handle": record.handle,
        "projection": "jsonpath",
        "jsonpath": path,
        "count": matches.len(),
        "matches": matches,
        "truncated": false,
    });
    let rendered = serde_json::to_string(&payload).unwrap_or_default();
    if rendered.chars().count() > max_chars {
        payload["matches"] = json!([]);
        payload["preview"] = json!(truncate_chars(&rendered, max_chars));
        payload["truncated"] = json!(true);
    }
    Ok(payload)
}

fn bounded_text_projection(
    record: &HandleRecord,
    projection: &str,
    raw: String,
    max_chars: usize,
    extra: Value,
) -> Value {
    let raw_chars = raw.chars().count();
    let content = truncate_chars(&raw, max_chars);
    let shown_chars = content.chars().count();
    json!({
        "handle": record.handle,
        "projection": projection,
        "content": content,
        "truncated": shown_chars < raw_chars,
        "shown_chars": shown_chars,
        "omitted_chars": raw_chars.saturating_sub(shown_chars),
        "meta": extra,
    })
}

fn record_text(record: &HandleRecord) -> std::borrow::Cow<'_, str> {
    match &record.value {
        HandleValue::Text(text) => std::borrow::Cow::Borrowed(text),
        HandleValue::Json(value) => {
            std::borrow::Cow::Owned(serde_json::to_string_pretty(value).unwrap_or_default())
        }
        HandleValue::ArtifactText(_) => {
            unreachable!("artifact projections use the streaming path")
        }
    }
}

pub(crate) fn query_jsonpath(root: &Value, path: &str) -> Result<Vec<Value>, String> {
    if !path.starts_with('$') {
        return Err("jsonpath must start with `$`".to_string());
    }
    let mut idx = 1usize;
    let bytes = path.as_bytes();
    let mut current = vec![root];
    while idx < bytes.len() {
        match bytes[idx] {
            b'.' => {
                idx += 1;
                if idx < bytes.len() && bytes[idx] == b'.' {
                    return Err("recursive descent (`..`) is not supported".to_string());
                }
                let start = idx;
                while idx < bytes.len()
                    && (bytes[idx].is_ascii_alphanumeric() || bytes[idx] == b'_')
                {
                    idx += 1;
                }
                if start == idx {
                    return Err("expected field name after `.`".to_string());
                }
                let field = &path[start..idx];
                current = current
                    .into_iter()
                    .filter_map(|value| value.get(field))
                    .collect();
            }
            b'[' => {
                let Some(close_rel) = path[idx + 1..].find(']') else {
                    return Err("unterminated `[` segment".to_string());
                };
                let close = idx + 1 + close_rel;
                let token = path[idx + 1..close].trim();
                idx = close + 1;
                current = apply_bracket_token(current, token)?;
            }
            other => {
                return Err(format!(
                    "unexpected character `{}` in jsonpath",
                    other as char
                ));
            }
        }
    }
    Ok(current.into_iter().cloned().collect())
}

fn apply_bracket_token<'a>(values: Vec<&'a Value>, token: &str) -> Result<Vec<&'a Value>, String> {
    if token == "*" {
        let mut out = Vec::new();
        for value in values {
            match value {
                Value::Array(items) => out.extend(items),
                Value::Object(map) => out.extend(map.values()),
                _ => {}
            }
        }
        return Ok(out);
    }

    if let Some(field) = quoted_field(token) {
        return Ok(values
            .into_iter()
            .filter_map(|value| value.get(field))
            .collect());
    }

    let index = token
        .parse::<usize>()
        .map_err(|_| format!("unsupported bracket token `{token}`"))?;
    Ok(values
        .into_iter()
        .filter_map(|value| value.as_array().and_then(|items| items.get(index)))
        .collect())
}

fn quoted_field(token: &str) -> Option<&str> {
    if token.len() < 2 {
        return None;
    }
    let bytes = token.as_bytes();
    let quote = bytes[0];
    if !matches!(quote, b'\'' | b'"') || bytes[token.len() - 1] != quote {
        return None;
    }
    Some(&token[1..token.len() - 1])
}

fn char_slice(text: &str, start: usize, end: usize) -> String {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in text.chars().enumerate() {
        if idx == max_chars {
            break;
        }
        out.push(ch);
    }
    out
}

#[allow(dead_code)] // Used when producer tools register handle payloads.
fn sha256_hex(bytes: &[u8]) -> String {
    crate::hashing::sha256_hex(bytes)
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> ToolContext {
        ToolContext::new(".")
    }

    #[test]
    fn verified_projection_uses_same_open_artifact_snapshot() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("evidence.txt");
        let moved = temp.path().join("original.txt");
        let original = "verified original bytes";
        let replacement = "unverified replacement";
        std::fs::write(&path, original).expect("write original");
        let backing = ArtifactTextBacking {
            relative_path: PathBuf::from("artifacts/evidence.txt"),
            byte_length: original.len() as u64,
            char_length: original.chars().count(),
            line_count: Some(1),
            sha256: crate::hashing::sha256_hex(original.as_bytes()),
        };
        let mut file = open_artifact_no_follow(&path).expect("open exact artifact");
        verify_artifact(&mut file, &backing).expect("verify original descriptor");

        std::fs::rename(&path, &moved).expect("replace verified path");
        std::fs::write(&path, replacement).expect("write path replacement");
        let projected = stream_char_slice(&mut file, 0, original.len(), 1_000)
            .expect("project verified descriptor");

        assert_eq!(projected, original);
        assert_ne!(projected, replacement);
    }

    #[cfg(unix)]
    #[test]
    fn artifact_leaf_open_refuses_symlink() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target.txt");
        let link = temp.path().join("evidence.txt");
        std::fs::write(&target, "secret").expect("write target");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");

        assert!(open_artifact_no_follow(&link).is_err());
    }

    #[tokio::test]
    async fn handle_read_slices_text_by_chars() {
        let ctx = ctx();
        let handle = {
            let mut store = ctx.runtime.handle_store.lock().await;
            store.insert_text("rlm:test", "matches", "abcdef")
        };

        let result = HandleReadTool
            .execute(
                json!({"handle": handle, "slice": {"start": 1, "end": 4}}),
                &ctx,
            )
            .await
            .expect("execute");
        let body: Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(body["content"], "bcd");
        assert_eq!(body["truncated"], false);
    }

    #[tokio::test]
    async fn handle_read_ranges_text_by_one_based_lines() {
        let ctx = ctx();
        let handle = {
            let mut store = ctx.runtime.handle_store.lock().await;
            store.insert_text("agent:test", "transcript", "one\ntwo\nthree\nfour")
        };

        let result = HandleReadTool
            .execute(
                json!({"handle": handle, "range": {"start": 2, "end": 3}}),
                &ctx,
            )
            .await
            .expect("execute");
        let body: Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(body["content"], "two\nthree");
        assert_eq!(body["meta"]["shown_start"], 2);
        assert_eq!(body["meta"]["shown_end"], 3);
    }

    #[tokio::test]
    async fn handle_read_counts_json_collections() {
        let ctx = ctx();
        let handle = {
            let mut store = ctx.runtime.handle_store.lock().await;
            store.insert_json("rlm:test", "items", json!([{"a": 1}, {"a": 2}]))
        };

        let result = HandleReadTool
            .execute(json!({"handle": handle, "count": true}), &ctx)
            .await
            .expect("execute");
        let body: Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(body["json_type"], "array");
        assert_eq!(body["length"], 2);
    }

    #[tokio::test]
    async fn handle_read_introspects_object_handle_with_examples() {
        let ctx = ctx();
        let handle = {
            let mut store = ctx.runtime.handle_store.lock().await;
            store.insert_json("rlm:test", "items", json!({"items": [{"a": 1}]}))
        };

        let result = HandleReadTool
            .execute(json!({"handle": handle, "introspect": true}), &ctx)
            .await
            .expect("execute");
        let body: Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(body["projection"], "introspect");
        assert_eq!(body["handle"]["kind"], "var_handle");
        assert!(
            body["projections"]
                .as_array()
                .expect("projection examples")
                .iter()
                .any(|entry| entry["name"] == "jsonpath"),
            "json handles should advertise jsonpath examples"
        );
    }

    #[tokio::test]
    async fn handle_read_projects_jsonpath_subset() {
        let ctx = ctx();
        let handle = {
            let mut store = ctx.runtime.handle_store.lock().await;
            store.insert_json(
                "rlm:test",
                "items",
                json!({"items": [{"name": "a"}, {"name": "b"}]}),
            )
        };

        let result = HandleReadTool
            .execute(
                json!({"handle": handle, "jsonpath": "$.items[*].name"}),
                &ctx,
            )
            .await
            .expect("execute");
        let body: Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(body["matches"], json!(["a", "b"]));
        assert_eq!(body["count"], 2);
    }

    #[tokio::test]
    async fn handle_read_rejects_unbounded_projection_requests() {
        let ctx = ctx();
        let handle = {
            let mut store = ctx.runtime.handle_store.lock().await;
            store.insert_text("rlm:test", "body", "abc")
        };

        let err = HandleReadTool
            .execute(json!({"handle": handle}), &ctx)
            .await
            .expect_err("projection required");
        let message = err.to_string();
        assert!(message.contains("exactly one"));
        assert!(message.contains("slice"));
        assert!(message.contains("introspect"));
    }

    #[tokio::test]
    async fn handle_read_points_artifact_refs_to_tool_result_retrieval() {
        let ctx = ctx();
        let err = HandleReadTool
            .execute(json!({"handle": "art_call_abc123", "count": true}), &ctx)
            .await
            .expect_err("artifact refs are not var handles");
        let message = err.to_string();
        assert!(message.contains("retrieve_tool_result"));
        assert!(message.contains("artifact/tool-result ref"));
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)] // Serializes the process-global artifact-root override.
    async fn artifact_handles_are_session_scoped_and_fail_closed_after_restart() {
        let _root_guard = crate::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let prior =
            crate::artifacts::set_test_artifact_sessions_root(Some(temp.path().join("sessions")));
        struct RestoreRoot(Option<PathBuf>);
        impl Drop for RestoreRoot {
            fn drop(&mut self) {
                crate::artifacts::set_test_artifact_sessions_root(self.0.take());
            }
        }
        let _restore = RestoreRoot(prior);

        let raw = "alpha\nneedle with bounded context\nomega\n";
        let sha256 = crate::hashing::sha256_hex(raw.as_bytes());
        let name = format!("output_{sha256}_0123456789ab");
        let artifact_id = format!("art_{name}");
        let (path, relative_path) =
            crate::artifacts::write_session_artifact("owner-session", &artifact_id, raw)
                .expect("artifact");
        let store = new_shared_handle_store();
        let handle = {
            let mut guard = store.lock().await;
            guard.insert_artifact_text(
                "owner-session",
                name.clone(),
                ArtifactTextBacking {
                    relative_path,
                    byte_length: raw.len() as u64,
                    char_length: raw.chars().count(),
                    line_count: Some(raw.lines().count()),
                    sha256: sha256.clone(),
                },
                "alpha",
            )
        };

        let mut foreign = ToolContext::new(temp.path()).with_state_namespace("foreign-session");
        foreign.runtime.handle_store = store.clone();
        let error = HandleReadTool
            .execute(json!({"handle": handle, "count": true}), &foreign)
            .await
            .expect_err("cross-session artifact handle must fail");
        assert!(error.to_string().contains("current session"));

        let mut owner = ToolContext::new(temp.path()).with_state_namespace("owner-session");
        owner.runtime.handle_store = store.clone();
        let search = HandleReadTool
            .execute(
                json!({"handle": name, "search": {"query": "needle"}}),
                &owner,
            )
            .await
            .expect("bounded search");
        assert!(search.content.contains("bounded context"));
        let tiny_search = HandleReadTool
            .execute(
                json!({
                    "handle": name,
                    "search": {"query": "needle", "max_matches": 100},
                    "max_chars": 1
                }),
                &owner,
            )
            .await
            .expect("tiny bounded search");
        let tiny: Value = serde_json::from_str(&tiny_search.content).unwrap();
        let excerpt_chars = tiny["matches"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry["excerpt"].as_str())
            .map(str::chars)
            .map(Iterator::count)
            .sum::<usize>();
        assert!(excerpt_chars <= 1, "{}", tiny_search.content);
        assert_eq!(tiny["truncated"], true);

        *store.lock().await = HandleStore::default();
        std::fs::write(&path, raw.replace("alpha", "ALPHA")).expect("same-size corruption");
        let error = HandleReadTool
            .execute(
                json!({"handle": format!("output_{sha256}_0123456789ab"), "count": true}),
                &owner,
            )
            .await
            .expect_err("digest mismatch must fail");
        assert!(error.to_string().contains("sha256 mismatch"));

        let forged_sha256 = crate::hashing::sha256_hex(raw.replace("alpha", "ALPHA").as_bytes());
        let error = HandleReadTool
            .execute(
                json!({
                    "handle": {
                        "kind": "var_handle",
                        "session_id": "owner-session",
                        "name": format!("output_{sha256}_0123456789ab"),
                        "type": "str",
                        "length": raw.chars().count(),
                        "repr_preview": "",
                        "sha256": forged_sha256
                    },
                    "count": true
                }),
                &owner,
            )
            .await
            .expect_err("caller-supplied digest must not override the opaque alias");
        assert!(error.to_string().contains("opaque output alias"));

        std::fs::remove_file(path).expect("remove test artifact");
        let error = HandleReadTool
            .execute(
                json!({"handle": format!("output_{sha256}_0123456789ab"), "count": true}),
                &owner,
            )
            .await
            .expect_err("missing exact evidence must fail");
        assert!(error.to_string().contains("no payload found"));
    }

    #[tokio::test]
    async fn handle_read_rejects_oversized_search_query() {
        let ctx = ctx();
        let handle = {
            let mut store = ctx.runtime.handle_store.lock().await;
            store.insert_text("rlm:test", "body", "abc")
        };
        let error = HandleReadTool
            .execute(
                json!({"handle": handle, "search": {"query": "x".repeat(513)}}),
                &ctx,
            )
            .await
            .expect_err("query cap");
        assert!(error.to_string().contains("at most 512"));
    }
}
