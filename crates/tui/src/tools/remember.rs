//! `remember` tool — model-callable bullet-add into the user memory file.
//!
//! Lets the model itself notice a durable preference, convention, or fact
//! worth keeping across sessions and write it to the user's `memory.md`.
//! The tool is auto-approved and side-effecting only on the user-owned
//! memory file (`~/.deepseek/memory.md` by default), so it doesn't get
//! gated behind the same approval flow as shell or arbitrary file writes.
//!
//! Only registered when `[memory] enabled = true` (or
//! `DEEPSEEK_MEMORY=on`). When disabled, the tool isn't surfaced to the
//! model at all, so prompts that mention `remember` simply fall through.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_str, required_str,
};

/// Tool that appends one bullet to the user memory file.
pub struct RememberTool;

#[async_trait]
impl ToolSpec for RememberTool {
    fn name(&self) -> &'static str {
        "remember"
    }

    fn description(&self) -> &'static str {
        "Append a durable note to the user memory file so it surfaces in \
         future sessions. Use this when the user states a preference, a \
         convention they want enforced, or a fact about themselves or \
         their workflow that you should not have to relearn next time. \
         Keep notes terse (one sentence). Don't store secrets, transient \
         tasks, or reasoning scratch — those belong in a checklist or in \
         the conversation."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "note": {
                    "type": "string",
                    "description": "The single-sentence durable note to remember."
                },
                "scope": {
                    "type": "string",
                    "enum": ["user", "project"],
                    "description": "Where to retain the note. Use user for preferences that should follow the person across workspaces. Use project for repo conventions or decisions that should only recall in this workspace. Defaults to user."
                }
            },
            "required": ["note"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        // Memory writes are scoped to the user's own memory file; gating
        // them behind the standard shell/write approval would defeat the
        // point of automatic memory.
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let note = required_str(&input, "note")?;
        let path = context.memory_path.as_ref().ok_or_else(|| {
            ToolError::execution_failed(
                "user memory is disabled — set `[memory] enabled = true` in config.toml or \
                 `DEEPSEEK_MEMORY=on` in the environment to enable",
            )
        })?;

        let scope = optional_str(&input, "scope").unwrap_or("user");
        let written_path = match scope {
            "user" => {
                crate::memory::append_entry(path, note).map_err(|err| {
                    ToolError::execution_failed(format!(
                        "failed to append to {}: {err}",
                        path.display()
                    ))
                })?;
                path.clone()
            }
            "project" => crate::memory::append_project_entry(path, &context.workspace, note)
                .map_err(|err| {
                    ToolError::execution_failed(format!(
                        "failed to append project memory for {}: {err}",
                        context.workspace.display()
                    ))
                })?,
            other => {
                return Err(ToolError::invalid_input(format!(
                    "invalid memory scope `{other}`; expected `user` or `project`"
                )));
            }
        };

        Ok(ToolResult::success(format!(
            "remembered ({scope}) in {}: {}",
            written_path.display(),
            note.trim_start_matches('#').trim(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn ctx_with_memory(path: PathBuf) -> ToolContext {
        let mut ctx = ToolContext::new(path.parent().unwrap_or_else(|| std::path::Path::new(".")));
        ctx.memory_path = Some(path);
        ctx
    }

    #[tokio::test]
    async fn returns_error_when_memory_disabled() {
        let tmp = tempdir().unwrap();
        let mut ctx = ToolContext::new(tmp.path());
        ctx.memory_path = None; // explicitly disabled

        let tool = RememberTool;
        let err = tool
            .execute(json!({"note": "use 4 spaces for indentation"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("memory is disabled"), "{err}");
    }

    #[tokio::test]
    async fn appends_bullet_to_memory_file() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("memory.md");
        let ctx = ctx_with_memory(path.clone());

        let tool = RememberTool;
        let result = tool
            .execute(json!({"note": "use 4 spaces for indentation"}), &ctx)
            .await
            .expect("ok");
        assert!(result.success);
        assert!(result.content.contains("4 spaces"));

        let body = std::fs::read_to_string(&path).expect("read");
        assert!(body.contains("4 spaces"));
        assert!(body.starts_with("- ("), "{body}");
    }

    #[tokio::test]
    async fn appends_project_scoped_bullet_to_project_memory_file() {
        let tmp = tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let path = tmp.path().join("memory.md");
        let mut ctx = ToolContext::new(&workspace);
        ctx.memory_path = Some(path.clone());

        let tool = RememberTool;
        let result = tool
            .execute(
                json!({"note": "run cargo fmt before commits", "scope": "project"}),
                &ctx,
            )
            .await
            .expect("ok");
        assert!(result.success);
        assert!(result.content.contains("project"));

        let user_body = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(!user_body.contains("cargo fmt"));

        let project_path = crate::memory::project_memory_path(&path, &workspace);
        let project_body = std::fs::read_to_string(&project_path).expect("project memory");
        assert!(project_body.contains("cargo fmt"));
    }

    #[tokio::test]
    async fn rejects_unknown_memory_scope() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("memory.md");
        let ctx = ctx_with_memory(path);

        let tool = RememberTool;
        let err = tool
            .execute(json!({"note": "x", "scope": "session"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid memory scope"), "{err}");
    }

    #[tokio::test]
    async fn rejects_missing_note_field() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("memory.md");
        let ctx = ctx_with_memory(path);

        let tool = RememberTool;
        let err = tool.execute(json!({}), &ctx).await.unwrap_err();
        assert!(err.to_string().to_lowercase().contains("note"), "{err}");
    }
}
