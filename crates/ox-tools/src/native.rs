//! Native (in-process) tool support.
//!
//! Native tools execute directly in the agent's process — no subprocess,
//! no sandbox. Used for: browser JS callbacks, Rust closures, completion
//! tools, or any tool where subprocess isolation isn't needed or possible.

use crate::ToolSchemaEntry;

/// A tool that executes in-process.
pub trait NativeTool: Send + Sync {
    /// Execute the tool with JSON input, returning JSON output.
    fn execute(&self, input: serde_json::Value) -> Result<serde_json::Value, String>;

    /// The tool's schema for model consumption.
    fn schema(&self) -> ToolSchemaEntry;
}

/// Closure-backed native tool.
pub struct FnTool {
    wire_name: String,
    internal_path: String,
    description: String,
    input_schema: serde_json::Value,
    run: Box<dyn Fn(serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync>,
}

impl FnTool {
    pub fn new(
        wire_name: impl Into<String>,
        internal_path: impl Into<String>,
        description: impl Into<String>,
        input_schema: serde_json::Value,
        run: impl Fn(serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            wire_name: wire_name.into(),
            internal_path: internal_path.into(),
            description: description.into(),
            input_schema,
            run: Box::new(run),
        }
    }
}

impl NativeTool for FnTool {
    fn execute(&self, input: serde_json::Value) -> Result<serde_json::Value, String> {
        (self.run)(input)
    }

    fn schema(&self) -> ToolSchemaEntry {
        ToolSchemaEntry {
            wire_name: self.wire_name.clone(),
            internal_path: self.internal_path.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fn_tool_executes() {
        let tool = FnTool::new(
            "test",
            "custom/test",
            "test tool",
            serde_json::json!({}),
            |_| Ok(serde_json::json!({"ok": true})),
        );
        let result = tool.execute(serde_json::json!({})).unwrap();
        assert_eq!(result, serde_json::json!({"ok": true}));
    }

    #[test]
    fn fn_tool_schema() {
        let tool = FnTool::new(
            "test",
            "custom/test",
            "test tool",
            serde_json::json!({"type": "object"}),
            |_| Ok(serde_json::json!({})),
        );
        let schema = tool.schema();
        assert_eq!(schema.wire_name, "test");
        assert_eq!(schema.internal_path, "custom/test");
        assert_eq!(schema.description, "test tool");
    }

    #[test]
    fn fn_tool_error_propagates() {
        let tool = FnTool::new(
            "fail",
            "custom/fail",
            "always fails",
            serde_json::json!({}),
            |_| Err("boom".to_string()),
        );
        let result = tool.execute(serde_json::json!({}));
        assert_eq!(result, Err("boom".to_string()));
    }
}
