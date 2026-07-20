//! `retrieve_tool_result` - selective retrieval for spilled tool outputs.
//!
//! Large successful tool results are spilled to
//! `~/.codewhale/tool_outputs/<tool-call-id>.txt` by `tools::truncate`. This
//! tool gives the model a read-only, directory-scoped way to fetch summaries or
//! slices of those historical outputs without replaying the entire file into
//! every subsequent request.

use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::spec::{
    ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, optional_str, optional_u64,
    required_str,
};

const DEFAULT_MAX_BYTES: usize = 8 * 1024;
const HARD_MAX_BYTES: usize = 128 * 1024;
const DEFAULT_LINE_COUNT: usize = 40;
const HARD_LINE_COUNT: usize = 500;
const DEFAULT_MAX_MATCHES: usize = 20;
const HARD_MAX_MATCHES: usize = 100;
const DEFAULT_CONTEXT_LINES: usize = 1;
const HARD_CONTEXT_LINES: usize = 5;
/// Compatibility retrieval may still build a line index. Keep that legacy
/// path explicitly bounded; larger canonical artifacts are read through the
/// streaming `handle_read` surface advertised by adaptive evidence.
const HARD_SOURCE_BYTES: u64 = 32 * 1024 * 1024;

/// Retrieve summaries or slices of a prior spilled tool result.
pub struct RetrieveToolResultTool;

#[derive(Clone)]
struct ResolvedSpilloverReference {
    display_path: PathBuf,
    session: Option<SessionArtifactReference>,
}

#[derive(Clone)]
struct SessionArtifactReference {
    session_id: String,
    relative_path: PathBuf,
}

impl ResolvedSpilloverReference {
    fn legacy(path: PathBuf) -> Self {
        Self {
            display_path: path,
            session: None,
        }
    }

    fn session(session_id: &str, relative_path: PathBuf, display_path: PathBuf) -> Self {
        Self {
            display_path,
            session: Some(SessionArtifactReference {
                session_id: session_id.to_string(),
                relative_path,
            }),
        }
    }
}

#[async_trait]
impl ToolSpec for RetrieveToolResultTool {
    fn name(&self) -> &'static str {
        "retrieve_tool_result"
    }

    fn description(&self) -> &'static str {
        "Retrieve a previously spilled large tool result. Accepts a tool_call_id (`call_abc123`), artifact id (`art_call_abc123`), SHA reference (`sha:<64-hex>` or bare 64-hex from `<TOOL_RESULT_REF>`), relative filename (`call_abc123.txt`, `artifacts/art_call_abc123.txt`), or absolute path under ~/.codewhale. Modes: summary, head, tail, lines, query."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "ref": {
                    "type": "string",
                    "description": "Tool call id, artifact id (`art_<id>`), SHA ref (`sha:<64-hex>`), spillover filename, or absolute path under ~/.codewhale."
                },
                "mode": {
                    "type": "string",
                    "enum": ["summary", "head", "tail", "lines", "query"],
                    "description": "Retrieval mode. Defaults to summary."
                },
                "query": {
                    "type": "string",
                    "description": "Case-insensitive substring to search for when mode=query."
                },
                "lines": {
                    "type": "string",
                    "description": "Line selector for mode=lines, e.g. \"10\" or \"10-40\"."
                },
                "start_line": {
                    "type": "integer",
                    "description": "1-based first line for mode=lines."
                },
                "end_line": {
                    "type": "integer",
                    "description": "1-based final line for mode=lines."
                },
                "line_count": {
                    "type": "integer",
                    "description": "Number of lines for head/tail modes. Default 40, hard cap 500."
                },
                "max_bytes": {
                    "type": "integer",
                    "description": "Maximum bytes of excerpt text returned. Default 8192, hard cap 131072."
                },
                "max_matches": {
                    "type": "integer",
                    "description": "Maximum query matches or signal lines returned. Default 20, hard cap 100."
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Extra lines around each query match. Default 1, hard cap 5."
                }
            },
            "required": ["ref"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let reference = required_str(&input, "ref")?.trim();
        if reference.is_empty() {
            return Err(ToolError::invalid_input("ref cannot be empty"));
        }

        let mode = optional_str(&input, "mode")
            .unwrap_or("summary")
            .trim()
            .to_ascii_lowercase();
        let max_bytes = clamp_u64(
            optional_u64(&input, "max_bytes", DEFAULT_MAX_BYTES as u64),
            1,
            HARD_MAX_BYTES,
        );
        let resolved = resolve_spillover_reference(reference, &context.state_namespace)?;
        let display_path = resolved.display_path.clone();
        let content = tokio::task::spawn_blocking(move || read_verified_result(&resolved))
            .await
            .map_err(|err| {
                ToolError::execution_failed(format!("tool-result reader task failed: {err}"))
            })?
            .map_err(|err| {
                ToolError::execution_failed(format!(
                    "failed to read {}: {err}",
                    display_path.display()
                ))
            })?;

        let lines: Vec<&str> = content.lines().collect();
        let payload = match mode.as_str() {
            "summary" => build_summary_payload(
                reference,
                &display_path,
                &content,
                &lines,
                &input,
                max_bytes,
            ),
            "head" => {
                build_head_tail_payload(reference, &display_path, "head", &lines, &input, max_bytes)
            }
            "tail" => {
                build_head_tail_payload(reference, &display_path, "tail", &lines, &input, max_bytes)
            }
            "lines" => build_lines_payload(reference, &display_path, &lines, &input, max_bytes)?,
            "query" => build_query_payload(reference, &display_path, &lines, &input, max_bytes)?,
            other => {
                return Err(ToolError::invalid_input(format!(
                    "unsupported mode `{other}` (expected summary, head, tail, lines, or query)"
                )));
            }
        };

        ToolResult::json(&payload).map_err(|err| {
            ToolError::execution_failed(format!("failed to serialize result: {err}"))
        })
    }
}

fn read_verified_result(reference: &ResolvedSpilloverReference) -> io::Result<String> {
    let mut file = if let Some(session) = reference.session.as_ref() {
        crate::artifacts::open_session_artifact_for_read(
            &session.session_id,
            &session.relative_path,
        )?
    } else {
        open_legacy_result_no_follow(&reference.display_path)?
    };
    let source_bytes = file.metadata()?.len();
    if source_bytes > HARD_SOURCE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "stored tool result is {source_bytes} bytes; compatibility retrieval refuses sources over {HARD_SOURCE_BYTES} bytes (use handle_read)"
            ),
        ));
    }
    let mut content = String::with_capacity(source_bytes as usize);
    file.read_to_string(&mut content)?;
    if let Some(expected) = digest_encoded_in_filename(&reference.display_path) {
        let actual = crate::hashing::sha256_hex(content.as_bytes());
        if actual != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "stored tool-result digest does not match its content-addressed reference",
            ));
        }
    }
    Ok(content)
}

fn open_legacy_result_no_follow(path: &std::path::Path) -> io::Result<fs::File> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)?
    };
    #[cfg(not(unix))]
    let file = {
        if fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "tool-result symlink refused",
            ));
        }
        fs::File::open(path)?
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "tool result is not a regular file",
        ));
    }
    Ok(file)
}

fn digest_encoded_in_filename(path: &std::path::Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?.strip_suffix(".txt")?;
    if let Some(digests) = file_name.strip_prefix("art_output_") {
        let (content_digest, occurrence) = digests.split_once('_')?;
        if content_digest.len() == 64
            && occurrence.len() == 12
            && content_digest.chars().all(|ch| ch.is_ascii_hexdigit())
            && occurrence.chars().all(|ch| ch.is_ascii_hexdigit())
        {
            return Some(content_digest.to_ascii_lowercase());
        }
    }
    let digest = file_name.strip_prefix("sha_")?;
    crate::tools::truncate::is_valid_sha256(&digest.to_ascii_lowercase())
        .then(|| digest.to_ascii_lowercase())
}

/// Resolve a tool-result ref to a concrete file path.
///
/// Accepts six shapes:
/// 1. `tool_call_id` — legacy spillover form, `<id>.txt` under `tool_outputs/`.
/// 2. `art_<id>` — current artifact id, written by `apply_spillover_with_artifact`.
///    Tries the session artifact directory first, falls back to `<id>.txt`
///    (stripping the `art_` prefix) so old + new naming both work.
/// 3. `sha:<64-hex>` or bare 64-hex — content-addressed wire dedup, `sha_<hex>.txt`.
/// 4. `tool_result:<x>` — `<x>` is any of the above after the prefix.
/// 5. `artifacts/<file>.txt` or `<file>.txt` — relative paths.
/// 6. Absolute paths under the CodeWhale home.
///
/// The error message on a miss enumerates which forms were tried so the
/// model can correct course without a second blind guess.
fn resolve_spillover_reference(
    reference: &str,
    session_id: &str,
) -> Result<ResolvedSpilloverReference, ToolError> {
    let root = crate::tools::truncate::spillover_root().ok_or_else(|| {
        ToolError::execution_failed("could not resolve ~/.codewhale/tool_outputs")
    })?;
    let root_canonical = root.canonicalize().ok();

    // Resolve the session's `artifacts/` directory.
    // `session_artifact_absolute_path(sid, p)` returns
    // `~/.codewhale/sessions/<sid>/<p>` — so passing the literal
    // `ARTIFACTS_DIR_NAME` ("artifacts") gets us the real artifacts
    // root. An earlier draft passed `Path::new(".")` and took
    // `.parent()`, which landed one directory too high (`<sid>` instead
    // of `<sid>/artifacts`) and silently broke every bare `art_<id>`
    // ref — only the legacy-spillover fallback survived. The test
    // `resolves_art_prefix_to_legacy_spillover_id` masked it because
    // it ONLY wrote a legacy spillover file. The new test
    // `resolves_art_prefix_via_session_artifacts` exercises the real
    // path.
    let session_artifacts_root = if !session_id.is_empty() {
        crate::artifacts::session_artifact_absolute_path(
            session_id,
            std::path::Path::new(crate::artifacts::ARTIFACTS_DIR_NAME),
        )
    } else {
        None
    };
    let trimmed = reference.trim();
    let stripped = trimmed
        .strip_prefix("tool_result:")
        .unwrap_or(trimmed)
        .trim();

    let mut tried: Vec<PathBuf> = Vec::new();
    let try_legacy_path =
        |candidate: PathBuf, tried: &mut Vec<PathBuf>| -> Option<ResolvedSpilloverReference> {
            // Always record what we tried so the `not_found` diagnostic
            // can enumerate every candidate, even ones whose
            // `canonicalize` returns ENOENT. Models otherwise saw the
            // useless "(no valid candidates derived from ref)" line.
            tried.push(candidate.clone());

            // Do not follow a leaf symlink even in the compatibility store.
            // Session artifacts take the stricter resolver below, which also
            // refuses symlinks in every parent component.
            if let Ok(meta) = std::fs::symlink_metadata(&candidate)
                && meta.file_type().is_symlink()
            {
                return None;
            }

            let canonical = candidate.canonicalize().ok()?;
            if !canonical.is_file() {
                return None;
            }
            if root_canonical
                .as_ref()
                .is_some_and(|root| canonical.starts_with(root))
            {
                Some(ResolvedSpilloverReference::legacy(canonical))
            } else {
                None
            }
        };
    let try_session_relative =
        |relative: PathBuf, tried: &mut Vec<PathBuf>| -> Option<ResolvedSpilloverReference> {
            let display = session_artifacts_root
                .as_ref()
                .map_or_else(|| relative.clone(), |root| root.join(&relative));
            tried.push(display);
            let relative = PathBuf::from(crate::artifacts::ARTIFACTS_DIR_NAME).join(relative);
            let resolved =
                crate::artifacts::resolve_session_artifact_for_read(session_id, &relative).ok()?;
            Some(ResolvedSpilloverReference::session(
                session_id, relative, resolved,
            ))
        };

    // Form 1/3: absolute path. Validate it lives under one of the allowed roots.
    let raw_path = PathBuf::from(stripped);
    if raw_path.is_absolute() {
        if let Some(found) = try_legacy_path(raw_path.clone(), &mut tried) {
            return Ok(found);
        }
        if let Some(relative) = session_artifacts_root
            .as_ref()
            .and_then(|root| raw_path.strip_prefix(root).ok())
            && let Some(found) = try_session_relative(relative.to_path_buf(), &mut tried)
        {
            return Ok(found);
        }
        return Err(not_found(
            reference,
            &tried,
            &root,
            session_artifacts_root.as_deref(),
        ));
    }

    // Form 4: `sha:<hex>` prefix or bare 64-hex SHA → SHA-addressed file.
    let sha_candidate = stripped
        .strip_prefix("sha:")
        .or_else(|| stripped.strip_prefix("sha_"))
        .unwrap_or(stripped)
        .trim();
    if crate::tools::truncate::is_valid_sha256(&sha_candidate.to_ascii_lowercase())
        && let Some(p) = crate::tools::truncate::sha_spillover_path(sha_candidate)
        && let Some(found) = try_legacy_path(p, &mut tried)
    {
        return Ok(found);
    }

    // Form 5: relative path with separator or `.txt` suffix.
    let looks_like_path = stripped.ends_with(".txt")
        || stripped.contains('/')
        || (std::path::MAIN_SEPARATOR != '/' && stripped.contains(std::path::MAIN_SEPARATOR));
    if looks_like_path {
        // Try legacy spillover root.
        if let Some(found) = try_legacy_path(root.join(stripped), &mut tried) {
            return Ok(found);
        }
        // Session artifact roots point directly at `<sid>/artifacts/`.
        // Strip an optional leading `artifacts/` segment from transcript
        // paths before joining.
        if session_artifacts_root.is_some() {
            let rel = stripped.strip_prefix("artifacts/").unwrap_or(stripped);
            if let Some(found) = try_session_relative(PathBuf::from(rel), &mut tried) {
                return Ok(found);
            }
        }
        return Err(not_found(
            reference,
            &tried,
            &root,
            session_artifacts_root.as_deref(),
        ));
    }

    // Form 1: bare id → legacy `tool_outputs/<id>.txt`.
    if let Some(p) = crate::tools::truncate::spillover_path(stripped)
        && let Some(found) = try_legacy_path(p, &mut tried)
    {
        return Ok(found);
    }
    // Form 2: `art_<id>` → strip prefix and try both:
    //   a) session artifacts dir at `artifacts/art_<id>.txt`
    //   b) legacy spillover at `<id>.txt`
    if let Some(stripped_art) = stripped.strip_prefix("art_") {
        if session_artifacts_root.is_some()
            && let Some(found) =
                try_session_relative(PathBuf::from(format!("art_{stripped_art}.txt")), &mut tried)
        {
            return Ok(found);
        }
        if let Some(p) = crate::tools::truncate::spillover_path(stripped_art)
            && let Some(found) = try_legacy_path(p, &mut tried)
        {
            return Ok(found);
        }
        if session_artifacts_root.is_some()
            && let Some(found) =
                resolve_adaptive_call_artifact(session_id, stripped_art, &mut tried)
        {
            return Ok(found);
        }
    }
    // Form 2b: maybe the model passed the bare id but the artifact lives
    // under the session artifacts dir. Try `artifacts/art_<id>.txt`.
    if session_artifacts_root.is_some() {
        if let Some(found) =
            try_session_relative(PathBuf::from(format!("art_{stripped}.txt")), &mut tried)
        {
            return Ok(found);
        }
        if let Some(found) = resolve_adaptive_call_artifact(session_id, stripped, &mut tried) {
            return Ok(found);
        }
    }

    Err(not_found(
        reference,
        &tried,
        &root,
        session_artifacts_root.as_deref(),
    ))
}

/// Adaptive artifacts keep the content digest in their filename and a short
/// hash of the original call id as the occurrence suffix. This lets legacy
/// `ref=<call-id>` requests find the same canonical session file without a
/// second home-level raw copy.
fn resolve_adaptive_call_artifact(
    session_id: &str,
    tool_call_id: &str,
    tried: &mut Vec<PathBuf>,
) -> Option<ResolvedSpilloverReference> {
    let session_artifacts_root =
        crate::artifacts::resolve_session_artifacts_dir_for_read(session_id).ok()?;
    let digest = crate::hashing::sha256_hex(tool_call_id.as_bytes());
    let suffix = format!("_{}.txt", &digest[..12]);
    let mut entries = std::fs::read_dir(session_artifacts_root)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            (name.starts_with("art_output_") && name.ends_with(&suffix)).then_some(name)
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries.into_iter().find_map(|file_name| {
        let relative = PathBuf::from(crate::artifacts::ARTIFACTS_DIR_NAME).join(&file_name);
        tried.push(
            crate::artifacts::session_artifact_absolute_path(session_id, &relative)
                .unwrap_or_else(|| relative.clone()),
        );
        let resolved =
            crate::artifacts::resolve_session_artifact_for_read(session_id, &relative).ok()?;
        Some(ResolvedSpilloverReference::session(
            session_id, relative, resolved,
        ))
    })
}

/// Format a "ref didn't resolve" error with enough detail for the
/// caller to choose a valid reference form on the next attempt.
fn not_found(
    reference: &str,
    tried: &[PathBuf],
    legacy_root: &std::path::Path,
    session_artifacts_root: Option<&std::path::Path>,
) -> ToolError {
    let tried_list = if tried.is_empty() {
        "(no valid candidates derived from ref)".to_string()
    } else {
        tried
            .iter()
            .map(|p| format!("  - {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let session_hint = session_artifacts_root
        .map(|p| format!("\nsession artifacts root: {}", p.display()))
        .unwrap_or_default();
    ToolError::execution_failed(format!(
        "spilled tool result `{reference}` not found. Tried:\n{tried_list}\n\
         spillover root: {legacy}{session}\n\
         Accepted ref forms: \
         (a) `<tool_call_id>` for legacy spillover, \
         (b) `art_<tool_call_id>` for session artifacts, \
         (c) `sha:<64-hex>` or bare 64-hex from a <TOOL_RESULT_REF> block, \
         (d) `artifacts/art_<id>.txt` or `<id>.txt` relative paths. \
         If the source was a `<TOOL_RESULT_REF sha=\"...\" />` block, copy the \
         sha value and pass it as `ref=sha:<value>`. \
         If the source was an [artifact ...] block, pass the `id:` field \
         (the `art_<id>` form) directly.",
        legacy = legacy_root.display(),
        session = session_hint,
    ))
}

fn build_summary_payload(
    reference: &str,
    path: &std::path::Path,
    content: &str,
    lines: &[&str],
    input: &Value,
    max_bytes: usize,
) -> Value {
    let max_matches = clamp_u64(
        optional_u64(input, "max_matches", DEFAULT_MAX_MATCHES as u64),
        1,
        HARD_MAX_MATCHES,
    );
    let signal_lines = collect_signal_lines(lines, max_matches);
    let head_count = DEFAULT_LINE_COUNT.min(lines.len());
    let tail_count = DEFAULT_LINE_COUNT.min(lines.len());
    let head = render_numbered_lines(
        lines
            .iter()
            .take(head_count)
            .enumerate()
            .map(|(idx, line)| (idx + 1, *line)),
        max_bytes / 2,
    );
    let tail_start = lines.len().saturating_sub(tail_count);
    let tail = render_numbered_lines(
        lines
            .iter()
            .enumerate()
            .skip(tail_start)
            .map(|(idx, line)| (idx + 1, *line)),
        max_bytes / 2,
    );

    json!({
        "ref": reference,
        "path": path.display().to_string(),
        "mode": "summary",
        "total_bytes": content.len(),
        "total_lines": lines.len(),
        "non_empty_lines": lines.iter().filter(|line| !line.trim().is_empty()).count(),
        "signal_lines": signal_lines,
        "head": head,
        "tail": tail,
        "hint": "Use mode=head, tail, lines, or query to retrieve a narrower slice."
    })
}

fn build_head_tail_payload(
    reference: &str,
    path: &std::path::Path,
    mode: &str,
    lines: &[&str],
    input: &Value,
    max_bytes: usize,
) -> Value {
    let count = clamp_u64(
        optional_u64(input, "line_count", DEFAULT_LINE_COUNT as u64),
        1,
        HARD_LINE_COUNT,
    );
    let selected: Vec<(usize, &str)> = if mode == "head" {
        lines
            .iter()
            .take(count)
            .enumerate()
            .map(|(idx, line)| (idx + 1, *line))
            .collect()
    } else {
        let start = lines.len().saturating_sub(count);
        lines
            .iter()
            .enumerate()
            .skip(start)
            .map(|(idx, line)| (idx + 1, *line))
            .collect()
    };
    let excerpt = render_numbered_lines(selected.iter().copied(), max_bytes);

    json!({
        "ref": reference,
        "path": path.display().to_string(),
        "mode": mode,
        "total_lines": lines.len(),
        "line_count": count,
        "excerpt": excerpt,
    })
}

fn build_lines_payload(
    reference: &str,
    path: &std::path::Path,
    lines: &[&str],
    input: &Value,
    max_bytes: usize,
) -> Result<Value, ToolError> {
    let (start, end) = parse_line_selector(input)?;
    let excerpt = if start > lines.len() {
        String::new()
    } else {
        let end = end.min(lines.len());
        render_numbered_lines(
            lines
                .iter()
                .enumerate()
                .skip(start - 1)
                .take(end.saturating_sub(start) + 1)
                .map(|(idx, line)| (idx + 1, *line)),
            max_bytes,
        )
    };

    Ok(json!({
        "ref": reference,
        "path": path.display().to_string(),
        "mode": "lines",
        "total_lines": lines.len(),
        "start_line": start,
        "end_line": end.min(lines.len()),
        "excerpt": excerpt,
    }))
}

fn build_query_payload(
    reference: &str,
    path: &std::path::Path,
    lines: &[&str],
    input: &Value,
    max_bytes: usize,
) -> Result<Value, ToolError> {
    let query = optional_str(input, "query")
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .ok_or_else(|| ToolError::invalid_input("query is required when mode=query"))?;
    let query_lower = query.to_lowercase();
    let max_matches = clamp_u64(
        optional_u64(input, "max_matches", DEFAULT_MAX_MATCHES as u64),
        1,
        HARD_MAX_MATCHES,
    );
    let context_lines = clamp_u64(
        optional_u64(input, "context_lines", DEFAULT_CONTEXT_LINES as u64),
        0,
        HARD_CONTEXT_LINES,
    );

    let mut matched_lines = 0usize;
    let mut results = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if !line.to_lowercase().contains(&query_lower) {
            continue;
        }
        matched_lines += 1;
        if results.len() >= max_matches {
            continue;
        }
        let start = idx.saturating_sub(context_lines);
        let end = (idx + context_lines).min(lines.len().saturating_sub(1));
        let excerpt = render_numbered_lines(
            lines
                .iter()
                .enumerate()
                .skip(start)
                .take(end.saturating_sub(start) + 1)
                .map(|(line_idx, text)| (line_idx + 1, *text)),
            max_bytes / max_matches.max(1),
        );
        results.push(json!({
            "line": idx + 1,
            "excerpt": excerpt,
        }));
    }

    Ok(json!({
        "ref": reference,
        "path": path.display().to_string(),
        "mode": "query",
        "query": query,
        "total_lines": lines.len(),
        "matched_lines": matched_lines,
        "matches_returned": results.len(),
        "results": results,
    }))
}

fn parse_line_selector(input: &Value) -> Result<(usize, usize), ToolError> {
    let explicit_start = input.get("start_line").and_then(Value::as_u64);
    let explicit_end = input.get("end_line").and_then(Value::as_u64);
    if explicit_start.is_some() || explicit_end.is_some() {
        let start = explicit_start.ok_or_else(|| {
            ToolError::invalid_input("start_line is required when end_line is supplied")
        })?;
        let end = explicit_end.unwrap_or(start);
        return validate_line_range(start as usize, end as usize);
    }

    let spec = optional_str(input, "lines")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            ToolError::invalid_input(
                "mode=lines requires `lines` (for example \"10-40\") or start_line/end_line",
            )
        })?;

    if let Some((start, end)) = spec.split_once('-') {
        let start = parse_positive_line(start.trim(), "lines start")?;
        let end = parse_positive_line(end.trim(), "lines end")?;
        validate_line_range(start, end)
    } else {
        let line = parse_positive_line(spec, "lines")?;
        validate_line_range(line, line)
    }
}

fn validate_line_range(start: usize, end: usize) -> Result<(usize, usize), ToolError> {
    if start == 0 || end == 0 {
        return Err(ToolError::invalid_input("line numbers are 1-based"));
    }
    if end < start {
        return Err(ToolError::invalid_input(
            "end_line must be greater than or equal to start_line",
        ));
    }
    Ok((start, end))
}

fn parse_positive_line(raw: &str, field: &str) -> Result<usize, ToolError> {
    raw.parse::<usize>().map_err(|_| {
        ToolError::invalid_input(format!("{field} must be a positive integer line number"))
    })
}

fn collect_signal_lines(lines: &[&str], max_matches: usize) -> Vec<Value> {
    let mut out = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if !is_signal_line(line) {
            continue;
        }
        out.push(json!({
            "line": idx + 1,
            "text": truncate_line(line.trim(), 300),
        }));
        if out.len() >= max_matches {
            break;
        }
    }
    out
}

fn is_signal_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    [
        "error",
        "failed",
        "failure",
        "panic",
        "warning",
        "exception",
        "traceback",
        "assertion",
        "exit code",
        "test result",
        "thread '",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn render_numbered_lines<'a>(
    lines: impl IntoIterator<Item = (usize, &'a str)>,
    max_bytes: usize,
) -> String {
    let mut rendered = String::new();
    for (line_no, line) in lines {
        rendered.push_str(&format!("{line_no}: {line}\n"));
        if rendered.len() > max_bytes {
            break;
        }
    }
    truncate_text(&rendered, max_bytes)
}

fn truncate_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.trim_end_matches('\n').to_string();
    }
    let note = "\n[truncated to max_bytes]";
    let budget = max_bytes.saturating_sub(note.len()).max(1);
    let cut = (0..=budget)
        .rev()
        .find(|idx| text.is_char_boundary(*idx))
        .unwrap_or(0);
    format!("{}{}", text[..cut].trim_end_matches('\n'), note)
}

fn truncate_line(line: &str, max_chars: usize) -> String {
    if line.chars().count() <= max_chars {
        return line.to_string();
    }
    let mut out: String = line.chars().take(max_chars.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

fn clamp_u64(value: u64, min: usize, max: usize) -> usize {
    (value as usize).clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::MutexGuard;
    use tempfile::tempdir;

    struct SpilloverRootGuard {
        prior: Option<PathBuf>,
    }

    impl Drop for SpilloverRootGuard {
        fn drop(&mut self) {
            crate::tools::truncate::set_test_spillover_root(self.prior.take());
        }
    }

    fn set_spillover_root(path: PathBuf) -> SpilloverRootGuard {
        let prior = crate::tools::truncate::set_test_spillover_root(Some(path));
        SpilloverRootGuard { prior }
    }

    fn context() -> ToolContext {
        let tmp = tempdir().unwrap();
        ToolContext::new(tmp.path())
    }

    fn test_lock() -> MutexGuard<'static, ()> {
        crate::tools::truncate::TEST_SPILLOVER_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

    fn execute_tool(input: Value) -> Result<ToolResult, ToolError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(RetrieveToolResultTool.execute(input, &context()))
    }

    #[test]
    fn summary_reads_spillover_by_tool_call_id() {
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let _guard = set_spillover_root(tmp.path().join("tool_outputs"));
        crate::tools::truncate::write_spillover(
            "call-abc",
            "checking crate\nerror[E0425]: missing value\nwarning: unused import\nfinished",
        )
        .unwrap();

        let result = execute_tool(json!({"ref": "call-abc"})).unwrap();

        assert!(result.success);
        let body: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(body["mode"], "summary");
        assert!(body["signal_lines"].to_string().contains("error[E0425]"));
        assert!(body["signal_lines"].to_string().contains("warning"));
    }

    #[test]
    fn query_returns_matching_line_with_context() {
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let _guard = set_spillover_root(tmp.path().join("tool_outputs"));
        crate::tools::truncate::write_spillover(
            "call-query",
            "one\ntwo before\nneedle here\nafter\nlast",
        )
        .unwrap();

        let result = execute_tool(json!({
            "ref": "tool_result:call-query",
            "mode": "query",
            "query": "needle",
            "context_lines": 1
        }))
        .unwrap();

        let body: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(body["matched_lines"], 1);
        let rendered = body["results"].to_string();
        assert!(rendered.contains("2: two before"));
        assert!(rendered.contains("3: needle here"));
        assert!(rendered.contains("4: after"));
    }

    #[test]
    fn lines_mode_accepts_filename_inside_spillover_root() {
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("tool_outputs");
        let _guard = set_spillover_root(root.clone());
        crate::tools::truncate::write_spillover("call-lines", "a\nb\nc\nd").unwrap();

        let result = execute_tool(json!({
            "ref": "call-lines.txt",
            "mode": "lines",
            "lines": "2-3"
        }))
        .unwrap();

        let body: Value = serde_json::from_str(&result.content).unwrap();
        let excerpt = body["excerpt"].as_str().unwrap();
        assert!(excerpt.contains("2: b"));
        assert!(excerpt.contains("3: c"));
        assert!(!excerpt.contains("1: a"));
        assert!(!excerpt.contains("4: d"));
    }

    #[test]
    fn rejects_path_outside_spillover_root() {
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("tool_outputs");
        fs::create_dir_all(&root).unwrap();
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, "secret").unwrap();
        let _guard = set_spillover_root(root);

        let err = execute_tool(json!({"ref": outside.display().to_string()})).unwrap_err();

        // The new resolver classifies anything that fails to live under
        // an approved root as "not found" so we don't accidentally
        // leak whether an outside path exists on disk.
        let msg = err.to_string();
        assert!(
            msg.contains("not found"),
            "expected `not found` diagnostic, got: {msg}"
        );
    }

    #[test]
    fn resolves_sha_reference_from_wire_dedup() {
        // A SHA-keyed lookup — emulates what happens when the model
        // sees a `<TOOL_RESULT_REF sha="..." />` block and passes the
        // SHA to retrieve_tool_result.
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let _guard = set_spillover_root(tmp.path().join("tool_outputs"));
        let body = "checking crate ... error[E0425]: cannot find value\n".repeat(80);
        let sha = crate::hashing::sha256_hex(body.as_bytes());
        crate::tools::truncate::write_sha_spillover(&sha, &body).unwrap();

        // Form: `sha:<hex>`
        let result = execute_tool(json!({"ref": format!("sha:{sha}")})).unwrap();
        assert!(result.success, "sha:<hex> form should resolve");

        // Form: bare 64-hex
        let result = execute_tool(json!({"ref": &sha})).unwrap();
        assert!(result.success, "bare 64-hex form should resolve");
    }

    #[test]
    fn resolves_art_prefix_to_legacy_spillover_id() {
        // The model commonly sees `id: art_call_xyz` in artifact
        // ref blocks. retrieve_tool_result should strip the `art_`
        // prefix and find the legacy `<id>.txt` file if no
        // session-artifact equivalent exists.
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let _guard = set_spillover_root(tmp.path().join("tool_outputs"));
        crate::tools::truncate::write_spillover("call_xyz", "line1\nline2\nline3").unwrap();

        let result = execute_tool(json!({"ref": "art_call_xyz"})).unwrap();
        assert!(result.success, "art_ prefix should resolve to legacy id");
    }

    #[test]
    fn not_found_error_lists_tried_candidates_and_accepted_forms() {
        let _lock = test_lock();
        let tmp = tempdir().unwrap();
        let _guard = set_spillover_root(tmp.path().join("tool_outputs"));
        fs::create_dir_all(tmp.path().join("tool_outputs")).unwrap();

        let err = execute_tool(json!({"ref": "definitely_missing_id"})).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not found"), "got: {msg}");
        assert!(
            msg.contains("sha:"),
            "diagnostic should mention sha form: {msg}"
        );
        assert!(
            msg.contains("art_<tool_call_id>"),
            "diagnostic should mention art form: {msg}"
        );
        assert!(
            msg.contains("tool_outputs"),
            "tried list should include the legacy spillover candidate: {msg}"
        );
        assert!(
            !msg.contains("(no valid candidates derived from ref)"),
            "tried list should not be empty: {msg}"
        );
    }

    #[test]
    fn resolves_art_prefix_via_session_artifacts() {
        let _lock = test_lock();
        let _artifact_lock = crate::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tmp = tempdir().unwrap();
        let _spill_guard = set_spillover_root(tmp.path().join("tool_outputs"));
        let _art_guard = {
            let prior = crate::artifacts::set_test_artifact_sessions_root(Some(
                tmp.path().join("sessions"),
            ));
            scopeguard_for_test(prior)
        };
        let session_id = "session-abc";
        let body = "this is the canonical session artifact body, not a legacy file";
        crate::artifacts::write_session_artifact(session_id, "art_call_real", body).unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let workspace_tmp = tempdir().unwrap();
        let ctx = ToolContext::new(workspace_tmp.path()).with_state_namespace(session_id);
        let result = runtime
            .block_on(RetrieveToolResultTool.execute(json!({"ref": "art_call_real"}), &ctx))
            .expect("art_<id> should resolve via session artifacts");
        assert!(result.success);
        let payload: Value = serde_json::from_str(&result.content).unwrap();
        assert!(
            payload
                .to_string()
                .contains("canonical session artifact body"),
            "summary should pull from session artifact, got: {payload}"
        );
    }

    #[test]
    fn call_id_compatibility_reads_the_single_adaptive_session_artifact() {
        let _lock = test_lock();
        let _artifact_lock = crate::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tmp = tempdir().unwrap();
        let legacy_root = tmp.path().join("tool_outputs");
        let _spill_guard = set_spillover_root(legacy_root.clone());
        let _art_guard = {
            let prior = crate::artifacts::set_test_artifact_sessions_root(Some(
                tmp.path().join("sessions"),
            ));
            scopeguard_for_test(prior)
        };
        let session_id = "adaptive-session";
        let call_id = "call-provider-uuid-compatible";
        let body = "one canonical adaptive evidence file\nerror[E0382]: moved value\n";
        let content_sha = crate::hashing::sha256_hex(body.as_bytes());
        let call_sha = crate::hashing::sha256_hex(call_id.as_bytes());
        let artifact_id = format!("art_output_{content_sha}_{}", &call_sha[..12]);
        let (artifact_path, _) =
            crate::artifacts::write_session_artifact(session_id, &artifact_id, body).unwrap();
        assert!(
            !legacy_root.join(format!("{call_id}.txt")).exists(),
            "compatibility must not require a second legacy raw copy"
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let workspace_tmp = tempdir().unwrap();
        let ctx = ToolContext::new(workspace_tmp.path()).with_state_namespace(session_id);
        for reference in [call_id.to_string(), format!("art_{call_id}"), artifact_id] {
            let result = runtime
                .block_on(RetrieveToolResultTool.execute(
                    json!({"ref": reference, "mode": "query", "query": "E0382"}),
                    &ctx,
                ))
                .expect("compatibility ref resolves canonical artifact");
            assert!(result.content.contains("E0382"));
        }

        let tampered = body.replace("one", "ONE");
        assert_eq!(tampered.len(), body.len());
        std::fs::write(&artifact_path, tampered).expect("tamper canonical artifact");
        let error = runtime
            .block_on(
                RetrieveToolResultTool.execute(json!({"ref": call_id, "mode": "summary"}), &ctx),
            )
            .expect_err("compatibility retrieval must verify adaptive content digest");
        assert!(error.to_string().contains("content-addressed reference"));
    }

    #[test]
    fn compatibility_reader_refuses_unbounded_sources_before_materializing() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("legacy-large.txt");
        let file = fs::File::create(&path).expect("create sparse source");
        file.set_len(HARD_SOURCE_BYTES + 1)
            .expect("size sparse source");

        let error = read_verified_result(&ResolvedSpilloverReference::legacy(path))
            .expect_err("oversized source must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("use handle_read"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_inside_session_artifacts() {
        let _lock = test_lock();
        let _artifact_lock = crate::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tmp = tempdir().unwrap();
        let _spill_guard = set_spillover_root(tmp.path().join("tool_outputs"));
        let _art_guard = {
            let prior = crate::artifacts::set_test_artifact_sessions_root(Some(
                tmp.path().join("sessions"),
            ));
            scopeguard_for_test(prior)
        };
        let session_id = "session-xyz";
        // Plant a sensitive file outside the artifact dir.
        let secret = tmp.path().join("secret.txt");
        fs::write(&secret, "do not leak").unwrap();
        // Create the artifact dir, then drop a symlink inside it
        // pointing at the secret.
        let art_dir = tmp
            .path()
            .join("sessions")
            .join(session_id)
            .join("artifacts");
        fs::create_dir_all(&art_dir).unwrap();
        std::os::unix::fs::symlink(&secret, art_dir.join("art_evil.txt")).unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let workspace_tmp = tempdir().unwrap();
        let ctx = ToolContext::new(workspace_tmp.path()).with_state_namespace(session_id);
        let result =
            runtime.block_on(RetrieveToolResultTool.execute(json!({"ref": "art_evil"}), &ctx));
        let err = result.expect_err("symlink artifact must not resolve");
        assert!(
            err.to_string().contains("not found"),
            "expected `not found`, got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn compatibility_reader_refuses_cross_session_artifacts_parent_symlink() {
        let _lock = test_lock();
        let _artifact_lock = crate::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tmp = tempdir().unwrap();
        let _spill_guard = set_spillover_root(tmp.path().join("tool_outputs"));
        let sessions = tmp.path().join("sessions");
        let _art_guard = {
            let prior = crate::artifacts::set_test_artifact_sessions_root(Some(sessions.clone()));
            scopeguard_for_test(prior)
        };
        let owner_artifacts = sessions.join("owner/artifacts");
        fs::create_dir_all(&owner_artifacts).unwrap();
        let call_id = "call-cross-session-secret";
        let body = "owner-only canonical evidence";
        let content_sha = crate::hashing::sha256_hex(body.as_bytes());
        let call_sha = crate::hashing::sha256_hex(call_id.as_bytes());
        let file_name = format!("art_output_{content_sha}_{}.txt", &call_sha[..12]);
        fs::write(owner_artifacts.join(&file_name), body).unwrap();
        fs::create_dir_all(sessions.join("attacker")).unwrap();
        std::os::unix::fs::symlink(&owner_artifacts, sessions.join("attacker/artifacts")).unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let workspace_tmp = tempdir().unwrap();
        let ctx = ToolContext::new(workspace_tmp.path()).with_state_namespace("attacker");
        for reference in [call_id.to_string(), format!("art_{call_id}"), file_name] {
            let error = runtime
                .block_on(
                    RetrieveToolResultTool
                        .execute(json!({"ref": reference, "mode": "summary"}), &ctx),
                )
                .expect_err("cross-session parent symlink must not resolve");
            assert!(error.to_string().contains("not found"), "{error}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn compatibility_reader_keeps_open_anchored_when_parent_is_swapped() {
        let _lock = test_lock();
        let _artifact_lock = crate::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let tmp = tempdir().unwrap();
        let _spill_guard = set_spillover_root(tmp.path().join("tool_outputs"));
        let sessions = tmp.path().join("sessions");
        let _art_guard = {
            let prior = crate::artifacts::set_test_artifact_sessions_root(Some(sessions.clone()));
            scopeguard_for_test(prior)
        };
        let attacker_artifacts = sessions.join("attacker/artifacts");
        let owner_artifacts = sessions.join("owner/artifacts");
        fs::create_dir_all(&attacker_artifacts).unwrap();
        fs::create_dir_all(&owner_artifacts).unwrap();
        fs::write(
            attacker_artifacts.join("art_race.txt"),
            "attacker-owned evidence",
        )
        .unwrap();
        fs::write(owner_artifacts.join("art_race.txt"), "owner secret").unwrap();

        let attacker_artifacts_for_swap = attacker_artifacts.clone();
        let owner_artifacts_for_swap = owner_artifacts.clone();
        crate::artifacts::set_before_session_artifact_leaf_open_hook(move || {
            let original = attacker_artifacts_for_swap.with_file_name("artifacts-original");
            fs::rename(&attacker_artifacts_for_swap, &original).unwrap();
            std::os::unix::fs::symlink(&owner_artifacts_for_swap, &attacker_artifacts_for_swap)
                .unwrap();
        });

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let workspace_tmp = tempdir().unwrap();
        let ctx = ToolContext::new(workspace_tmp.path()).with_state_namespace("attacker");
        let result = runtime
            .block_on(
                RetrieveToolResultTool.execute(json!({"ref": "art_race", "mode": "summary"}), &ctx),
            )
            .expect("descriptor-relative open stays on the checked session tree");
        assert!(result.content.contains("attacker-owned evidence"));
        assert!(!result.content.contains("owner secret"));
    }

    struct ArtifactRootGuard {
        prior: Option<PathBuf>,
    }
    impl Drop for ArtifactRootGuard {
        fn drop(&mut self) {
            crate::artifacts::set_test_artifact_sessions_root(self.prior.take());
        }
    }
    fn scopeguard_for_test(prior: Option<PathBuf>) -> ArtifactRootGuard {
        ArtifactRootGuard { prior }
    }
}
