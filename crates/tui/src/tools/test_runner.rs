//! Cargo test runner tool: `run_tests`.
//!
//! `cargo test` runs workspace code, so this tool follows the same explicit
//! approval policy as the other code-executing tools.

use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::cargo_failure_summary::summarize_cargo_failure;
use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_bool, optional_str,
};

use crate::dependencies::ExternalTool;

/// Tool for running `cargo test` in the workspace root.
pub struct RunTestsTool;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunTestsOutput {
    success: bool,
    exit_code: i32,
    stdout: String,
    stderr: String,
    command: String,
}

#[async_trait]
impl ToolSpec for RunTestsTool {
    fn name(&self) -> &'static str {
        "run_tests"
    }

    fn description(&self) -> &'static str {
        "Run `cargo test` in the workspace root with optional extra arguments."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "args": {
                    "type": "string",
                    "description": "Optional extra arguments to pass to `cargo test` (shell-style)."
                },
                "all_features": {
                    "type": "boolean",
                    "description": "When true, include `--all-features`."
                }
            },
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ExecutesCode, ToolCapability::Sandboxable]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        // `run_tests` declares `ToolCapability::ExecutesCode` — match the
        // default approval policy for code-executing tools.
        ApprovalRequirement::Required
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let all_features = optional_bool(&input, "all_features", false);
        let extra_args = optional_str(&input, "args")
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let mut args = vec!["test".to_string()];
        if all_features {
            args.push("--all-features".to_string());
        }
        if let Some(extra) = extra_args {
            let split = shlex::split(extra).ok_or_else(|| {
                ToolError::invalid_input("Failed to parse 'args' as shell-style tokens")
            })?;
            args.extend(split);
        }

        let command_str = format_command(&context.workspace, &args);
        let output = run_cargo(&context.workspace, &args)?;

        let exit_code = output.status.code().unwrap_or(-1);
        let result = RunTestsOutput {
            success: output.status.success(),
            exit_code,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            command: command_str,
        };
        tool_result_from_output(&result)
    }
}

fn tool_result_from_output(result: &RunTestsOutput) -> Result<ToolResult, ToolError> {
    // Preserve the complete streams until the engine-owned adaptive broker.
    // The broker decides whether the JSON stays inline or becomes an exact
    // artifact; this tool must not destroy bytes before that boundary.
    let mut tool_result =
        ToolResult::json(result).map_err(|e| ToolError::execution_failed(e.to_string()))?;
    tool_result.success = result.success;
    let mut metadata = json!({"exit_code": result.exit_code});
    if let Some(summary) = summarize_cargo_failure(
        &result.command,
        &result.stdout,
        &result.stderr,
        Some(result.exit_code),
    ) {
        metadata["summary"] = json!(summary.summary);
        metadata["cargo_failure_summary"] = summary.to_metadata_value();
    }
    tool_result = tool_result.with_metadata(metadata);
    Ok(tool_result)
}

// === Helpers ===

fn run_cargo(workspace: &Path, args: &[String]) -> Result<std::process::Output, ToolError> {
    let Some(mut cmd) = crate::dependencies::Cargo::command() else {
        return Err(ToolError::not_available(
            "cargo is not installed or not in PATH",
        ));
    };
    cmd.args(args).current_dir(workspace);
    cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ToolError::not_available("cargo is not installed or not in PATH")
        } else {
            ToolError::execution_failed(format!("Failed to run cargo: {e}"))
        }
    })
}

fn format_command(workspace: &Path, args: &[String]) -> String {
    format!(
        "(cd {} && cargo {})",
        workspace.display(),
        args.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tempfile::tempdir;

    static NEXT_CARGO_PROJECT: AtomicU64 = AtomicU64::new(0);

    fn cargo_available() -> bool {
        Command::new("cargo")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn init_cargo_project(root: &Path) -> std::path::PathBuf {
        let project_dir = root.join("project");
        let package_name = format!(
            "eval_project_{}_{}",
            std::process::id(),
            NEXT_CARGO_PROJECT.fetch_add(1, Ordering::Relaxed)
        );
        fs::create_dir_all(&project_dir).expect("create project dir");
        let status = crate::dependencies::Cargo::command()
            .expect("cargo not found")
            .args(["init", "--lib", "--vcs", "none", "-q"])
            .arg("--name")
            .arg(package_name)
            .current_dir(&project_dir)
            .status()
            .expect("cargo should spawn");
        assert!(status.success(), "cargo init failed");
        project_dir
    }

    /// `run_tests` is `ToolCapability::ExecutesCode`, so it must follow the
    /// explicit-approval policy that applies to other code-executing tools.
    #[test]
    fn run_tests_requires_user_approval() {
        let tool = RunTestsTool;
        assert_eq!(
            tool.approval_requirement(),
            ApprovalRequirement::Required,
            "run_tests must gate cargo test behind user approval"
        );
    }

    #[tokio::test]
    async fn run_tests_succeeds_on_fresh_project() {
        if !cargo_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        // Release jobs commonly export one CARGO_TARGET_DIR for the whole
        // workspace. Give concurrent nested Cargo fixtures distinct package
        // identities so their test artifacts cannot replace each other.
        let project_dir = init_cargo_project(tmp.path());

        let ctx = ToolContext::new(&project_dir);
        let tool = RunTestsTool;
        let result = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(result.success);

        let parsed: RunTestsOutput =
            serde_json::from_str(&result.content).expect("tool result should be json");
        assert!(
            parsed.success,
            "nested cargo test unexpectedly failed:\n{}",
            parsed.stderr
        );
        assert_eq!(parsed.exit_code, 0);
        assert!(parsed.command.contains("cargo test"));
    }

    #[tokio::test]
    async fn run_tests_reports_failures_without_hard_error() {
        if !cargo_available() {
            return;
        }
        let tmp = tempdir().expect("tempdir");
        let project_dir = init_cargo_project(tmp.path());

        let lib_rs = project_dir.join("src/lib.rs");
        let failing = r#"
pub fn add(a: i32, b: i32) -> i32 { a + b }

#[cfg(test)]
mod tests {
    #[test]
    fn fails() {
        assert_eq!(2 + 2, 5);
    }
}
"#;
        fs::write(&lib_rs, failing).expect("write failing test");

        let ctx = ToolContext::new(&project_dir);
        let tool = RunTestsTool;
        let result = tool.execute(json!({}), &ctx).await.expect("execute");
        assert!(!result.success);

        let parsed: RunTestsOutput =
            serde_json::from_str(&result.content).expect("tool result should be json");
        assert!(
            !parsed.success,
            "nested cargo test unexpectedly passed:\nstdout:\n{}\nstderr:\n{}",
            parsed.stdout, parsed.stderr
        );
        assert_ne!(parsed.exit_code, 0);
        let metadata = result.metadata.expect("metadata");
        assert_eq!(
            metadata["cargo_failure_summary"]["kind"],
            json!("test_failure")
        );
        assert!(
            metadata["cargo_failure_summary"]["summary"]
                .as_str()
                .unwrap()
                .contains("Failing tests:")
        );
    }

    #[test]
    fn failing_output_preserves_sentinel_beyond_old_truncation_boundary() {
        let sentinel = "CW_RUN_TESTS_EXACT_SENTINEL";
        let stdout = format!("{}{sentinel}", "x".repeat(50_000));
        let output = RunTestsOutput {
            success: false,
            exit_code: 101,
            stdout: stdout.clone(),
            stderr: "test result: FAILED. 0 passed; 1 failed".to_string(),
            command: "cargo test --workspace".to_string(),
        };

        let result = tool_result_from_output(&output).expect("tool result");
        assert!(!result.success, "transport status must match cargo status");
        assert!(result.content.contains(sentinel));
        let parsed: RunTestsOutput = serde_json::from_str(&result.content).expect("exact JSON");
        assert_eq!(parsed.stdout, stdout);
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)] // Serializes the process-global artifact-root override.
    async fn brokered_failure_keeps_exact_run_tests_output_and_compact_exit_facts() {
        use crate::context_budget::PressureLevel;
        use crate::tools::handle::new_shared_handle_store;
        use crate::tools::large_output_router::{
            LargeOutputBroker, ProjectionOutcome, WorkshopConfig,
        };

        let _root_guard = crate::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let temp = tempdir().expect("tempdir");
        let prior =
            crate::artifacts::set_test_artifact_sessions_root(Some(temp.path().join("sessions")));
        struct RestoreRoot(Option<std::path::PathBuf>);
        impl Drop for RestoreRoot {
            fn drop(&mut self) {
                crate::artifacts::set_test_artifact_sessions_root(self.0.take());
            }
        }
        let _restore = RestoreRoot(prior);

        let sentinel = "CW_BROKERED_RUN_TESTS_LATE_SENTINEL";
        let output = RunTestsOutput {
            success: false,
            exit_code: 101,
            stdout: format!(
                "running 1 test\ntest tests::fails ... FAILED\n{}\n{sentinel}\n",
                "repetitive compiler diagnostics\n".repeat(4_000)
            ),
            stderr: "test result: FAILED. 0 passed; 1 failed; 0 ignored\nerror: test failed"
                .to_string(),
            command: "cargo test --workspace".to_string(),
        };
        let mut result = tool_result_from_output(&output).expect("raw tool result");
        let exact_json = result.content.clone();
        let broker = LargeOutputBroker::new(
            WorkshopConfig {
                large_output_threshold_tokens: Some(100),
                ..WorkshopConfig::default()
            },
            "run-tests-broker-session",
            temp.path(),
            new_shared_handle_store(),
            PressureLevel::Low,
            Some(128_000),
        );

        let outcome = broker
            .project(
                "call-run-tests-large-failure",
                "run_tests",
                &json!({"args": "--workspace"}),
                &mut result,
            )
            .await;
        let ProjectionOutcome::Stored { path, .. } = outcome else {
            panic!("large failed RunTests result must be brokered")
        };
        assert!(!result.success);
        assert!(!result.content.contains(sentinel));
        let envelope: Value = serde_json::from_str(&result.content).expect("evidence envelope");
        assert_eq!(envelope["status"], json!("failed"));
        let facts = envelope["facts"].as_array().expect("facts");
        assert!(
            facts.iter().any(|fact| fact == "exit code: 101"),
            "exit code must remain model-visible: {facts:?}"
        );
        assert!(
            facts.iter().any(|fact| {
                fact.as_str()
                    .is_some_and(|text| text.contains("error: test failed"))
            }),
            "failure diagnosis must remain model-visible: {facts:?}"
        );
        assert!(facts.iter().any(|fact| fact == "tests::fails"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), exact_json);
        let parsed: RunTestsOutput = serde_json::from_str(&exact_json).unwrap();
        assert!(parsed.stdout.contains(sentinel));
        assert_eq!(parsed.exit_code, 101);
    }
}
