//! User-level memory file (deprecated — see Moraine).
//!
//! ## Deprecation
//!
//! DEPRECATED(v0.8.66–v0.8.71): Superseded by Moraine MCP recall.
//! The legacy push/inject path is gated behind `MemoryConfig.moraine_fallback`.
//! When Moraine lands (v0.8.66/67), this module can be deleted entirely.
//!
//! Migration guide: use Moraine MCP tools (`search_sessions`, `open`,
//! `list_sessions`, `file_attention`) instead of `<user_memory>` injection.
//!
//! Ref: https://github.com/Hmbown/CodeWhale/issues/3495 (Moraine adoption)
//! Ref: https://github.com/Hmbown/CodeWhale/issues/3490 (v0.8.71 dead-code inventory)
//!
//! ### Migration
//!
//! 1. Install Moraine: `uv tool install moraine-cli && moraine setup && moraine up`
//! 2. Enable `moraine-mcp` in `~/.codewhale/mcp.json` (set `disabled` to `false`)
//! 3. Set `[memory] moraine_fallback = true` in `config.toml` to skip the legacy
//!    `<user_memory>` block, `remember` tool, and `# foo` quick-add.
//!
//! ## Legacy docs (pre-Moraine)
//!
//! v0.8.8 shipped an MVP that let the user keep a persistent personal
//! note file the model sees on every turn:
//!
//! - **Load** `~/.codewhale/memory.md` (path is configurable via
//!   `memory_path` in `config.toml` and `DEEPSEEK_MEMORY_PATH` env),
//!   wrap it in a `<user_memory>` block, and prepend it to the system
//!   prompt alongside the existing `<project_instructions>` block.
//! - **`# foo`** typed in the composer appends `foo` to the memory
//!   file as a timestamped bullet — fast capture without leaving the TUI.
//! - **`/memory`** shows the resolved file path and current contents, and
//!   **`/memory edit`** prints a copy-pasteable `$VISUAL` / `$EDITOR`
//!   command for opening the file yourself.
//! - **`remember` tool** lets the model itself append a bullet when it
//!   notices a durable preference or convention worth keeping across
//!   sessions.
//!
//! Default behavior is **opt-in**: load + use the memory file only when
//! `[memory] enabled = true` in `config.toml` or `DEEPSEEK_MEMORY=on`.
//! That keeps existing users on zero-overhead behavior and makes the
//! feature explicit.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;

/// Maximum size of the user memory file. Larger files are loaded but the
/// `<user_memory>` block carries a `<truncated bytes=N source="...">`
/// marker so the user knows the model only saw a slice. Mirrors
/// `project_context::MAX_CONTEXT_SIZE`.
const MAX_MEMORY_SIZE: usize = 100 * 1024;
/// Maximum project-scoped memory injected into the prompt. Project memory is a
/// bridge toward Moraine-style recall, so it intentionally stays smaller than
/// the legacy user-global block.
const MAX_PROJECT_MEMORY_SIZE: usize = 32 * 1024;

/// Read the user memory file at `path`, returning `None` when the file
/// doesn't exist or is empty after trimming.
#[must_use]
pub fn load(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    Some(content)
}

/// Wrap memory content in a `<user_memory>` block ready to prepend to the
/// system prompt. The `source` value is rendered verbatim into a
/// `source="…"` attribute — pass the path so the model can see where the
/// memory came from. Returns `None` for empty content.
#[must_use]
pub fn as_system_block(content: &str, source: &Path) -> Option<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }

    let display = source.display().to_string();
    let payload = if content.len() > MAX_MEMORY_SIZE {
        let cutoff = truncation_cutoff(content, &display);
        let omitted_bytes = content.len() - cutoff;
        let mut head = content[..cutoff].to_string();
        head.push_str(&truncation_marker(omitted_bytes, &display));
        head
    } else {
        trimmed.to_string()
    };

    Some(format!(
        "<user_memory source=\"{display}\">\n{payload}\n</user_memory>"
    ))
}

/// Wrap a bounded project-memory recall slice in a `<project_memory>` block.
#[must_use]
pub fn as_project_system_block(content: &str, source: &Path, workspace: &Path) -> Option<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }

    let display = source.display().to_string();
    let workspace_display = workspace.display().to_string();
    let payload = bounded_project_memory_payload(trimmed, &display);

    Some(format!(
        "<project_memory workspace=\"{workspace_display}\" source=\"{display}\" max_bytes=\"{MAX_PROJECT_MEMORY_SIZE}\">\n{payload}\n</project_memory>"
    ))
}

fn truncation_cutoff(content: &str, source: &str) -> usize {
    let mut cutoff = previous_char_boundary(content, MAX_MEMORY_SIZE);
    loop {
        let omitted_bytes = content.len() - cutoff;
        let max_head_len =
            MAX_MEMORY_SIZE.saturating_sub(truncation_marker(omitted_bytes, source).len());
        let next_cutoff = previous_char_boundary(content, cutoff.min(max_head_len));
        if next_cutoff == cutoff {
            return cutoff;
        }
        cutoff = next_cutoff;
    }
}

fn truncation_marker(omitted_bytes: usize, source: &str) -> String {
    format!("\n<truncated bytes={omitted_bytes} source=\"{source}\">")
}

fn previous_char_boundary(value: &str, mut index: usize) -> usize {
    while !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Compose the `<user_memory>` block for the system prompt, honouring the
/// opt-in toggle. Returns `None` when the feature is disabled, when
/// `moraine_fallback` is active, or when the file is missing / empty so
/// the caller doesn't have to check both conditions.
///
/// Callers that hold a `&Config` should pass `config.memory_enabled() &&
/// !config.moraine_fallback()` and `config.memory_path()` directly.
/// The split keeps this module `Config`-free so it can be reused from
/// sub-agent / engine boundaries where the high-level `Config` isn't
/// available.
#[must_use]
pub fn compose_block(enabled: bool, path: &Path) -> Option<String> {
    if !enabled {
        return None;
    }
    let content = load(path)?;
    as_system_block(&content, path)
}

/// Compose both legacy user-global memory and the bounded project-scoped
/// memory seed. Returns `None` when both blocks are absent.
#[must_use]
pub fn compose_prompt_block(enabled: bool, user_path: &Path, workspace: &Path) -> Option<String> {
    if !enabled {
        return None;
    }

    let mut blocks = Vec::new();
    if let Some(block) = compose_block(true, user_path) {
        blocks.push(block);
    }
    if let Some(block) = compose_project_block(true, user_path, workspace) {
        blocks.push(block);
    }

    if blocks.is_empty() {
        None
    } else {
        Some(blocks.join("\n\n"))
    }
}

/// Compose only the project-scoped memory block for `workspace`.
#[must_use]
pub fn compose_project_block(enabled: bool, user_path: &Path, workspace: &Path) -> Option<String> {
    if !enabled {
        return None;
    }
    let path = project_memory_path(user_path, workspace);
    let content = load(&path)?;
    as_project_system_block(&content, &path, workspace)
}

/// Append `entry` to the memory file at `path`, creating it (and its
/// parent directory) if needed. The entry is timestamped so the user can
/// later see when each note was added. The leading `#` from a `# foo`
/// quick-add is stripped so the file stays as readable Markdown.
pub fn append_entry(path: &Path, entry: &str) -> io::Result<()> {
    let trimmed = entry.trim_start_matches('#').trim();
    if trimmed.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "memory entry is empty after stripping `#` prefix",
        ));
    }

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    let timestamp = Utc::now().format("%Y-%m-%d %H:%M UTC");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "- ({timestamp}) {trimmed}")?;
    Ok(())
}

/// Append a project-scoped memory entry under the CodeWhale memory state tree
/// associated with `user_path`.
pub fn append_project_entry(
    user_path: &Path,
    workspace: &Path,
    entry: &str,
) -> io::Result<PathBuf> {
    let path = project_memory_path(user_path, workspace);
    append_entry(&path, entry)?;
    Ok(path)
}

/// Derive the memory file for `workspace` without storing anything inside the
/// workspace. The readable slug helps humans inspect the directory; the hash
/// prevents collisions between repos with the same basename.
#[must_use]
pub fn project_memory_path(user_path: &Path, workspace: &Path) -> PathBuf {
    let base = user_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    base.join("memory")
        .join("projects")
        .join(format!("{}.md", project_memory_key(workspace)))
}

#[must_use]
pub fn project_memory_key(workspace: &Path) -> String {
    let identity = fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let identity = identity.to_string_lossy();
    let slug = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_project_memory_slug)
        .filter(|slug| !slug.is_empty())
        .unwrap_or_else(|| "workspace".to_string());
    let digest = crate::hashing::sha256_hex(identity.as_bytes());
    format!("{slug}-{}", &digest[..12])
}

fn sanitize_project_memory_slug(input: &str) -> String {
    let mut slug = String::new();
    for ch in input.chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '-'
        };
        if normalized == '-' && slug.ends_with('-') {
            continue;
        }
        slug.push(normalized);
        if slug.len() >= 48 {
            break;
        }
    }
    slug.trim_matches('-').to_string()
}

fn bounded_project_memory_payload(content: &str, source: &str) -> String {
    if content.len() <= MAX_PROJECT_MEMORY_SIZE {
        return content.to_string();
    }

    let mut cutoff = previous_char_boundary(content, content.len() - MAX_PROJECT_MEMORY_SIZE);
    loop {
        let omitted_bytes = cutoff;
        let marker = project_truncation_marker(omitted_bytes, source);
        let max_tail_len = MAX_PROJECT_MEMORY_SIZE.saturating_sub(marker.len() + 1);
        let next_cutoff =
            previous_char_boundary(content, content.len().saturating_sub(max_tail_len));
        if next_cutoff == cutoff {
            return format!("{marker}\n{}", content[cutoff..].trim_start());
        }
        cutoff = next_cutoff;
    }
}

fn project_truncation_marker(omitted_bytes: usize, source: &str) -> String {
    format!("<project_memory_truncated bytes={omitted_bytes} source=\"{source}\">")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_returns_none_for_missing_file() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("never-existed.md");
        assert!(load(&path).is_none());
    }

    #[test]
    fn load_returns_none_for_whitespace_only_file() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("memory.md");
        fs::write(&path, "   \n   \n").unwrap();
        assert!(load(&path).is_none());
    }

    #[test]
    fn load_returns_content_for_real_file() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("memory.md");
        fs::write(&path, "remember the milk").unwrap();
        assert_eq!(load(&path).as_deref(), Some("remember the milk"));
    }

    #[test]
    fn as_system_block_produces_xml_wrapper() {
        let block = as_system_block("note 1", Path::new("/tmp/m.md")).unwrap();
        assert!(block.contains("<user_memory source=\"/tmp/m.md\">"));
        assert!(block.contains("note 1"));
        assert!(block.ends_with("</user_memory>"));
    }

    #[test]
    fn as_system_block_returns_none_for_empty_content() {
        assert!(as_system_block("   ", Path::new("/tmp/m.md")).is_none());
    }

    #[test]
    fn project_memory_path_is_scoped_by_workspace_identity() {
        let tmp = tempdir().unwrap();
        let user_path = tmp.path().join("memory.md");
        let workspace_a = tmp.path().join("same-name-a").join("repo");
        let workspace_b = tmp.path().join("same-name-b").join("repo");
        fs::create_dir_all(&workspace_a).unwrap();
        fs::create_dir_all(&workspace_b).unwrap();

        let path_a = project_memory_path(&user_path, &workspace_a);
        let path_b = project_memory_path(&user_path, &workspace_b);

        assert_ne!(path_a, path_b);
        assert!(path_a.starts_with(tmp.path().join("memory").join("projects")));
        assert!(
            path_a
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("repo-")
        );
    }

    #[test]
    fn project_memory_block_is_distinct_and_bounded() {
        let big = "project note\n".repeat(4096);
        let block = as_project_system_block(&big, Path::new("/tmp/project.md"), Path::new("/repo"))
            .unwrap();

        assert!(block.contains("<project_memory workspace=\"/repo\""));
        assert!(block.contains("max_bytes=\"32768\""));
        assert!(block.contains("<project_memory_truncated bytes="));
        let payload = block
            .strip_prefix("<project_memory workspace=\"/repo\" source=\"/tmp/project.md\" max_bytes=\"32768\">\n")
            .unwrap()
            .strip_suffix("\n</project_memory>")
            .unwrap();
        assert!(payload.len() <= MAX_PROJECT_MEMORY_SIZE);
    }

    #[test]
    fn compose_prompt_block_keeps_user_and_project_memory_separate() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("repo");
        fs::create_dir_all(&workspace).unwrap();
        let user_path = tmp.path().join("memory.md");
        append_entry(&user_path, "prefer concise answers").unwrap();
        append_project_entry(&user_path, &workspace, "run cargo fmt here").unwrap();

        let block = compose_prompt_block(true, &user_path, &workspace).unwrap();

        assert!(block.contains("<user_memory "));
        assert!(block.contains("prefer concise answers"));
        assert!(block.contains("<project_memory "));
        assert!(block.contains("run cargo fmt here"));
    }

    #[test]
    fn compose_project_block_does_not_leak_between_workspaces() {
        let tmp = tempdir().unwrap();
        let user_path = tmp.path().join("memory.md");
        let workspace_a = tmp.path().join("a");
        let workspace_b = tmp.path().join("b");
        fs::create_dir_all(&workspace_a).unwrap();
        fs::create_dir_all(&workspace_b).unwrap();
        append_project_entry(&user_path, &workspace_a, "a-only convention").unwrap();

        assert!(
            compose_project_block(true, &user_path, &workspace_a)
                .unwrap()
                .contains("a-only convention")
        );
        assert!(compose_project_block(true, &user_path, &workspace_b).is_none());
    }

    #[test]
    fn as_system_block_truncates_oversize_input() {
        let big = "x".repeat(MAX_MEMORY_SIZE + 100);
        let block = as_system_block(&big, Path::new("/tmp/m.md")).unwrap();
        let payload = user_memory_payload(&block);
        assert_eq!(payload.len(), MAX_MEMORY_SIZE);
        assert!(payload.ends_with("<truncated bytes=141 source=\"/tmp/m.md\">"));
    }

    #[test]
    fn as_system_block_truncates_non_ascii_at_char_boundary() {
        let mut content = "x".repeat(MAX_MEMORY_SIZE - 1);
        content.push('é');
        content.push_str("tail");

        let block = as_system_block(&content, Path::new("/tmp/m.md")).unwrap();
        let payload = block
            .strip_prefix("<user_memory source=\"/tmp/m.md\">\n")
            .unwrap()
            .strip_suffix("\n</user_memory>")
            .unwrap();
        let (head, marker) = payload
            .split_once("\n<truncated bytes=45 source=\"/tmp/m.md\">")
            .unwrap();

        assert_eq!(payload.len(), MAX_MEMORY_SIZE);
        assert_eq!(head.len(), MAX_MEMORY_SIZE - 40);
        assert!(head.bytes().all(|byte| byte == b'x'));
        assert_eq!(marker, "");
    }

    #[test]
    fn as_system_block_truncates_emoji_at_char_boundary() {
        let mut content = "x".repeat(MAX_MEMORY_SIZE - 1);
        content.push('😀');
        content.push_str("tail");

        let block = as_system_block(&content, Path::new("/tmp/m.md")).unwrap();
        assert!(block.contains("<truncated bytes=47 source=\"/tmp/m.md\">"));

        let payload = block
            .strip_prefix("<user_memory source=\"/tmp/m.md\">\n")
            .unwrap()
            .strip_suffix("\n</user_memory>")
            .unwrap();
        let head = payload
            .strip_suffix("\n<truncated bytes=47 source=\"/tmp/m.md\">")
            .unwrap();

        assert_eq!(payload.len(), MAX_MEMORY_SIZE);
        assert!(head.len() <= MAX_MEMORY_SIZE);
        assert_eq!(head.len(), MAX_MEMORY_SIZE - 40);
        assert!(head.bytes().all(|byte| byte == b'x'));
    }

    fn user_memory_payload(block: &str) -> &str {
        block
            .strip_prefix("<user_memory source=\"/tmp/m.md\">\n")
            .unwrap()
            .strip_suffix("\n</user_memory>")
            .unwrap()
    }

    #[test]
    fn append_entry_creates_file_and_writes_one_bullet() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("memory.md");
        append_entry(&path, "# remember the milk").unwrap();

        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("remember the milk"), "{body}");
        assert!(
            body.starts_with("- ("),
            "should start with bullet + date: {body}"
        );
        assert!(body.trim_end().ends_with("remember the milk"));
    }

    #[test]
    fn append_entry_appends_subsequent_lines() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("memory.md");
        append_entry(&path, "# first").unwrap();
        append_entry(&path, "second").unwrap();
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("first"));
        assert!(body.contains("second"));
        // Two bullets means two lines of `- (date) entry`.
        assert_eq!(body.matches("- (").count(), 2);
    }

    #[test]
    fn append_entry_rejects_empty_after_strip() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("memory.md");
        let err = append_entry(&path, "###").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
