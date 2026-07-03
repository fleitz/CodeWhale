use std::path::PathBuf;

/// Errors that can occur during tool execution.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ToolError {
    #[error("Failed to validate input: {message}")]
    InvalidInput { message: String },
    #[error("Failed to validate input: missing required field '{field}'")]
    MissingField { field: String },
    #[error("Failed to resolve path '{}': path escapes workspace", path.display())]
    PathEscape { path: PathBuf },
    #[error("Failed to execute tool: {message}")]
    ExecutionFailed { message: String },
    #[error("Failed to execute tool: operation timed out after {seconds}s")]
    Timeout { seconds: u64 },
    #[error("Failed to locate tool: {message}")]
    NotAvailable { message: String },
    #[error("Failed to authorize tool execution: {message}")]
    PermissionDenied { message: String },
}

impl ToolError {
    #[must_use]
    pub fn invalid_input(msg: impl Into<String>) -> Self {
        Self::InvalidInput {
            message: msg.into(),
        }
    }

    #[must_use]
    pub fn missing_field(field: impl Into<String>) -> Self {
        Self::MissingField {
            field: field.into(),
        }
    }

    #[must_use]
    pub fn execution_failed(msg: impl Into<String>) -> Self {
        Self::ExecutionFailed {
            message: msg.into(),
        }
    }

    #[must_use]
    pub fn path_escape(path: impl Into<PathBuf>) -> Self {
        Self::PathEscape { path: path.into() }
    }

    #[must_use]
    pub fn not_available(msg: impl Into<String>) -> Self {
        Self::NotAvailable {
            message: msg.into(),
        }
    }

    #[must_use]
    pub fn permission_denied(msg: impl Into<String>) -> Self {
        Self::PermissionDenied {
            message: msg.into(),
        }
    }
}
