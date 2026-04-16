//! Codec functions for LLM provider wire formats.
//!
//! Each sub-module handles a specific provider's SSE format and request shape.

pub mod anthropic;
pub mod openai;

/// Token usage information from a completion response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageInfo {
    /// Number of input (prompt) tokens consumed.
    pub input_tokens: u32,
    /// Number of output (completion) tokens generated.
    pub output_tokens: u32,
    /// Tokens used to create a new cache entry (Anthropic).
    pub cache_creation_input_tokens: u32,
    /// Tokens read from an existing cache entry (Anthropic).
    pub cache_read_input_tokens: u32,
}
