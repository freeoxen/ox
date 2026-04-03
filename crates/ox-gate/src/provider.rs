//! Provider configuration for LLM API endpoints.

use serde::{Deserialize, Serialize};

/// Configuration for an LLM provider endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Wire format dialect: `"anthropic"` or `"openai"`.
    pub dialect: String,
    /// API endpoint URL.
    pub endpoint: String,
    /// API version header (e.g. `"2023-06-01"` for Anthropic; empty for OpenAI).
    pub version: String,
}

impl ProviderConfig {
    /// Default configuration for the Anthropic Messages API.
    pub fn anthropic() -> Self {
        Self {
            dialect: "anthropic".to_string(),
            endpoint: "https://api.anthropic.com/v1/messages".to_string(),
            version: "2023-06-01".to_string(),
        }
    }

    /// Default configuration for the OpenAI Chat Completions API.
    pub fn openai() -> Self {
        Self {
            dialect: "openai".to_string(),
            endpoint: "https://api.openai.com/v1/chat/completions".to_string(),
            version: String::new(),
        }
    }
}
