//! Shell types: data structures for shell execution results, job tracking,
//! process management, and platform-specific abstractions.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, ChildStdin};

/// Status of a shell process
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ShellStatus {
    Running,
    Completed,
    Failed,
    Killed,
    TimedOut,
}

/// Result from a shell command execution
#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct ShellResult {
    pub task_id: Option<String>,
    pub status: ShellStatus,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    /// Original stdout length in bytes.
    #[serde(default)]
    pub stdout_len: usize,
    /// Original stderr length in bytes.
    #[serde(default)]
    pub stderr_len: usize,
    /// Bytes omitted from stdout due to truncation.
    #[serde(default)]
    pub stdout_omitted: usize,
    /// Bytes omitted from stderr due to truncation.
    #[serde(default)]
    pub stderr_omitted: usize,
    /// Whether stdout was truncated.
    #[serde(default)]
    pub stdout_truncated: bool,
    /// Whether stderr was truncated.
    #[serde(default)]
    pub stderr_truncated: bool,
    /// Whether the command was executed in a sandbox.
    #[serde(default)]
    pub sandboxed: bool,
    /// Type of sandbox used (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_type: Option<String>,
    /// Whether the command was blocked by sandbox restrictions.
    #[serde(default)]
    pub sandbox_denied: bool,
}

/// Compact, UI-oriented view of a tracked background shell job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellJobSnapshot {
    pub id: String,
    pub job_id: String,
    pub command: String,
    pub cwd: PathBuf,
    pub status: ShellStatus,
    pub exit_code: Option<i32>,
    pub elapsed_ms: u64,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub stdout_len: usize,
    pub stderr_len: usize,
    pub stdin_available: bool,
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_since_output_ms: Option<u64>,
    pub linked_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_agent_name: Option<String>,
}

/// Once-only completion event for a tracked background shell job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellCompletionEvent {
    pub task_id: String,
    pub command: String,
    pub status: ShellStatus,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub linked_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_agent_name: Option<String>,
}

/// Optional owner attribution for background shell work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellJobOwner {
    pub agent_id: String,
    pub agent_name: String,
}

/// Full output view used by `/jobs show <id>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellJobDetail {
    pub snapshot: ShellJobSnapshot,
    pub stdout: String,
    pub stderr: String,
}

pub struct ShellDeltaResult {
    pub command: String,
    pub result: ShellResult,
    pub stdout_total_len: usize,
    pub stderr_total_len: usize,
}

pub(crate) enum ShellChild {
    Process(Child),
    #[cfg(not(target_env = "ohos"))]
    Pty(Box<dyn portable_pty::Child + Send>),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ShellExitStatus {
    pub(crate) code: Option<i32>,
    pub(crate) success: bool,
}

pub(crate) enum StdinWriter {
    Pipe(ChildStdin),
    #[cfg(not(target_env = "ohos"))]
    Pty(Box<dyn Write + Send>),
}

#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct WindowsJob {
    pub(crate) handle: windows::Win32::Foundation::HANDLE,
}
