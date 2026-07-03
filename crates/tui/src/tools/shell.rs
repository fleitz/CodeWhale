//! Advanced shell execution with background process support and sandboxing.
//!
//! Provides:
//! - Synchronous command execution with timeout
//! - Background process execution
//! - Process output retrieval
//! - Process termination
//! - Sandbox support (macOS Seatbelt)
//! - Streaming output (future)
//!
//! This module is split into sub-modules:
//! - `types`: data structures
//! - `process`: low-level process management
//! - `background`: BackgroundShell
//! - `manager`: ShellManager
//! - `hints`: diagnostic helpers
//! - `tools`: tool implementations

mod background;
mod hints;
mod manager;
mod process;
mod tools;
mod types;

#[cfg(test)]
mod tests;

// Re-export all public types
#[allow(unused_imports)]
pub use types::{
    ShellCompletionEvent, ShellDeltaResult, ShellJobDetail, ShellJobOwner, ShellJobSnapshot,
    ShellResult, ShellStatus,
};

#[allow(unused_imports)]
pub use background::BackgroundShell;
pub use manager::{SharedShellManager, ShellManager, new_shared_shell_manager};
pub use tools::{ExecShellTool, NoteTool, ShellCancelTool, ShellInteractTool, ShellWaitTool};
