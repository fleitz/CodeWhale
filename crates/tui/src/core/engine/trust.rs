//! Trust policy and approval decision helpers.
//!
//! Extracted from `core/engine.rs` (#3944). This module holds the stateless
//! functions that bridge user intent, execution policy rules, and auto-review
//! policy into concrete approval / block decisions.  Channel-based approval
//! handshakes stay in `approval.rs`.

use std::path::Path;

use codewhale_execpolicy::{AskForApproval, ExecPolicyContext};
use serde_json::Value;

use crate::core::ops::UserInputProvenance;
use crate::tui::app::AppMode;

use super::EngineConfig;

/// Compute the effective approval mode for a turn given the UI-level
/// `auto_approve` flag and the active `approval_mode`.
pub(crate) fn agent_approval_mode_for_turn(
    auto_approve: bool,
    approval_mode: crate::tui::approval::ApprovalMode,
) -> crate::tui::approval::ApprovalMode {
    if auto_approve {
        crate::tui::approval::ApprovalMode::Bypass
    } else {
        approval_mode
    }
}

// ---------------------------------------------------------------------------
// Ask-rule / exec-policy decisions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ToolAskRuleDecision {
    Prompt(String),
    Block(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AutoReviewPlanDecision {
    NoChange,
    ForcePrompt(String),
    Block(String),
}

pub(super) fn auto_review_run_origin_for_plan(
    detached_start: bool,
) -> crate::tui::auto_review::RunOrigin {
    if detached_start {
        crate::tui::auto_review::RunOrigin::Background
    } else {
        crate::tui::auto_review::RunOrigin::Interactive
    }
}

/// Thin wrapper around `AutoReviewPolicy::evaluate` that converts the
/// policy decision into an `AutoReviewPlanDecision`.
// The parameter list intentionally mirrors `AutoReviewContext::from_tool_call`,
// which this thin wrapper builds; the 8 call sites (1 prod + tests) read clearer
// passing the fields than constructing a context first.
#[allow(clippy::too_many_arguments)]
pub(super) fn auto_review_plan_decision(
    policy: &crate::tui::auto_review::AutoReviewPolicy,
    tool_name: &str,
    tool_input: &Value,
    run_origin: crate::tui::auto_review::RunOrigin,
    approval_mode: crate::tui::approval::ApprovalMode,
    user_intent: Option<&str>,
    workspace_trusted: bool,
    dirty_worktree: bool,
) -> (AutoReviewPlanDecision, Value) {
    let context = crate::tui::auto_review::AutoReviewContext::from_tool_call(
        tool_name,
        tool_input,
        run_origin,
        approval_mode,
        user_intent,
        workspace_trusted,
        dirty_worktree,
    );
    let decision = policy.evaluate(&context);
    let audit_event = policy.audit_event(&context, &decision);
    let plan_decision = match decision.action {
        crate::tui::auto_review::AutoReviewAction::Allow
        | crate::tui::auto_review::AutoReviewAction::AskUser => AutoReviewPlanDecision::NoChange,
        crate::tui::auto_review::AutoReviewAction::HoldForReview => {
            // HoldForReview only originates from the built-in safety floor
            // (configured rules produce Allow/Block), so name the gate
            // honestly instead of blaming an "auto-review policy" the user
            // may never have configured (#3883).
            let reason = format!(
                "Built-in safety gate requires approval: {}",
                decision.reason
            );
            if matches!(approval_mode, crate::tui::approval::ApprovalMode::Never) {
                AutoReviewPlanDecision::Block(reason)
            } else {
                AutoReviewPlanDecision::ForcePrompt(reason)
            }
        }
        crate::tui::auto_review::AutoReviewAction::Block => AutoReviewPlanDecision::Block(format!(
            "Auto-review policy blocked tool '{tool_name}': {}",
            decision.reason
        )),
    };
    (plan_decision, audit_event)
}

// ---------------------------------------------------------------------------
// exec_shell / file-tool ask-rule decisions
// ---------------------------------------------------------------------------

pub(super) fn exec_shell_ask_rule_decision(
    config: &EngineConfig,
    tool_name: &str,
    tool_input: &Value,
    workspace: &Path,
    approval_mode: crate::tui::approval::ApprovalMode,
) -> Option<ToolAskRuleDecision> {
    if tool_name != "exec_shell" {
        return None;
    }
    let command = tool_input.get("command").and_then(Value::as_str)?;
    tool_ask_rule_decision_for_context(config, tool_name, command, None, workspace, approval_mode)
}

pub(super) fn file_tool_ask_rule_decision(
    config: &EngineConfig,
    tool_name: &str,
    tool_input: &Value,
    workspace: &Path,
    approval_mode: crate::tui::approval::ApprovalMode,
) -> Option<ToolAskRuleDecision> {
    let paths = file_tool_permission_paths(tool_name, tool_input)?;
    if paths.is_empty() {
        return tool_ask_rule_decision_for_context(
            config,
            tool_name,
            "",
            None,
            workspace,
            approval_mode,
        );
    }

    let mut prompt: Option<String> = None;
    for path in paths {
        match tool_ask_rule_decision_for_context(
            config,
            tool_name,
            "",
            Some(&path),
            workspace,
            approval_mode,
        ) {
            Some(ToolAskRuleDecision::Block(reason)) => {
                return Some(ToolAskRuleDecision::Block(reason));
            }
            Some(ToolAskRuleDecision::Prompt(reason)) => {
                prompt.get_or_insert(reason);
            }
            None => {}
        }
    }
    prompt.map(ToolAskRuleDecision::Prompt)
}

fn tool_ask_rule_decision_for_context(
    config: &EngineConfig,
    tool_name: &str,
    command: &str,
    path: Option<&str>,
    workspace: &Path,
    approval_mode: crate::tui::approval::ApprovalMode,
) -> Option<ToolAskRuleDecision> {
    let cwd = workspace.to_string_lossy();
    let ask_for_approval = match approval_mode {
        crate::tui::approval::ApprovalMode::Never => AskForApproval::Never,
        crate::tui::approval::ApprovalMode::Auto
        | crate::tui::approval::ApprovalMode::Bypass
        | crate::tui::approval::ApprovalMode::Suggest => AskForApproval::OnFailure,
    };
    let decision = config
        .exec_policy_engine
        .check(ExecPolicyContext {
            command,
            cwd: cwd.as_ref(),
            tool: Some(tool_name),
            path,
            ask_for_approval,
            sandbox_mode: None,
        })
        .ok()?;
    if !decision.allow {
        Some(ToolAskRuleDecision::Block(decision.reason().to_string()))
    } else if decision.requires_approval {
        Some(ToolAskRuleDecision::Prompt(decision.reason().to_string()))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// File-tool path extraction helpers
// ---------------------------------------------------------------------------

fn file_tool_permission_paths(tool_name: &str, input: &Value) -> Option<Vec<String>> {
    match tool_name {
        "read_file" | "write_file" | "edit_file" | "file_search" | "grep_files" => {
            Some(string_field(input, "path").into_iter().collect())
        }
        "list_dir" => Some(vec![
            string_field(input, "path").unwrap_or_else(|| ".".to_string()),
        ]),
        "apply_patch" => Some(apply_patch_permission_paths(input)),
        _ => None,
    }
}

fn string_field(input: &Value, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn apply_patch_permission_paths(input: &Value) -> Vec<String> {
    crate::tools::apply_patch::preflight_apply_patch(input)
        .map(|preflight| preflight.touched_files)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Input provenance → effective policy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(super) struct EffectiveInputPolicy {
    pub(super) mode: AppMode,
    pub(super) allow_shell: bool,
    pub(super) trust_mode: bool,
    pub(super) auto_approve: bool,
    pub(super) approval_mode: crate::tui::approval::ApprovalMode,
    pub(super) dynamic_active_tools: Vec<&'static str>,
    pub(super) status: Option<String>,
}

pub(super) fn effective_input_policy(
    provenance: UserInputProvenance,
    requested_mode: AppMode,
    content: &str,
    allow_shell: bool,
    trust_mode: bool,
    auto_approve: bool,
    approval_mode: crate::tui::approval::ApprovalMode,
) -> EffectiveInputPolicy {
    let mut mode = requested_mode;
    let mut trust_mode = trust_mode;
    let mut auto_approve = auto_approve;
    let mut approval_mode = approval_mode;
    let dynamic_active_tools = Vec::new();
    let mut status = None;

    if !provenance_can_inherit_standing_auto_authority(provenance) {
        let had_auto_authority = matches!(mode, AppMode::Yolo)
            || trust_mode
            || auto_approve
            || matches!(approval_mode, crate::tui::approval::ApprovalMode::Bypass);
        if matches!(mode, AppMode::Yolo) {
            mode = AppMode::Agent;
        }
        trust_mode = false;
        auto_approve = false;
        if matches!(
            approval_mode,
            crate::tui::approval::ApprovalMode::Auto | crate::tui::approval::ApprovalMode::Bypass
        ) {
            approval_mode = crate::tui::approval::ApprovalMode::Suggest;
        }
        if had_auto_authority {
            status = Some(format!(
                "Input provenance '{}' cannot inherit standing auto-approval authority; continuing with approvals required.",
                provenance.as_str()
            ));
        }
    } else if matches!(provenance, UserInputProvenance::ExternalUser)
        && is_review_only_user_intent(content)
    {
        mode = AppMode::Plan;
        trust_mode = false;
        auto_approve = false;
        if matches!(
            approval_mode,
            crate::tui::approval::ApprovalMode::Auto | crate::tui::approval::ApprovalMode::Bypass
        ) {
            approval_mode = crate::tui::approval::ApprovalMode::Suggest;
        }
        status = Some(
            "Review/inspection request detected; using read-only Plan tools for this turn. Add an explicit fix/edit/commit instruction to allow writes.".to_string(),
        );
    }

    EffectiveInputPolicy {
        mode,
        allow_shell,
        trust_mode,
        auto_approve,
        approval_mode,
        dynamic_active_tools,
        status,
    }
}

fn provenance_can_inherit_standing_auto_authority(provenance: UserInputProvenance) -> bool {
    matches!(
        provenance,
        UserInputProvenance::ExternalUser
            | UserInputProvenance::Runtime
            | UserInputProvenance::SubAgentHandoff
    )
}

fn is_review_only_user_intent(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    let asks_to_inspect = [
        "look",
        "check",
        "review",
        "inspect",
        "scan",
        "audit",
        "看看",
        "看一下",
        "检查",
        "审查",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if !asks_to_inspect {
        return false;
    }

    let explicit_write = [
        "fix",
        "change",
        "update",
        "implement",
        "apply",
        "patch",
        "modify",
        "edit",
        "write",
        "commit",
        "修",
        "改",
        "补",
        "提交",
        "写",
    ]
    .iter()
    .any(|needle| lower.contains(needle));

    !explicit_write
}
