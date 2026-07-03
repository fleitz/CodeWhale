use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Specification that describes a tool available in the registry.
///
/// Contains the tool's name, its JSON input/output schemas, and
/// execution constraints such as timeout and parallelism.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Unique name used to look up the tool.
    pub name: String,
    /// JSON Schema describing the tool's expected input parameters.
    pub input_schema: Value,
    /// JSON Schema describing the tool's output format.
    pub output_schema: Value,
    /// Whether multiple invocations of this tool may run concurrently.
    pub supports_parallel_tool_calls: bool,
    /// Optional per-call timeout in milliseconds; `None` means no timeout.
    pub timeout_ms: Option<u64>,
}

/// A [`ToolSpec`] together with its runtime configuration.
///
/// Wraps a `ToolSpec` and exposes the parallelism flag directly so the
/// dispatcher can check it without digging into the inner spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfiguredToolSpec {
    /// The underlying tool specification.
    pub spec: ToolSpec,
    /// Whether this tool supports concurrent invocations.
    pub supports_parallel_tool_calls: bool,
}
