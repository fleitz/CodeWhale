use async_trait::async_trait;
use codewhale_protocol::{ToolKind, ToolOutput};

use crate::call::{FunctionCallError, ToolInvocation};

/// Trait implemented by concrete tool handlers.
///
/// Each registered tool is backed by a handler that reports its kind,
/// whether it is mutating, and performs the actual execution.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// The [`ToolKind`] this handler expects (e.g. `Function` or `Mcp`).
    fn kind(&self) -> ToolKind;

    /// Returns `true` if `kind` matches this handler's expected kind.
    ///
    /// The default implementation compares against [`kind()`](ToolHandler::kind).
    fn matches_kind(&self, kind: ToolKind) -> bool {
        self.kind() == kind
    }

    /// Whether this tool performs side-effects that require user approval.
    ///
    /// Defaults to `false` (read-only / safe).
    fn is_mutating(&self) -> bool {
        false
    }

    /// Execute the tool with the given invocation context.
    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> std::result::Result<ToolOutput, FunctionCallError>;
}
