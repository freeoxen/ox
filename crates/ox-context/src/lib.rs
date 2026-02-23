use ox_kernel::{CompletionRequest, ToolRegistry};

/// Minimal context assembly. No StructFS, no providers, no windowing.
///
/// Holds the base system prompt and builds CompletionRequests by
/// concatenating system prompt + history messages + tool schemas.
pub struct ContextManager {
    system_prompt: String,
    model: String,
    max_tokens: u32,
}

impl ContextManager {
    pub fn new(system_prompt: String, model: String, max_tokens: u32) -> Self {
        Self {
            system_prompt,
            model,
            max_tokens,
        }
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// Build a completion request from history messages and available tools.
    pub fn build_request(
        &self,
        messages: Vec<serde_json::Value>,
        tools: &ToolRegistry,
    ) -> CompletionRequest {
        CompletionRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            system: self.system_prompt.clone(),
            messages,
            tools: tools.schemas(),
            stream: true,
        }
    }
}
