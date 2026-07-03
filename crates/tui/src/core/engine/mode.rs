//! Mode authority — functions that determine behaviour based on `AppMode`.
//!
//! Extracted from `core/engine.rs` and `core/engine/tool_setup.rs` (#3944).
//! Sandbox policy, shell policy, runtime instructions, and the per-turn
//! mode/approval/trust application live here.

use std::path::Path;

use crate::prompts;
use crate::sandbox::SandboxPolicy;
use crate::tui::app::AppMode;
use crate::worker_profile::ShellPolicy;

use super::Engine;

/// Pick the sandbox policy that gates shell commands for a given UI mode.
///
/// - **Plan** (#1077): `ReadOnly` — no writes, no network. The previous
///   `WorkspaceWrite` policy let `python -c "open('f','w').write('x')"` mutate
///   files inside the workspace because it whitelisted the workspace as
///   writable. Plan mode is investigation only; if the user wants to change
///   files they should switch to Agent.
/// - **Agent/Auto**: `WorkspaceWrite` with workspace as writable root and
///   network on. Approval flow gates risky individual commands; the sandbox
///   handles the rest. Network is allowed because cargo / npm / curl-style
///   commands are normal during agent work and DNS-deny breaks them silently.
/// - **YOLO**: `DangerFullAccess` — explicit no-guardrails contract.
pub(crate) fn sandbox_policy_for_mode(mode: AppMode, workspace: &Path) -> SandboxPolicy {
    match mode {
        AppMode::Plan => SandboxPolicy::ReadOnly,
        AppMode::Agent | AppMode::Auto => SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![workspace.to_path_buf()],
            network_access: true,
            exclude_tmpdir: false,
            exclude_slash_tmp: false,
        },
        AppMode::Yolo => SandboxPolicy::DangerFullAccess,
    }
}

/// Resolve the effective shell policy for a turn from the legacy shell opt-in
/// plus the active mode. This is the typed bridge away from passing a bare
/// `allow_shell` boolean through the runtime.
pub(crate) fn shell_policy_for_mode(mode: AppMode, allow_shell: bool) -> ShellPolicy {
    if !allow_shell {
        return ShellPolicy::None;
    }
    match mode {
        // Plan is read-only planning with no shell execution. The runtime
        // prompt already reports `shell_access="none"` for Plan, so mapping it
        // to `ReadOnly` here created a prompt/registry inconsistency (the
        // registry would expose `exec_shell` while the prompt said there was
        // no shell). Keep Plan shell-free; switch to Agent to run commands.
        AppMode::Plan => ShellPolicy::None,
        AppMode::Agent | AppMode::Auto | AppMode::Yolo => ShellPolicy::Full,
    }
}

/// Returns the mode-specific runtime instructions block injected into the
/// system prompt.
pub(crate) fn mode_runtime_instructions(mode: AppMode) -> &'static str {
    match mode {
        AppMode::Agent | AppMode::Auto => prompts::AGENT_MODE,
        AppMode::Plan => prompts::PLAN_MODE,
        AppMode::Yolo => prompts::YOLO_MODE,
    }
    .trim()
}

impl Engine {
    /// Apply the resolved mode, shell, trust, and approval policy for the
    /// upcoming turn.  Called once at the top of every `handle_send_message`
    /// and `handle_run_shell_command` so mid-turn consumers see a consistent
    /// snapshot.
    pub(super) fn apply_runtime_mode_policy(
        &mut self,
        mode: AppMode,
        allow_shell: bool,
        trust_mode: bool,
        auto_approve: bool,
        approval_mode: crate::tui::approval::ApprovalMode,
    ) {
        self.current_mode = mode;
        self.session.allow_shell = allow_shell;
        self.config.allow_shell = allow_shell;
        self.session.trust_mode = trust_mode;
        self.config.trust_mode = trust_mode;
        self.session.auto_approve = auto_approve;
        self.session.approval_mode =
            super::trust::agent_approval_mode_for_turn(auto_approve, approval_mode);
    }
}
