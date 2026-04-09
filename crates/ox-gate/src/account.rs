//! Account configuration for LLM API access.

use serde::{Deserialize, Serialize};

/// An account binds a named provider to its configuration.
///
/// API keys are resolved separately from key files and environment
/// variables — they do not live on this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    /// Name of the provider dialect (e.g. `"anthropic"`, `"openai"`).
    pub provider: String,
}
