//! Completion tools — delegate sub-completions to named accounts.

use std::sync::Arc;

use ox_kernel::{CompletionRequest, FnTool, StreamEvent, ToolSchema};

use crate::account::AccountConfig;
use crate::provider::ProviderConfig;

/// A synchronous send function shared across completion tools.
pub type SendFn = dyn Fn(&CompletionRequest) -> Result<Vec<StreamEvent>, String> + Send + Sync;

/// The tool name for an account (e.g. `"complete_openai"`).
pub fn tool_name_for(account_name: &str) -> String {
    format!("complete_{account_name}")
}

/// Generate a [`ToolSchema`] for an account without needing a send function.
pub fn completion_tool_schema(account_name: &str, provider: &ProviderConfig) -> ToolSchema {
    let schema = completion_params_schema();
    ToolSchema {
        name: tool_name_for(account_name),
        description: format!(
            "Send a completion to the {} account ({} dialect)",
            account_name, provider.dialect,
        ),
        input_schema: schema,
    }
}

/// Create a completion tool for the given account.
///
/// Returns a [`FnTool`] that delegates sub-completions through `send`.
pub fn completion_tool(
    account_name: String,
    account: &AccountConfig,
    provider: &ProviderConfig,
    send: Arc<SendFn>,
) -> FnTool {
    // TODO(task-9): accept model as a parameter instead of hardcoding
    let _ = account;
    let model = "claude-sonnet-4-20250514".to_string();
    let description = format!(
        "Send a completion to the {} account ({} dialect)",
        account_name, provider.dialect,
    );
    FnTool::new(
        tool_name_for(&account_name),
        description,
        completion_params_schema(),
        move |input| {
            let prompt = input
                .get("prompt")
                .and_then(|v| v.as_str())
                .ok_or("missing required 'prompt' field")?;
            let system = input.get("system").and_then(|v| v.as_str()).unwrap_or("");
            complete_via_gate(&*send, &model, prompt, system)
        },
    )
}

fn completion_params_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "prompt": {
                "type": "string",
                "description": "The user prompt to send"
            },
            "system": {
                "type": "string",
                "description": "Optional system prompt"
            }
        },
        "required": ["prompt"]
    })
}

/// Execute a sub-completion through a send function.
///
/// Builds a [`CompletionRequest`], sends it, and accumulates the text response.
pub fn complete_via_gate(
    send: &dyn Fn(&CompletionRequest) -> Result<Vec<StreamEvent>, String>,
    model: &str,
    prompt: &str,
    system: &str,
) -> Result<String, String> {
    let request = CompletionRequest {
        model: model.to_string(),
        max_tokens: 4096,
        system: system.to_string(),
        messages: vec![serde_json::json!({"role": "user", "content": prompt})],
        tools: vec![],
        stream: true,
    };

    let events = send(&request)?;

    let text: String = events
        .iter()
        .filter_map(|e| {
            if let StreamEvent::TextDelta(t) = e {
                Some(t.as_str())
            } else {
                None
            }
        })
        .collect();

    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_kernel::Tool;

    fn mock_send(request: &CompletionRequest) -> Result<Vec<StreamEvent>, String> {
        let content = request.messages[0]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();
        Ok(vec![
            StreamEvent::TextDelta(format!("Response to: {content}")),
            StreamEvent::MessageStop,
        ])
    }

    #[test]
    fn complete_via_gate_basic() {
        let result = complete_via_gate(&mock_send, "test-model", "Hello", "").unwrap();
        assert_eq!(result, "Response to: Hello");
    }

    #[test]
    fn complete_via_gate_with_system() {
        let send = |req: &CompletionRequest| -> Result<Vec<StreamEvent>, String> {
            assert_eq!(req.system, "Be brief");
            Ok(vec![
                StreamEvent::TextDelta("OK".to_string()),
                StreamEvent::MessageStop,
            ])
        };
        let result = complete_via_gate(&send, "test-model", "Hi", "Be brief").unwrap();
        assert_eq!(result, "OK");
    }

    #[test]
    fn completion_tool_execute() {
        let account = AccountConfig {
            provider: "test".to_string(),
            key: "sk-test".to_string(),
        };
        let provider = ProviderConfig::anthropic();
        let send: Arc<SendFn> = Arc::new(mock_send);

        let tool = completion_tool("test".to_string(), &account, &provider, send);
        let result = tool
            .execute(serde_json::json!({"prompt": "Hello"}))
            .unwrap();
        assert_eq!(result, "Response to: Hello");
    }

    #[test]
    fn completion_tool_missing_prompt() {
        let account = AccountConfig {
            provider: "test".to_string(),
            key: "sk-test".to_string(),
        };
        let provider = ProviderConfig::anthropic();
        let send: Arc<SendFn> = Arc::new(mock_send);

        let tool = completion_tool("test".to_string(), &account, &provider, send);
        let result = tool.execute(serde_json::json!({}));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("prompt"));
    }

    #[test]
    fn schema_for_generates_correct_name() {
        let schema = completion_tool_schema("openai", &ProviderConfig::openai());
        assert_eq!(schema.name, "complete_openai");
        assert!(schema.description.contains("openai"));
        assert!(
            schema.input_schema["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("prompt"))
        );
    }

    #[test]
    fn tool_name_format() {
        assert_eq!(tool_name_for("anthropic"), "complete_anthropic");
        assert_eq!(tool_name_for("openai"), "complete_openai");
    }
}
