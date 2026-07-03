//! Tool catalog policy: building, deferral, and filtering.
//!
//! Extracted from `tool_catalog.rs` (#3940). This module owns the catalog-level
//! policy decisions — which tools are core, what gets deferred, how the catalog
//! is assembled from native and MCP tools, and how allow/deny gates filter it.
//! The operational helpers (tool search, hydration, consistency checks) remain
//! in `tool_catalog.rs`.

use std::collections::HashSet;

use serde_json::json;

use crate::model_profile::ToolSurfaceBudget;
use crate::models::Tool;
use crate::tui::app::AppMode;

pub(super) const MULTI_TOOL_PARALLEL_NAME: &str = "multi_tool_use.parallel";
pub(super) const REQUEST_USER_INPUT_NAME: &str = "request_user_input";
pub(super) const CODE_EXECUTION_TOOL_NAME: &str = "code_execution";
const CODE_EXECUTION_TOOL_TYPE: &str = "code_execution_20250825";
pub(super) use crate::tools::js_execution::JS_EXECUTION_TOOL_NAME;
pub(super) const TOOL_SEARCH_NAME: &str = "tool_search";
const TOOL_SEARCH_TYPE: &str = "tool_search_20251119";
pub(super) const LEGACY_TOOL_SEARCH_REGEX_NAME: &str = "tool_search_tool_regex";
pub(super) const LEGACY_TOOL_SEARCH_BM25_NAME: &str = "tool_search_tool_bm25";
pub(super) const TOOL_SEARCH_DEFAULT_MAX_RESULTS: usize = 20;
pub(super) const TOOL_SEARCH_MAX_RESULTS_LIMIT: usize = 100;

const SUBAGENT_ALLOWED_ROLES: &[&str] = &[
    "general",
    "explore",
    "plan",
    "review",
    "implementer",
    "verifier",
    "custom",
];

const PLAN_PROMPT_VISIBLE_NATIVE_TOOLS: &[&str] = &[
    "checklist_write",
    "fetch_url",
    "file_search",
    "git_diff",
    "git_log",
    "git_show",
    "git_status",
    "grep_files",
    "handle_read",
    "list_dir",
    "read_file",
    "task_list",
    "task_read",
    "update_plan",
    "web_search",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ToolSurfacePolicy {
    mode: AppMode,
    allowed_tools: Option<Vec<String>>,
    disallowed_tools: Option<Vec<String>>,
    surface_budget: ToolSurfaceBudget,
    pub(super) capabilities: ToolSurfaceCapabilities,
    pub(super) approvals: ToolApprovalPolicy,
    pub(super) subagents: SubAgentSurfacePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ToolSurfaceCapabilities {
    pub(super) read: bool,
    pub(super) write: bool,
    pub(super) shell: bool,
    pub(super) network: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ToolApprovalPolicy {
    pub(super) write_requires_approval: bool,
    pub(super) shell_requires_approval: bool,
    pub(super) auto_approve: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SubAgentSurfacePolicy {
    pub(super) available: bool,
    pub(super) allowed_roles: Vec<&'static str>,
}

impl ToolSurfacePolicy {
    pub(super) fn for_turn(
        mode: AppMode,
        allow_shell: bool,
        approval_mode: crate::tui::approval::ApprovalMode,
        auto_approve: bool,
        surface_budget: ToolSurfaceBudget,
        allowed_tools: Option<&[String]>,
        disallowed_tools: Option<&[String]>,
    ) -> Self {
        let write = !matches!(mode, AppMode::Plan);
        let shell = allow_shell && !matches!(mode, AppMode::Plan);
        let network = !matches!(mode, AppMode::Plan) || prompt_claims_include_network(mode);
        let auto_approve = auto_approve || matches!(mode, AppMode::Yolo);
        let requires_approval = !auto_approve
            && !matches!(
                approval_mode,
                crate::tui::approval::ApprovalMode::Auto
                    | crate::tui::approval::ApprovalMode::Bypass
            );
        let subagents_available = !matches!(mode, AppMode::Plan)
            && tool_allowed_by_gates("agent", allowed_tools, disallowed_tools);

        Self {
            mode,
            allowed_tools: allowed_tools.map(<[String]>::to_vec),
            disallowed_tools: disallowed_tools.map(<[String]>::to_vec),
            surface_budget,
            capabilities: ToolSurfaceCapabilities {
                read: true,
                write,
                shell,
                network,
            },
            approvals: ToolApprovalPolicy {
                write_requires_approval: write && requires_approval,
                shell_requires_approval: shell && requires_approval,
                auto_approve,
            },
            subagents: SubAgentSurfacePolicy {
                available: subagents_available,
                allowed_roles: if subagents_available {
                    SUBAGENT_ALLOWED_ROLES.to_vec()
                } else {
                    Vec::new()
                },
            },
        }
    }

    pub(super) fn for_prompt_mode(
        mode: AppMode,
        allow_shell: bool,
        allowed_tools: Option<&[String]>,
        disallowed_tools: Option<&[String]>,
    ) -> Self {
        Self::for_turn(
            mode,
            allow_shell,
            crate::tui::approval::ApprovalMode::Suggest,
            false,
            ToolSurfaceBudget::Standard,
            allowed_tools,
            disallowed_tools,
        )
    }

    pub(super) fn build_catalog(
        &self,
        native_tools: Vec<Tool>,
        mcp_tools: Vec<Tool>,
        always_load: &HashSet<String>,
        plugin_tool_names: &HashSet<String>,
    ) -> Vec<Tool> {
        let mut catalog = build_model_tool_catalog_with_surface(
            native_tools,
            mcp_tools,
            self.mode,
            always_load,
            self.surface_budget,
        );
        for tool in &mut catalog {
            if plugin_tool_names.contains(&tool.name) {
                tool.defer_loading = Some(false);
            }
        }
        filter_tool_catalog_for_gates(
            &mut catalog,
            self.allowed_tools.as_deref(),
            self.disallowed_tools.as_deref(),
        );
        catalog
    }

    #[cfg(test)]
    pub(super) fn prompt_visible_tool_claims(&self, catalog: &[Tool]) -> Vec<String> {
        let catalog_names: HashSet<&str> = catalog.iter().map(|tool| tool.name.as_str()).collect();
        self.default_prompt_tool_claims()
            .into_iter()
            .filter(|name| catalog_names.contains(*name))
            .map(ToString::to_string)
            .collect()
    }

    #[cfg(test)]
    pub(super) fn prompt_claim_mismatches(&self, catalog: &[Tool]) -> Vec<String> {
        let catalog_names: HashSet<&str> = catalog.iter().map(|tool| tool.name.as_str()).collect();
        self.default_prompt_tool_claims()
            .into_iter()
            .filter(|name| !catalog_names.contains(*name))
            .map(ToString::to_string)
            .collect()
    }

    pub(super) fn render_mode_instructions(&self, base: &str) -> String {
        let mut rendered = base.trim().to_string();
        if !self.subagents.available {
            rendered = rendered.replace(
                "Spawn read-only sub-agents for parallel investigation.",
                "Do not spawn sub-agents in this mode; the `agent` tool is not available.",
            );
            rendered = rendered.replace("sub-agent session open, or ", "");
            rendered = rendered.replace(
                "Open sub-agent sessions for independent work instead of doing everything sequentially\n",
                "",
            );
        }
        if !self.capabilities.shell {
            rendered = rendered.replace("shell execution, ", "");
            rendered = rendered.replace(
                "Shell and code execution are unavailable.",
                "Shell and code execution are unavailable; do not recommend shell or code-execution tools.",
            );
        }

        rendered.push_str("\n\n");
        rendered.push_str(&self.render_prompt_surface_block());
        rendered
    }

    fn render_prompt_surface_block(&self) -> String {
        let subagents = if self.subagents.available {
            format!(
                "available via `agent` for roles: {}",
                self.subagents.allowed_roles.join(", ")
            )
        } else {
            "unavailable; do not recommend `agent`".to_string()
        };
        let prompt_claims = self.default_prompt_tool_claims().join(", ");
        format!(
            "Tool surface policy: read={}, write={}, shell={}, network={}, subagents={}. Policy prompt tool claims: {}.",
            self.capabilities.read,
            self.capabilities.write,
            self.capabilities.shell,
            self.capabilities.network,
            subagents,
            prompt_claims,
        )
    }

    fn default_prompt_tool_claims(&self) -> Vec<&'static str> {
        let source = if matches!(self.mode, AppMode::Plan) {
            PLAN_PROMPT_VISIBLE_NATIVE_TOOLS
        } else {
            DEFAULT_ACTIVE_NATIVE_TOOLS
        };
        source
            .iter()
            .copied()
            .filter(|name| {
                tool_allowed_by_gates(
                    name,
                    self.allowed_tools.as_deref(),
                    self.disallowed_tools.as_deref(),
                )
            })
            .filter(|name| self.subagents.available || *name != "agent")
            .filter(|name| self.capabilities.shell || !is_shell_tool_name(name))
            .collect()
    }
}

fn prompt_claims_include_network(mode: AppMode) -> bool {
    !matches!(mode, AppMode::Plan)
}

fn is_shell_tool_name(name: &str) -> bool {
    matches!(
        name,
        "exec_shell"
            | "exec_shell_wait"
            | "exec_shell_interact"
            | "exec_shell_cancel"
            | "exec_wait"
            | "exec_interact"
            | "task_shell_start"
            | "task_shell_wait"
    )
}

fn tool_allowed_by_gates(
    name: &str,
    allowed_tools: Option<&[String]>,
    disallowed_tools: Option<&[String]>,
) -> bool {
    !super::turn_loop::command_denies_tool(disallowed_tools, name)
        && super::turn_loop::command_allows_tool(allowed_tools, name)
}

pub(super) fn is_tool_search_tool(name: &str) -> bool {
    matches!(
        name,
        TOOL_SEARCH_NAME | LEGACY_TOOL_SEARCH_REGEX_NAME | LEGACY_TOOL_SEARCH_BM25_NAME
    )
}

pub(super) const DEFAULT_ACTIVE_NATIVE_TOOLS: &[&str] = &[
    "agent",
    "apply_patch",
    "checklist_write",
    "edit_file",
    "exec_interact",
    "exec_shell",
    "exec_shell_interact",
    "exec_shell_wait",
    "exec_wait",
    "fetch_url",
    "file_search",
    "git_diff",
    "git_log",
    "git_show",
    "git_status",
    "grep_files",
    "list_dir",
    "read_file",
    "run_tests",
    "run_verifiers",
    "task_create",
    "task_list",
    "task_read",
    "update_plan",
    "wait_for_dev_server",
    "web_search",
    "write_file",
];

pub(super) const CORE_ACTION_TOOL_FALLBACKS: &[CoreActionToolFallback] = &[
    CoreActionToolFallback {
        name: "exec_shell",
        description: "Run shell commands in the workspace.",
        unavailable_reason: "Not present in the current model-visible catalog. Interactive Agent sessions expose shell by default unless allow_shell = false; noninteractive and durable profiles require allow_shell = true. Plan mode hides shell, and command tool allow/deny gates can also block it.",
    },
    CoreActionToolFallback {
        name: "write_file",
        description: "Create or overwrite files in the workspace.",
        unavailable_reason: "Not present in the current model-visible catalog. File writes require Agent or Yolo mode and no command tool allow/deny gate blocking write_file.",
    },
    CoreActionToolFallback {
        name: "edit_file",
        description: "Edit existing files by replacing text.",
        unavailable_reason: "Not present in the current model-visible catalog. File edits require Agent or Yolo mode and no command tool allow/deny gate blocking edit_file.",
    },
    CoreActionToolFallback {
        name: "apply_patch",
        description: "Apply a patch to one or more workspace files.",
        unavailable_reason: "Not present in the current model-visible catalog. Patches require Agent or Yolo mode, the apply_patch feature, and no command tool allow/deny gate blocking apply_patch.",
    },
];

#[derive(Debug, Clone, Copy)]
pub(super) struct CoreActionToolFallback {
    pub(super) name: &'static str,
    pub(super) description: &'static str,
    pub(super) unavailable_reason: &'static str,
}

pub(super) fn should_default_defer_tool(name: &str, always_load: &HashSet<String>) -> bool {
    if always_load.contains(name) {
        return false;
    }

    if is_tool_search_tool(name) {
        return false;
    }

    !DEFAULT_ACTIVE_NATIVE_TOOLS
        .iter()
        .any(|core_tool| core_tool == &name)
}

pub(super) fn apply_native_tool_deferral(catalog: &mut [Tool], always_load: &HashSet<String>) {
    for tool in catalog {
        tool.defer_loading = Some(should_default_defer_tool(&tool.name, always_load));
    }
}

fn should_keep_mcp_tool_loaded(name: &str) -> bool {
    matches!(
        name,
        "list_mcp_resources"
            | "list_mcp_resource_templates"
            | "mcp_read_resource"
            | "read_mcp_resource"
            | "mcp_get_prompt"
    )
}

pub(super) fn apply_mcp_tool_deferral(catalog: &mut [Tool], mode: AppMode) {
    for tool in catalog {
        tool.defer_loading =
            Some(mode != AppMode::Yolo && !should_keep_mcp_tool_loaded(&tool.name));
    }
}

/// Build the model tool catalog from native and MCP tool lists.
///
/// **Catalog-head stability invariant.** The head of the catalog (all
/// non-deferred tools) must remain byte-identical across mode toggles
/// (Plan ↔ Agent ↔ YOLO) for tools that are common to both modes.
/// Deferred tool activations append to the tail and never reorder the
/// head. This invariant is critical for DeepSeek's KV prefix cache:
/// the tools array is part of the immutable prefix, and any byte-level
/// change in the head forces a full re-prefill on the next turn.
#[cfg(test)]
pub(super) fn build_model_tool_catalog(
    native_tools: Vec<Tool>,
    mcp_tools: Vec<Tool>,
    mode: AppMode,
    always_load: &HashSet<String>,
) -> Vec<Tool> {
    build_model_tool_catalog_with_surface(
        native_tools,
        mcp_tools,
        mode,
        always_load,
        ToolSurfaceBudget::Standard,
    )
}

pub(super) fn build_model_tool_catalog_with_surface(
    mut native_tools: Vec<Tool>,
    mut mcp_tools: Vec<Tool>,
    mode: AppMode,
    always_load: &HashSet<String>,
    surface_budget: ToolSurfaceBudget,
) -> Vec<Tool> {
    apply_native_tool_deferral(&mut native_tools, always_load);
    apply_mcp_tool_deferral(&mut mcp_tools, mode);
    apply_tool_surface_budget(&mut native_tools, surface_budget, always_load);
    apply_tool_surface_budget(&mut mcp_tools, surface_budget, always_load);
    // Sort each partition by name for prefix-cache stability (#263). The
    // upstream `to_api_tools()` already sorts the registry's HashMap output;
    // this catalog is built from caller-supplied Vecs which the test harness
    // and (future) caller refactors may not pre-sort. Built-ins stay as a
    // contiguous prefix ahead of MCP tools so adding/removing an MCP tool
    // never shifts a built-in's position.
    native_tools.sort_by(|a, b| a.name.cmp(&b.name));
    mcp_tools.sort_by(|a, b| a.name.cmp(&b.name));
    native_tools.extend(mcp_tools);
    native_tools
}

fn apply_tool_surface_budget(
    catalog: &mut [Tool],
    surface_budget: ToolSurfaceBudget,
    always_load: &HashSet<String>,
) {
    if !matches!(surface_budget, ToolSurfaceBudget::Compact) {
        return;
    }
    for tool in catalog {
        if always_load.contains(&tool.name) {
            continue;
        }
        if matches!(
            tool.name.as_str(),
            "agent" | "run_tests" | "run_verifiers" | "task_create" | "web_search"
        ) {
            tool.defer_loading = Some(true);
        }
    }
}

pub(super) fn ensure_advanced_tooling(
    catalog: &mut Vec<Tool>,
    mode: AppMode,
    always_load: &HashSet<String>,
) {
    // code_execution depends on a locally-installed Python interpreter
    // (python3 / python / py -3). Before v0.8.31, the tool was always
    // advertised and would fail at execution time on Windows where
    // `python3` isn't on PATH — the model treated the tool as reliable
    // once it appeared in the catalog. We now probe at catalog-build
    // time and only advertise when an interpreter resolves. See
    // `crate::dependencies::resolve_python_interpreter` for the probe.
    if mode != AppMode::Plan
        && !catalog.iter().any(|t| t.name == CODE_EXECUTION_TOOL_NAME)
        && crate::dependencies::resolve_python_interpreter().is_some()
    {
        catalog.push(Tool {
            tool_type: Some(CODE_EXECUTION_TOOL_TYPE.to_string()),
            name: CODE_EXECUTION_TOOL_NAME.to_string(),
            description: "Execute Python code in a local sandboxed runtime and return stdout/stderr/return_code as JSON.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "Python source code to execute." }
                },
                "required": ["code"]
            }),
            allowed_callers: Some(vec!["direct".to_string()]),
            defer_loading: Some(should_default_defer_tool(
                CODE_EXECUTION_TOOL_NAME,
                always_load,
            )),
            input_examples: None,
            strict: None,
            cache_control: None,
        });
    }

    // js_execution mirrors code_execution: gate on Node.js being
    // present locally so the model never sees a runtime it can't
    // actually use. Plan mode hides shell/exec surfaces (including
    // both interpreter tools) by construction; Agent / YOLO advertise
    // the tool only when `resolve_node()` succeeds.
    if mode != AppMode::Plan
        && !catalog.iter().any(|t| t.name == JS_EXECUTION_TOOL_NAME)
        && crate::dependencies::resolve_node().is_some()
    {
        let mut tool = crate::tools::js_execution::js_execution_tool_definition();
        tool.defer_loading = Some(should_default_defer_tool(&tool.name, always_load));
        catalog.push(tool);
    }

    if !catalog.iter().any(|t| t.name == TOOL_SEARCH_NAME) {
        catalog.push(Tool {
            tool_type: Some(TOOL_SEARCH_TYPE.to_string()),
            name: TOOL_SEARCH_NAME.to_string(),
            description: "Search deferred tool definitions and return matching tool references.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query for tool discovery." },
                    "match": {
                        "type": "string",
                        "enum": ["bm25", "regex"],
                        "default": "bm25",
                        "description": "Matching algorithm: bm25 for natural-language matching, regex for a regular expression over tool names/descriptions/schema."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": TOOL_SEARCH_MAX_RESULTS_LIMIT,
                        "default": TOOL_SEARCH_DEFAULT_MAX_RESULTS,
                        "description": "Maximum number of matching tool references to return."
                    }
                },
                "required": ["query"]
            }),
            allowed_callers: Some(vec!["direct".to_string()]),
            defer_loading: Some(false),
            input_examples: None,
            strict: None,
            cache_control: None,
        });
    }
}

/// Drop catalog entries the execution gates would reject (#3027): the model
/// should never be advertised a tool it cannot call. Deny wins over allow.
pub(super) fn filter_tool_catalog_for_gates(
    catalog: &mut Vec<Tool>,
    allowed_tools: Option<&[String]>,
    disallowed_tools: Option<&[String]>,
) {
    catalog.retain(|tool| {
        !super::turn_loop::command_denies_tool(disallowed_tools, &tool.name)
            && super::turn_loop::command_allows_tool(allowed_tools, &tool.name)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_tool(name: &str) -> Tool {
        Tool {
            tool_type: Some("function".to_string()),
            name: name.to_string(),
            description: format!("Test tool {name}"),
            input_schema: json!({"type": "object"}),
            allowed_callers: Some(vec!["direct".to_string()]),
            defer_loading: None,
            input_examples: None,
            strict: None,
            cache_control: None,
        }
    }

    #[test]
    fn plan_policy_removes_subagent_prompt_claim_when_agent_is_not_available() {
        let policy = ToolSurfacePolicy::for_prompt_mode(AppMode::Plan, true, None, None);
        assert!(!policy.subagents.available);

        let rendered = policy.render_mode_instructions(crate::prompts::PLAN_MODE);
        assert!(!rendered.contains("Spawn read-only sub-agents"));
        assert!(rendered.contains("do not recommend `agent`"));

        let catalog = policy.build_catalog(
            PLAN_PROMPT_VISIBLE_NATIVE_TOOLS
                .iter()
                .copied()
                .map(api_tool)
                .collect(),
            Vec::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        let claims = policy.prompt_visible_tool_claims(&catalog);
        assert!(!claims.iter().any(|name| name == "agent"));
        assert!(
            policy.prompt_claim_mismatches(&catalog).is_empty(),
            "Plan prompt claims must be backed by the Plan catalog"
        );
    }

    #[test]
    fn prompt_claims_follow_allow_deny_gates_used_by_catalog() {
        let disallowed = vec!["exec_shell".to_string(), "write_file".to_string()];
        let policy = ToolSurfacePolicy::for_turn(
            AppMode::Agent,
            true,
            crate::tui::approval::ApprovalMode::Suggest,
            false,
            ToolSurfaceBudget::Standard,
            None,
            Some(&disallowed),
        );

        let catalog = policy.build_catalog(
            DEFAULT_ACTIVE_NATIVE_TOOLS
                .iter()
                .copied()
                .map(api_tool)
                .collect(),
            Vec::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        let catalog_names: HashSet<&str> = catalog.iter().map(|tool| tool.name.as_str()).collect();
        assert!(!catalog_names.contains("exec_shell"));
        assert!(!catalog_names.contains("write_file"));

        let claims = policy.prompt_visible_tool_claims(&catalog);
        assert!(!claims.iter().any(|name| name == "exec_shell"));
        assert!(!claims.iter().any(|name| name == "write_file"));
        assert!(
            policy.prompt_claim_mismatches(&catalog).is_empty(),
            "prompt-visible claims must use the same allow/deny gates as the catalog"
        );
    }
}
