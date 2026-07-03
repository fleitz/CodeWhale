use std::sync::Arc;

use codewhale_protocol::ToolKind;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

tokio::task_local! {
    pub(crate) static TOOL_EXECUTION_LOCK_HELD: ();
}

/// Manages concurrent tool execution via a read/write lock.
///
/// Parallel-safe tools acquire a read lock (allowing overlap), while
/// serial tools acquire a write lock (exclusive access). Reentrant calls
/// (e.g. a tool invoking another tool) skip locking to avoid deadlock.
#[derive(Debug)]
pub struct ToolCallRuntime {
    execution_lock: Arc<RwLock<()>>,
}

impl Default for ToolCallRuntime {
    fn default() -> Self {
        Self {
            execution_lock: Arc::new(RwLock::new(())),
        }
    }
}

#[derive(Debug)]
pub(crate) enum ToolExecutionGuard {
    Parallel(#[allow(dead_code)] OwnedRwLockReadGuard<()>),
    Serial(#[allow(dead_code)] OwnedRwLockWriteGuard<()>),
    Reentrant,
}

impl ToolCallRuntime {
    pub(crate) async fn acquire(&self, supports_parallel: bool) -> ToolExecutionGuard {
        if TOOL_EXECUTION_LOCK_HELD.try_with(|_| ()).is_ok() {
            return ToolExecutionGuard::Reentrant;
        }

        if supports_parallel {
            ToolExecutionGuard::Parallel(self.execution_lock.clone().read_owned().await)
        } else {
            ToolExecutionGuard::Serial(self.execution_lock.clone().write_owned().await)
        }
    }
}

pub(crate) fn tool_payload_kind(payload: &codewhale_protocol::ToolPayload) -> ToolKind {
    match payload {
        codewhale_protocol::ToolPayload::Mcp { .. } => ToolKind::Mcp,
        codewhale_protocol::ToolPayload::Function { .. }
        | codewhale_protocol::ToolPayload::Custom { .. }
        | codewhale_protocol::ToolPayload::LocalShell { .. } => ToolKind::Function,
    }
}
