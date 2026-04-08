//! Account configuration for LLM API access.

use serde::{Deserialize, Serialize};

/// An account binds an API key and default model to a named provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    /// Name of the provider this account uses (e.g. `"anthropic"`).
    pub provider: String,
    /// API key for authentication.
    pub key: String,
    /// Default model for completions on this account.
    pub model: String,
    /// Maximum tokens for completions on this account.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

fn default_max_tokens() -> u32 {
    4096
}
