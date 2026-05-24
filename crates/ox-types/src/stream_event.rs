//! A single event from a streaming completion response.
//!
//! Crosses the StructFS substrate boundary as a typed record — promoted
//! from a kernel-internal enum so consumers on either side of a
//! Reader/Writer call can roundtrip the same shape.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    TextDelta { text: String },
    ToolUseStart { id: String, name: String },
    ToolUseInputDelta { delta: String },
    MessageStop,
    Error { message: String },
    /// Input-side usage (input tokens, cache creation, cache read).
    /// Emitted by upstream SSE parsers at message_start (Anthropic) or
    /// when prompt_tokens lands (OpenAI).
    InputUsage {
        input_tokens: u32,
        cache_creation: u32,
        cache_read: u32,
    },
    /// Output-side usage (completion tokens). Emitted at message_delta
    /// (Anthropic) or final usage block (OpenAI).
    OutputUsage { output_tokens: u32 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_delta_json_roundtrip() {
        let ev = StreamEvent::TextDelta { text: "hello".into() };
        let s = serde_json::to_string(&ev).unwrap();
        let back: StreamEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(ev, back);
    }

    #[test]
    fn input_usage_json_roundtrip() {
        let ev = StreamEvent::InputUsage {
            input_tokens: 100,
            cache_creation: 50,
            cache_read: 25,
        };
        let s = serde_json::to_string(&ev).unwrap();
        let back: StreamEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(ev, back);
    }

    #[test]
    fn output_usage_json_roundtrip() {
        let ev = StreamEvent::OutputUsage { output_tokens: 42 };
        let s = serde_json::to_string(&ev).unwrap();
        let back: StreamEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(ev, back);
    }
}
