//! BackgroundShell: a tracked background shell process with output buffering,
//! polling, stdin support, and snapshot/delta/completion-event generation.

use anyhow::{Context, Result, anyhow};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::process::{self, STALE_NO_OUTPUT_AFTER};
use super::types::{
    ShellChild, ShellCompletionEvent, ShellJobDetail, ShellJobOwner, ShellJobSnapshot, ShellResult,
    ShellStatus, StdinWriter,
};
use crate::sandbox::{SandboxManager, SandboxType};
use crate::tools::shell_output::truncate_with_meta;

/// A background shell process being tracked
pub struct BackgroundShell {
    pub id: String,
    pub command: String,
    pub working_dir: PathBuf,
    pub status: ShellStatus,
    pub exit_code: Option<i32>,
    pub started_at: Instant,
    pub(crate) last_output_at: Instant,
    pub(crate) last_observed_output_len: usize,
    pub sandbox_type: SandboxType,
    pub linked_task_id: Option<String>,
    pub owner_agent: Option<ShellJobOwner>,
    pub(crate) stdout_buffer: Arc<Mutex<Vec<u8>>>,
    pub(crate) stderr_buffer: Option<Arc<Mutex<Vec<u8>>>>,
    pub(crate) stdout_cursor: usize,
    pub(crate) stderr_cursor: usize,
    pub(crate) completion_reported: bool,
    pub(crate) stdin: Option<StdinWriter>,
    pub(crate) child: Option<ShellChild>,
    #[cfg(windows)]
    pub(crate) windows_job: Option<super::types::WindowsJob>,
    pub(crate) stdout_thread: Option<std::thread::JoinHandle<()>>,
    pub(crate) stderr_thread: Option<std::thread::JoinHandle<()>>,
}

impl BackgroundShell {
    /// Check if the process has completed and update status
    pub(crate) fn poll(&mut self) -> bool {
        self.refresh_output_activity();
        if self.status != ShellStatus::Running {
            return true;
        }

        if let Some(ref mut child) = self.child {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.exit_code = status.code;
                    self.status = if status.success {
                        ShellStatus::Completed
                    } else {
                        ShellStatus::Failed
                    };
                    self.collect_output();
                    true
                }
                Ok(None) => false, // Still running
                Err(_) => {
                    self.status = ShellStatus::Failed;
                    self.collect_output();
                    true
                }
            }
        } else {
            true
        }
    }

    fn refresh_output_activity(&mut self) {
        let observed_len = self.observed_output_len();
        if observed_len != self.last_observed_output_len {
            self.last_observed_output_len = observed_len;
            self.last_output_at = Instant::now();
        }
    }

    fn observed_output_len(&self) -> usize {
        let stdout_len = self
            .stdout_buffer
            .lock()
            .map(|data| data.len())
            .unwrap_or(0);
        let stderr_len = self
            .stderr_buffer
            .as_ref()
            .and_then(|buffer| buffer.lock().ok().map(|data| data.len()))
            .unwrap_or(0);
        stdout_len.saturating_add(stderr_len)
    }

    /// Collect output from the background threads
    pub(crate) fn collect_output(&mut self) {
        // Kill the whole process group before joining reader threads.
        // When the shell spawned persistent background jobs (e.g. `nohup curl`),
        // those subprocesses keep the pipe write-ends open after the shell exits.
        // Without this kill, handle.join() blocks indefinitely, freezing the UI
        // event loop that calls list_jobs() → poll() → collect_output().
        #[cfg(unix)]
        if let Some(child) = self.child.as_mut() {
            match child {
                ShellChild::Process(proc) => {
                    let _ = process::kill_child_process_group(proc);
                }
                #[cfg(not(target_env = "ohos"))]
                ShellChild::Pty(_) => {}
            }
        }
        #[cfg(windows)]
        process::terminate_and_close_windows_job(self.windows_job.take());
        if let Some(handle) = self.stdout_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.stderr_thread.take() {
            let _ = handle.join();
        }
        self.stdin = None;
        self.child = None;
    }

    pub(crate) fn write_stdin(&mut self, input: &str, close: bool) -> Result<()> {
        if let Some(stdin) = self.stdin.as_mut() {
            if !input.is_empty() {
                stdin
                    .write_all(input.as_bytes())
                    .context("Failed to write to stdin")?;
                stdin.flush().ok();
            }
            if close {
                self.stdin = None;
            }
            return Ok(());
        }

        if input.is_empty() && close {
            return Ok(());
        }

        Err(anyhow!("stdin is not available for task {}", self.id))
    }

    pub(crate) fn full_output(&self) -> (String, String, usize, usize) {
        let stdout_bytes = self
            .stdout_buffer
            .lock()
            .map(|data| data.clone())
            .unwrap_or_default();
        let stderr_bytes = self
            .stderr_buffer
            .as_ref()
            .and_then(|buffer| buffer.lock().ok().map(|data| data.clone()))
            .unwrap_or_default();

        let stdout_len = stdout_bytes.len();
        let stderr_len = stderr_bytes.len();

        (
            String::from_utf8_lossy(&stdout_bytes).to_string(),
            String::from_utf8_lossy(&stderr_bytes).to_string(),
            stdout_len,
            stderr_len,
        )
    }

    pub(crate) fn take_delta(&mut self) -> (String, String, usize, usize, usize, usize) {
        let (stdout_delta, stdout_total) =
            take_delta_from_buffer(&self.stdout_buffer, &mut self.stdout_cursor);
        let (stderr_delta, stderr_total) = if let Some(buffer) = self.stderr_buffer.as_ref() {
            take_delta_from_buffer(buffer, &mut self.stderr_cursor)
        } else {
            (Vec::new(), 0)
        };

        let stdout_delta_len = stdout_delta.len();
        let stderr_delta_len = stderr_delta.len();

        if stdout_delta_len > 0 || stderr_delta_len > 0 {
            self.last_output_at = Instant::now();
            self.last_observed_output_len = stdout_total.saturating_add(stderr_total);
        }

        (
            String::from_utf8_lossy(&stdout_delta).to_string(),
            String::from_utf8_lossy(&stderr_delta).to_string(),
            stdout_delta_len,
            stderr_delta_len,
            stdout_total,
            stderr_total,
        )
    }

    pub(crate) fn sandbox_denied(&self) -> bool {
        if matches!(self.status, ShellStatus::Running) {
            return false;
        }
        let (_, stderr_full, _, _) = self.full_output();
        SandboxManager::was_denied(
            self.sandbox_type,
            self.exit_code.unwrap_or(-1),
            &stderr_full,
        )
    }

    /// Kill the process
    pub(crate) fn kill(&mut self) -> Result<()> {
        if let Some(ref mut child) = self.child {
            match child {
                ShellChild::Process(proc) => {
                    #[cfg(windows)]
                    {
                        process::terminate_windows_job(self.windows_job.as_ref(), proc)
                            .context("Failed to kill process tree")?;
                        let _ = proc.wait();
                    }
                    #[cfg(not(windows))]
                    {
                        proc.kill().context("Failed to kill process")?;
                        let _ = proc.wait();
                    }
                }
                #[cfg(not(target_env = "ohos"))]
                ShellChild::Pty(child) => {
                    child.kill().context("Failed to kill process")?;
                    let _ = child.wait();
                }
            }
        }
        self.status = ShellStatus::Killed;
        self.collect_output();
        Ok(())
    }

    /// Get a snapshot of the current state
    #[allow(dead_code)]
    pub fn snapshot(&self) -> ShellResult {
        let sandboxed = !matches!(self.sandbox_type, SandboxType::None);
        let (stdout_full, stderr_full, _, _) = self.full_output();
        let (stdout, stdout_meta) = truncate_with_meta(&stdout_full);
        let (stderr, stderr_meta) = truncate_with_meta(&stderr_full);
        ShellResult {
            task_id: Some(self.id.clone()),
            status: self.status.clone(),
            exit_code: self.exit_code,
            stdout,
            stderr,
            duration_ms: u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            stdout_len: stdout_meta.original_len,
            stderr_len: stderr_meta.original_len,
            stdout_omitted: stdout_meta.omitted,
            stderr_omitted: stderr_meta.omitted,
            stdout_truncated: stdout_meta.truncated,
            stderr_truncated: stderr_meta.truncated,
            sandboxed,
            sandbox_type: if sandboxed {
                Some(self.sandbox_type.to_string())
            } else {
                None
            },
            sandbox_denied: self.sandbox_denied(),
        }
    }

    pub(crate) fn job_snapshot(&self) -> ShellJobSnapshot {
        // Use tail_from_buffer instead of full_output so we never clone the
        // entire accumulated stdout/stderr for display purposes.  full_output
        // is O(total_bytes_written), which caused the ShellManager mutex to be
        // held for an arbitrarily long time during list_jobs() calls from the
        // TUI event loop — freezing input handling on long automation runs.
        let (stdout_len, stdout_tail) = tail_from_buffer(&self.stdout_buffer, 1200);
        let (stderr_len, stderr_tail) = self
            .stderr_buffer
            .as_ref()
            .map(|buf| tail_from_buffer(buf, 1200))
            .unwrap_or((0, String::new()));
        let elapsed_since_output_ms = (self.status == ShellStatus::Running)
            .then(|| u64::try_from(self.last_output_at.elapsed().as_millis()).unwrap_or(u64::MAX));
        let stale = elapsed_since_output_ms.is_some_and(|elapsed| {
            elapsed >= u64::try_from(STALE_NO_OUTPUT_AFTER.as_millis()).unwrap_or(u64::MAX)
        });
        ShellJobSnapshot {
            id: self.id.clone(),
            job_id: self.id.clone(),
            command: self.command.clone(),
            cwd: self.working_dir.clone(),
            status: self.status.clone(),
            exit_code: self.exit_code,
            elapsed_ms: u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            stdout_tail,
            stderr_tail,
            stdout_len,
            stderr_len,
            stdin_available: self.stdin.is_some() && self.status == ShellStatus::Running,
            stale,
            elapsed_since_output_ms,
            linked_task_id: self.linked_task_id.clone(),
            owner_agent_id: self
                .owner_agent
                .as_ref()
                .map(|owner| owner.agent_id.clone()),
            owner_agent_name: self
                .owner_agent
                .as_ref()
                .map(|owner| owner.agent_name.clone()),
        }
    }

    pub(crate) fn completion_event(&self) -> ShellCompletionEvent {
        let snapshot = self.job_snapshot();
        ShellCompletionEvent {
            task_id: snapshot.id,
            command: snapshot.command,
            status: snapshot.status,
            exit_code: snapshot.exit_code,
            duration_ms: snapshot.elapsed_ms,
            stdout_tail: snapshot.stdout_tail,
            stderr_tail: snapshot.stderr_tail,
            linked_task_id: snapshot.linked_task_id,
            owner_agent_id: snapshot.owner_agent_id,
            owner_agent_name: snapshot.owner_agent_name,
        }
    }

    pub(crate) fn job_detail(&self) -> ShellJobDetail {
        let (stdout, stderr, _, _) = self.full_output();
        ShellJobDetail {
            snapshot: self.job_snapshot(),
            stdout,
            stderr,
        }
    }
}

impl Drop for BackgroundShell {
    fn drop(&mut self) {
        if self.status == ShellStatus::Running
            && let Some(ref mut child) = self.child
        {
            #[cfg(windows)]
            match child {
                ShellChild::Process(proc) => {
                    let _ = process::terminate_windows_job(self.windows_job.as_ref(), proc);
                }
                #[cfg(not(target_env = "ohos"))]
                ShellChild::Pty(child) => {
                    let _ = child.kill();
                }
            }
            #[cfg(not(windows))]
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Read only the delta (unread portion) of a byte buffer.
pub(crate) fn take_delta_from_buffer(
    buffer: &Arc<Mutex<Vec<u8>>>,
    cursor: &mut usize,
) -> (Vec<u8>, usize) {
    let guard = buffer.lock().unwrap_or_else(|e| e.into_inner());
    let total = guard.len();
    let start = (*cursor).min(total);
    // Clone only the unread portion (the delta), not the entire accumulated buffer.
    // Long-running processes can produce megabytes of output; cloning the full
    // buffer on every poll held the ShellManager mutex for O(total_bytes) time.
    let delta = guard[start..].to_vec();
    *cursor = total;
    (delta, total)
}

/// Read only the tail of a byte buffer and return (total_len, tail_string).
///
/// Avoids cloning the full buffer when only a trailing excerpt is needed
/// (e.g. for the job-panel display).  `max_tail_chars` is in Unicode scalar
/// values; we read at most `max_tail_chars * 4` bytes from the end to account
/// for multi-byte UTF-8 sequences.
pub(crate) fn tail_from_buffer(
    buffer: &Arc<Mutex<Vec<u8>>>,
    max_tail_chars: usize,
) -> (usize, String) {
    let guard = buffer.lock().unwrap_or_else(|e| e.into_inner());
    let total = guard.len();
    // Over-estimate byte count (4 bytes per char worst case for UTF-8).
    let mut tail_start = total.saturating_sub(max_tail_chars.saturating_mul(4));
    // Snap forward to the next valid UTF-8 codepoint boundary so we don't
    // pass a slice beginning with continuation bytes (0x80–0xBF) to
    // from_utf8_lossy, which would emit a leading U+FFFD replacement char.
    while tail_start < total && (guard[tail_start] & 0xC0) == 0x80 {
        tail_start += 1;
    }
    let tail_str = String::from_utf8_lossy(&guard[tail_start..]).into_owned();
    (total, tail_text(&tail_str, max_tail_chars))
}

fn tail_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let tail = text
        .chars()
        .rev()
        .take(max_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("...{tail}")
}
