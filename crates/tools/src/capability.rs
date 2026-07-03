/// Capabilities that a tool may have or require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolCapability {
    /// Tool only reads data, never modifies state.
    ReadOnly,
    /// Tool writes to the filesystem.
    WritesFiles,
    /// Tool executes arbitrary shell commands.
    ExecutesCode,
    /// Tool makes network requests.
    Network,
    /// Tool can be run in a sandbox.
    Sandboxable,
    /// Tool requires user approval before execution.
    RequiresApproval,
}

/// Approval requirement for a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalRequirement {
    /// Never needs approval: safe read-only operations.
    #[default]
    Auto,
    /// Suggest approval but allow user to skip.
    Suggest,
    /// Always require explicit user approval.
    Required,
}
