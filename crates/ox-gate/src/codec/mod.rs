//! Codec functions for LLM provider wire formats.
//!
//! Each sub-module handles a specific provider's SSE format and request shape.

pub mod anthropic;
pub mod error;
pub mod openai;
pub mod sse_encoder;
pub use error::CodecError;
pub use sse_encoder::SseEncoder;

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

impl UsageInfo {
    /// Compute aggregate usage by walking a stream of events.
    /// `InputUsage`/`OutputUsage` variants populate the corresponding fields;
    /// other events are no-ops. Later events of the same kind overwrite
    /// earlier ones (consistent with how upstream APIs emit them once).
    pub fn from_events(events: &[ox_types::StreamEvent]) -> Self {
        let mut info = Self::default();
        for ev in events {
            match ev {
                ox_types::StreamEvent::InputUsage {
                    input_tokens,
                    cache_creation,
                    cache_read,
                } => {
                    info.input_tokens = *input_tokens;
                    info.cache_creation_input_tokens = *cache_creation;
                    info.cache_read_input_tokens = *cache_read;
                }
                ox_types::StreamEvent::OutputUsage { output_tokens } => {
                    info.output_tokens = *output_tokens;
                }
                _ => {}
            }
        }
        info
    }
}
