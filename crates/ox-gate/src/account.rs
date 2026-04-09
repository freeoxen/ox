//! Account configuration for LLM API access.

use serde::{Deserialize, Serialize};

/// An account binds an API key to a named provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    /// Name of the provider this account uses (e.g. `"anthropic"`).
    pub provider: String,
    /// API key for authentication.
    #[serde(default)]
    pub key: String,
}
