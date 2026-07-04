//! Typed per-turn authority and execution posture.
//!
//! This is the engine-side owner for mode, shell, sandbox, approval, trust, and
//! provenance decisions. It deliberately preserves the existing policy outputs
//! while making authority transitions explicit and testable.

use std::path::{Path, PathBuf};

use crate::core::ops::UserInputProvenance;
use crate::sandbox::SandboxPolicy;
use crate::tools::spec::ToolCapability;
use crate::tui::app::AppMode;
use crate::tui::approval::ApprovalMode;
use crate::worker_profile::ShellPolicy;

use super::tool_setup::{sandbox_policy_for_mode, shell_policy_for_mode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionPosture {
    pub mode: AppMode,
    pub allow_shell: bool,
    pub shell_policy: ShellPolicy,
    pub sandbox_policy: SandboxPolicy,
    pub trust_mode: bool,
    pub auto_approve: bool,
    pub approval_mode: ApprovalMode,
}

impl ExecutionPosture {
    fn new(
        mode: AppMode,
        workspace: &Path,
        allow_shell: bool,
        trust_mode: bool,
        auto_approve: bool,
        approval_mode: ApprovalMode,
    ) -> Self {
        let (allow_shell, trust_mode, auto_approve, approval_mode) = match mode {
            AppMode::Yolo => (true, true, true, ApprovalMode::Bypass),
            AppMode::Plan => (false, false, false, ApprovalMode::Suggest),
            AppMode::Agent | AppMode::Auto => {
                let auto_approve = auto_approve;
                (
                    allow_shell,
                    trust_mode,
                    auto_approve,
                    if auto_approve {
                        ApprovalMode::Bypass
                    } else {
                        approval_mode
                    },
                )
            }
        };
        Self {
            mode,
            allow_shell,
            shell_policy: shell_policy_for_mode(mode, allow_shell),
            sandbox_policy: sandbox_policy_for_mode(mode, workspace),
            trust_mode,
            auto_approve,
            approval_mode,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnAuthority {
    pub posture: ExecutionPosture,
    pub provenance: UserInputProvenance,
    workspace: PathBuf,
    pub narrowing_reason: Option<String>,
}

impl TurnAuthority {
    pub(crate) fn for_mode(
        mode: AppMode,
        workspace: &Path,
        allow_shell: bool,
        trust_mode: bool,
        auto_approve: bool,
        approval_mode: ApprovalMode,
        provenance: UserInputProvenance,
    ) -> Self {
        Self {
            posture: ExecutionPosture::new(
                mode,
                workspace,
                allow_shell,
                trust_mode,
                auto_approve,
                approval_mode,
            ),
            provenance,
            workspace: workspace.to_path_buf(),
            narrowing_reason: None,
        }
    }

    pub(crate) fn for_input(
        provenance: UserInputProvenance,
        mode: AppMode,
        workspace: &Path,
        allow_shell: bool,
        trust_mode: bool,
        auto_approve: bool,
        approval_mode: ApprovalMode,
    ) -> Self {
        let authority = Self::for_mode(
            mode,
            workspace,
            allow_shell,
            trust_mode,
            auto_approve,
            approval_mode,
            provenance,
        );
        if provenance_can_inherit_standing_auto_authority(provenance) {
            authority
        } else {
            authority.strip_auto_authority(format!(
                "Input provenance '{}' cannot inherit standing auto-approval authority; continuing with approvals required.",
                provenance.as_str()
            ))
        }
    }

    pub(crate) fn strip_auto_authority(mut self, reason: impl Into<String>) -> Self {
        let had_auto_authority = matches!(self.posture.mode, AppMode::Yolo)
            || self.posture.trust_mode
            || self.posture.auto_approve
            || matches!(
                self.posture.approval_mode,
                ApprovalMode::Auto | ApprovalMode::Bypass
            );
        if matches!(self.posture.mode, AppMode::Yolo) {
            self.posture.mode = AppMode::Agent;
        }
        self.posture.trust_mode = false;
        self.posture.auto_approve = false;
        if matches!(
            self.posture.approval_mode,
            ApprovalMode::Auto | ApprovalMode::Bypass
        ) {
            self.posture.approval_mode = ApprovalMode::Suggest;
        }
        self.posture.shell_policy =
            shell_policy_for_mode(self.posture.mode, self.posture.allow_shell);
        self.posture.sandbox_policy = sandbox_policy_for_mode(self.posture.mode, &self.workspace);
        if had_auto_authority {
            self.narrowing_reason = Some(reason.into());
        }
        self
    }

    pub(crate) fn can_execute(&self, capability: ToolCapability) -> bool {
        match capability {
            ToolCapability::ReadOnly | ToolCapability::Sandboxable => true,
            ToolCapability::Network => {
                !matches!(self.posture.sandbox_policy, SandboxPolicy::ReadOnly)
            }
            ToolCapability::WritesFiles => !matches!(self.posture.mode, AppMode::Plan),
            ToolCapability::ExecutesCode => {
                !matches!(self.posture.mode, AppMode::Plan)
                    && self.posture.shell_policy.allows_shell()
            }
            ToolCapability::RequiresApproval => self.posture.auto_approve,
        }
    }

    pub(crate) fn to_prompt_summary(&self) -> String {
        format!(
            "mode={}, approval={:?}, shell={:?}, trust={}, provenance={}, narrowed={}",
            self.posture.mode.label(),
            self.posture.approval_mode,
            self.posture.shell_policy,
            self.posture.trust_mode,
            self.provenance.as_str(),
            self.narrowing_reason.as_deref().unwrap_or("none")
        )
    }
}

pub(crate) fn provenance_can_inherit_standing_auto_authority(
    provenance: UserInputProvenance,
) -> bool {
    matches!(
        provenance,
        UserInputProvenance::ExternalUser
            | UserInputProvenance::Runtime
            | UserInputProvenance::SubAgentHandoff
    )
}
