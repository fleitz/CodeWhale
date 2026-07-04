//! Parent-turn sub-agent launch policy.
//!
//! The child registry still enforces per-role tool posture. This module owns
//! the engine-side contract for whether the parent turn exposes `agent`, which
//! parent posture descendants inherit, and which surface/depth settings are
//! threaded into the top-level `SubAgentRuntime`.

use crate::features::{Feature, Features};
use crate::tools::AgentToolSurfaceOptions;
use crate::tools::subagent::SubAgentType;
use crate::tui::app::AppMode;
use crate::worker_profile::{ShellPolicy, WorkerRuntimeProfile};

#[derive(Clone)]
pub(super) struct SubAgentPolicy {
    enabled: bool,
    parent_mode: AppMode,
    runtime_allow_shell: bool,
    runtime_shell_policy: ShellPolicy,
    max_spawn_depth: u32,
    parent_profile: WorkerRuntimeProfile,
    allowed_roles: Vec<SubAgentType>,
    child_surface_options: AgentToolSurfaceOptions,
}

impl SubAgentPolicy {
    pub(super) fn for_parent_turn(
        subagents_enabled: bool,
        features: &Features,
        parent_mode: AppMode,
        runtime_allow_shell: bool,
        runtime_shell_policy: ShellPolicy,
        max_spawn_depth: u32,
        child_surface_options: AgentToolSurfaceOptions,
    ) -> Self {
        let enabled = subagents_enabled && features.enabled(Feature::Subagents);
        let mut parent_profile = match parent_mode {
            AppMode::Plan => WorkerRuntimeProfile::for_role(SubAgentType::Plan),
            AppMode::Agent | AppMode::Auto | AppMode::Yolo => {
                WorkerRuntimeProfile::for_role(SubAgentType::General)
            }
        };
        parent_profile.shell = runtime_shell_policy;
        parent_profile.max_spawn_depth = max_spawn_depth;

        Self {
            enabled,
            parent_mode,
            runtime_allow_shell,
            runtime_shell_policy,
            max_spawn_depth,
            parent_profile,
            allowed_roles: vec![
                SubAgentType::Explore,
                SubAgentType::Plan,
                SubAgentType::Review,
                SubAgentType::Verifier,
                SubAgentType::Implementer,
                SubAgentType::General,
                SubAgentType::Custom,
            ],
            child_surface_options,
        }
    }

    pub(super) fn exposes_agent_tool(&self) -> bool {
        self.enabled && self.max_spawn_depth > 0
    }

    pub(super) fn parent_mode(&self) -> AppMode {
        self.parent_mode
    }

    pub(super) fn runtime_allow_shell(&self) -> bool {
        self.runtime_allow_shell
    }

    pub(super) fn runtime_shell_policy(&self) -> ShellPolicy {
        self.runtime_shell_policy
    }

    pub(super) fn max_spawn_depth(&self) -> u32 {
        self.max_spawn_depth
    }

    pub(super) fn parent_profile(&self) -> &WorkerRuntimeProfile {
        &self.parent_profile
    }

    pub(super) fn allowed_roles(&self) -> &[SubAgentType] {
        &self.allowed_roles
    }

    pub(super) fn child_surface_options(&self) -> &AgentToolSurfaceOptions {
        &self.child_surface_options
    }

    pub(super) fn child_profile_for(&self, role: SubAgentType) -> WorkerRuntimeProfile {
        let mut requested = WorkerRuntimeProfile::for_role(role);
        requested.max_spawn_depth = self.max_spawn_depth;
        self.parent_profile.derive_child(&requested)
    }

    pub(super) fn prompt_guidance_summary(&self) -> String {
        let roles = self
            .allowed_roles
            .iter()
            .map(SubAgentType::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "subagents={}, parent_mode={}, roles={}, recurse={}, shell={:?}",
            self.exposes_agent_tool(),
            self.parent_mode.as_setting(),
            roles,
            self.max_spawn_depth > 0,
            self.runtime_shell_policy
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_for(mode: AppMode) -> SubAgentPolicy {
        let runtime_allow_shell = !matches!(mode, AppMode::Plan);
        let shell_policy = if runtime_allow_shell {
            ShellPolicy::Full
        } else {
            ShellPolicy::None
        };
        SubAgentPolicy::for_parent_turn(
            true,
            &Features::with_defaults(),
            mode,
            runtime_allow_shell,
            shell_policy,
            3,
            AgentToolSurfaceOptions::new(shell_policy),
        )
    }

    #[test]
    fn policy_records_plan_agent_and_yolo_parent_postures() {
        let plan = policy_for(AppMode::Plan);
        assert!(plan.exposes_agent_tool());
        assert_eq!(plan.parent_mode(), AppMode::Plan);
        assert_eq!(plan.runtime_shell_policy(), ShellPolicy::None);
        assert!(!plan.parent_profile().permissions.write);

        let agent = policy_for(AppMode::Agent);
        assert!(agent.exposes_agent_tool());
        assert_eq!(agent.parent_mode(), AppMode::Agent);
        assert_eq!(agent.runtime_shell_policy(), ShellPolicy::Full);
        assert!(agent.parent_profile().permissions.write);

        let yolo = policy_for(AppMode::Yolo);
        assert!(yolo.exposes_agent_tool());
        assert_eq!(yolo.parent_mode(), AppMode::Yolo);
        assert_eq!(yolo.runtime_shell_policy(), ShellPolicy::Full);
        assert!(yolo.parent_profile().permissions.write);
    }

    #[test]
    fn plan_parent_cannot_spawn_write_capable_implementer() {
        let plan = policy_for(AppMode::Plan);
        let child = plan.child_profile_for(SubAgentType::Implementer);

        assert_eq!(child.role, SubAgentType::Implementer);
        assert!(!child.permissions.write);
        assert_eq!(child.shell, ShellPolicy::None);
        assert_eq!(child.max_spawn_depth, 2);
    }

    #[test]
    fn agent_and_yolo_preserve_general_child_surface() {
        for mode in [AppMode::Agent, AppMode::Yolo] {
            let policy = policy_for(mode);
            let child = policy.child_profile_for(SubAgentType::General);

            assert!(child.permissions.write, "{mode:?}");
            assert_eq!(child.shell, ShellPolicy::Full, "{mode:?}");
            assert!(policy.allowed_roles().contains(&SubAgentType::Implementer));
            assert_eq!(
                policy.child_surface_options().shell_policy,
                ShellPolicy::Full
            );
            assert!(policy.prompt_guidance_summary().contains("implementer"));
        }
    }

    #[test]
    fn policy_exposure_requires_feature_and_depth() {
        let mut disabled_features = Features::with_defaults();
        disabled_features.disable(Feature::Subagents);
        let policy = SubAgentPolicy::for_parent_turn(
            true,
            &disabled_features,
            AppMode::Agent,
            true,
            ShellPolicy::Full,
            3,
            AgentToolSurfaceOptions::new(ShellPolicy::Full),
        );
        assert!(!policy.exposes_agent_tool());

        let no_depth = SubAgentPolicy::for_parent_turn(
            true,
            &Features::with_defaults(),
            AppMode::Agent,
            true,
            ShellPolicy::Full,
            0,
            AgentToolSurfaceOptions::new(ShellPolicy::Full),
        );
        assert!(!no_depth.exposes_agent_tool());
    }
}
